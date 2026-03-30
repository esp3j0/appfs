use agentfs_sdk::{
    connector_error_codes, ActionExecutionMode, AppAdapterV1, AppConnector, AppStructureSyncReason,
    AuthStatus, ConnectorContext, ConnectorError, ConnectorInfo, ConnectorTransport,
    DemoAppConnector, FetchLivePageRequest, FetchLivePageResponse, FetchSnapshotChunkRequest,
    FetchSnapshotChunkResponse, HealthStatus, SnapshotMeta, SubmitActionOutcome,
    SubmitActionRequest, SubmitActionResponse,
};
use anyhow::{Context, Result};
use serde_json::Value as JsonValue;
use std::collections::{HashMap, HashSet};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

use super::action_dispatcher;
use super::errors::{is_transient_connector_failure, ERR_INVALID_PAYLOAD};
use super::grpc_bridge_adapter::GrpcBridgeConnector;
use super::http_bridge_adapter::HttpBridgeConnector;
use super::shared::{
    action_template_matches, boundary_probe_from_bytes, classify_multiline_json_payload,
    collect_files_with_suffix, decode_jsonl_line, env_flag_enabled, extract_client_token,
    has_odd_unescaped_quotes, is_handle_format_valid, is_safe_action_rel_path,
    is_transient_action_sink_busy, parse_snapshot_on_timeout_policy, template_specificity,
    MultilineRecoveryOutcome,
};
use super::tree_sync::{ensure_app_structure_initialized, refresh_app_structure};
use super::{
    ActionCursorDoc, ActionCursorState, ActionSpec, AppfsAdapter, AppfsBridgeConfig, CursorState,
    ExecutionMode, InputMode, ManifestContract, ManifestDoc, ProcessOutcome, SnapshotSpec,
    StreamingJob, ACTION_CURSORS_FILENAME, DEFAULT_RETENTION_HINT_SEC,
    DEFAULT_SNAPSHOT_MAX_MATERIALIZED_BYTES, DEFAULT_SNAPSHOT_PREWARM_TIMEOUT_MS,
    DEFAULT_SNAPSHOT_READ_THROUGH_TIMEOUT_MS, SNAPSHOT_EXPAND_JOURNAL_FILENAME,
};

#[cfg_attr(not(test), allow(dead_code))]
struct LegacyAdapterConnector {
    app_id: String,
    transport: ConnectorTransport,
    adapter: Box<dyn AppAdapterV1>,
    live_page_state: HashMap<String, u32>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl LegacyAdapterConnector {
    fn new(app_id: String, transport: ConnectorTransport, adapter: Box<dyn AppAdapterV1>) -> Self {
        Self {
            app_id,
            transport,
            adapter,
            live_page_state: HashMap::new(),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn map_adapter_error_v1_to_connector_error(err: agentfs_sdk::AdapterErrorV1) -> ConnectorError {
    match err {
        agentfs_sdk::AdapterErrorV1::Rejected {
            code,
            message,
            retryable,
        } => ConnectorError {
            code,
            message,
            retryable,
            details: None,
        },
        agentfs_sdk::AdapterErrorV1::Internal { message } => ConnectorError {
            code: connector_error_codes::INTERNAL.to_string(),
            message,
            retryable: true,
            details: None,
        },
    }
}

impl AppConnector for LegacyAdapterConnector {
    fn connector_id(&self) -> std::result::Result<ConnectorInfo, ConnectorError> {
        Ok(ConnectorInfo {
            connector_id: format!("legacy-v1-bridge-{}", self.app_id),
            version: "v1-compat".to_string(),
            app_id: self.app_id.clone(),
            transport: self.transport,
            supports_snapshot: true,
            supports_live: true,
            supports_action: true,
            optional_features: vec!["legacy_v1_compat".to_string()],
        })
    }

    fn health(
        &mut self,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<HealthStatus, ConnectorError> {
        Ok(HealthStatus {
            healthy: true,
            auth_status: AuthStatus::Valid,
            message: Some("legacy v1 adapter compatibility connector".to_string()),
            checked_at: chrono::Utc::now().to_rfc3339(),
        })
    }

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: Duration,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<SnapshotMeta, ConnectorError> {
        let meta = self
            .adapter
            .prewarm_snapshot_meta(resource_path, timeout)
            .map_err(map_adapter_error_v1_to_connector_error)?;
        Ok(SnapshotMeta {
            size_bytes: meta.size_bytes,
            revision: meta.revision,
            last_modified: None,
            item_count: None,
        })
    }

    fn fetch_snapshot_chunk(
        &mut self,
        _request: FetchSnapshotChunkRequest,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<FetchSnapshotChunkResponse, ConnectorError> {
        Err(ConnectorError {
            code: connector_error_codes::NOT_SUPPORTED.to_string(),
            message: "legacy v1 adapter bridge does not support snapshot chunk fetch".to_string(),
            retryable: false,
            details: None,
        })
    }

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchLivePageResponse, ConnectorError> {
        let handle_id = request.handle_id.ok_or_else(|| ConnectorError {
            code: connector_error_codes::INVALID_ARGUMENT.to_string(),
            message: "handle_id is required for v1 live paging compatibility".to_string(),
            retryable: false,
            details: None,
        })?;

        let request_ctx = agentfs_sdk::RequestContextV1 {
            app_id: ctx.app_id.clone(),
            session_id: ctx.session_id.clone(),
            request_id: ctx.request_id.clone(),
            client_token: ctx.client_token.clone(),
        };

        let next_page_no = self
            .live_page_state
            .get(&handle_id)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        let optimistic_has_more = next_page_no < 3;

        let outcome = self
            .adapter
            .submit_control_action(
                "/_paging/fetch_next.act",
                agentfs_sdk::AdapterControlActionV1::PagingFetchNext {
                    handle_id: handle_id.clone(),
                    page_no: next_page_no as u64,
                    has_more: optimistic_has_more,
                },
                &request_ctx,
            )
            .map_err(map_adapter_error_v1_to_connector_error)?;

        let agentfs_sdk::AdapterControlOutcomeV1::Completed { content } = outcome;
        let items = content
            .get("items")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let page = content.get("page").cloned().unwrap_or_default();
        let handle_id = page
            .get("handle_id")
            .and_then(|v| v.as_str())
            .unwrap_or("legacy-handle")
            .to_string();
        let page_no = page
            .get("page_no")
            .and_then(|v| v.as_u64())
            .unwrap_or(next_page_no as u64) as u32;
        let has_more = page
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(optimistic_has_more);
        self.live_page_state.insert(handle_id.clone(), page_no);
        let next_cursor = if has_more {
            Some(format!("legacy-page-{}", page_no.saturating_add(1)))
        } else {
            None
        };

        Ok(FetchLivePageResponse {
            items,
            page: agentfs_sdk::LivePageInfo {
                handle_id,
                page_no,
                has_more,
                mode: agentfs_sdk::LiveMode::Live,
                expires_at: None,
                next_cursor,
                retry_after_ms: None,
            },
        })
    }

    fn submit_action(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let payload = serde_json::to_string(&request.payload).map_err(|err| ConnectorError {
            code: connector_error_codes::INVALID_PAYLOAD.to_string(),
            message: format!("failed to serialize action payload: {err}"),
            retryable: false,
            details: None,
        })?;
        let request_ctx = agentfs_sdk::RequestContextV1 {
            app_id: ctx.app_id.clone(),
            session_id: ctx.session_id.clone(),
            request_id: ctx.request_id.clone(),
            client_token: ctx.client_token.clone(),
        };
        let exec_mode = match request.execution_mode {
            ActionExecutionMode::Inline => agentfs_sdk::AdapterExecutionModeV1::Inline,
            ActionExecutionMode::Streaming => agentfs_sdk::AdapterExecutionModeV1::Streaming,
        };

        let outcome = self
            .adapter
            .submit_action(
                &request.path,
                &payload,
                agentfs_sdk::AdapterInputModeV1::Json,
                exec_mode,
                &request_ctx,
            )
            .map_err(map_adapter_error_v1_to_connector_error)?;

        let mapped = match outcome {
            agentfs_sdk::AdapterSubmitOutcomeV1::Completed { content } => {
                SubmitActionOutcome::Completed { content }
            }
            agentfs_sdk::AdapterSubmitOutcomeV1::Streaming { plan } => {
                SubmitActionOutcome::Streaming {
                    plan: agentfs_sdk::ActionStreamingPlan {
                        accepted_content: plan.accepted_content,
                        progress_content: plan.progress_content,
                        terminal_content: plan.terminal_content,
                    },
                }
            }
        };

        Ok(SubmitActionResponse {
            request_id: ctx.request_id.clone(),
            estimated_duration_ms: None,
            outcome: mapped,
        })
    }
}

impl AppfsAdapter {
    pub(super) fn new(
        root: PathBuf,
        app_id: String,
        session_id: String,
        bridge_config: AppfsBridgeConfig,
    ) -> Result<Self> {
        ensure_app_structure_initialized(&root, &app_id, &session_id, &bridge_config)?;
        let app_dir = root.join(&app_id);
        let manifest_path = app_dir.join("_meta").join("manifest.res.json");
        let events_path = app_dir.join("_stream").join("events.evt.jsonl");
        let cursor_path = app_dir.join("_stream").join("cursor.res.json");
        let replay_dir = app_dir.join("_stream").join("from-seq");
        let jobs_path = app_dir.join("_stream").join("inflight.jobs.res.json");
        let action_cursors_path = app_dir.join("_stream").join(ACTION_CURSORS_FILENAME);
        let snapshot_expand_journal_path = app_dir
            .join("_stream")
            .join(SNAPSHOT_EXPAND_JOURNAL_FILENAME);

        if !app_dir.exists() {
            anyhow::bail!("App directory not found: {}", app_dir.display());
        }
        if !manifest_path.exists() {
            anyhow::bail!("Missing manifest file: {}", manifest_path.display());
        }
        if !events_path.exists() {
            anyhow::bail!("Missing events stream file: {}", events_path.display());
        }
        if !cursor_path.exists() {
            anyhow::bail!("Missing cursor file: {}", cursor_path.display());
        }
        if !replay_dir.exists() {
            anyhow::bail!("Missing replay directory: {}", replay_dir.display());
        }

        let cursor = Self::load_cursor(&cursor_path)?;
        let next_seq = cursor.max_seq + 1;
        let manifest_contract = Self::load_manifest_contract(&manifest_path)?;
        if manifest_contract.requires_paging_controls {
            let has_fetch = manifest_contract
                .action_specs
                .iter()
                .any(|spec| spec.template == "_paging/fetch_next.act");
            let has_close = manifest_contract
                .action_specs
                .iter()
                .any(|spec| spec.template == "_paging/close.act");
            if !has_fetch || !has_close {
                anyhow::bail!(
                    "manifest declares live pageable resources but missing required paging actions: _paging/fetch_next.act and _paging/close.act"
                );
            }

            let fetch_path = app_dir.join("_paging").join("fetch_next.act");
            let close_path = app_dir.join("_paging").join("close.act");
            if !fetch_path.exists() || !close_path.exists() {
                anyhow::bail!(
                    "live pageable resources require paging control files to exist: {} and {}",
                    fetch_path.display(),
                    close_path.display()
                );
            }
        }
        let connector = build_app_connector(&app_id, &bridge_config)?;
        let connector_info = connector
            .connector_id()
            .map_err(|err| anyhow::anyhow!("connector_id failed: {}: {}", err.code, err.message))?;
        if connector_info.app_id != app_id {
            anyhow::bail!(
                "Connector app_id mismatch: connector={} runtime={}",
                connector_info.app_id,
                app_id
            );
        }

        let mut adapter = Self {
            app_id,
            session_id,
            app_dir,
            action_specs: manifest_contract.action_specs,
            snapshot_specs: manifest_contract.snapshot_specs,
            events_path,
            cursor_path,
            replay_dir,
            jobs_path: jobs_path.clone(),
            action_cursors_path: action_cursors_path.clone(),
            snapshot_expand_journal_path: snapshot_expand_journal_path.clone(),
            cursor,
            next_seq,
            action_cursors: Self::load_action_cursors(&action_cursors_path)?,
            handles: HashMap::new(),
            handle_aliases: HashMap::new(),
            snapshot_states: HashMap::new(),
            snapshot_recent_expands: HashMap::new(),
            snapshot_expand_journal: Self::load_snapshot_expand_journal(
                &snapshot_expand_journal_path,
            )?,
            streaming_jobs: Self::load_streaming_jobs(&jobs_path)?,
            actionline_v2_strict: env_flag_enabled("APPFS_V2_ACTIONLINE_STRICT"),
            connector,
        };
        adapter.initialize_snapshot_states();
        adapter.recover_snapshot_expand_journal()?;
        adapter.load_known_handles()?;
        adapter.startup_prewarm_snapshots();
        Ok(adapter)
    }

    pub(super) fn prepare_action_sinks(&mut self) -> Result<()> {
        let actions = self.collect_action_files()?;
        for action in actions {
            #[cfg(not(unix))]
            let _ = &action;
            #[cfg(unix)]
            {
                let perms = fs::Permissions::from_mode(0o666);
                fs::set_permissions(&action, perms).with_context(|| {
                    format!("Failed to set write permissions on {}", action.display())
                })?;
            }
        }
        Ok(())
    }

    pub(super) fn poll_once(&mut self) -> Result<()> {
        self.drain_streaming_jobs()?;

        let mut actions = self.collect_action_files()?;
        actions.sort();
        let mut cursor_dirty = false;

        for action_path in actions {
            cursor_dirty |= self.process_action_sink(&action_path)?;
        }

        if cursor_dirty {
            self.save_action_cursors()?;
        }

        Ok(())
    }

    fn process_action_sink(&mut self, action_path: &Path) -> Result<bool> {
        let rel = self.rel_path_for_log(action_path);
        if !is_safe_action_rel_path(&rel) {
            eprintln!("AppFS adapter rejected unsafe action path: {rel}");
            return Ok(false);
        }

        let Some(spec) = self.find_action_spec(&rel).cloned() else {
            eprintln!("AppFS adapter ignored undeclared action path: {rel}");
            return Ok(false);
        };

        let mut cursor = self.action_cursors.get(&rel).cloned().unwrap_or_default();
        let original_cursor = cursor.clone();
        let file_len = match fs::metadata(action_path) {
            Ok(meta) => meta.len(),
            Err(err) => {
                if is_transient_action_sink_busy(&err) {
                    // Writer currently owns an exclusive handle (common on Windows).
                    // Defer and retry next poll without consuming data.
                    return Ok(false);
                }
                eprintln!(
                    "AppFS adapter rejected action payload for {rel}: validation={ERR_INVALID_PAYLOAD} reason={}",
                    err
                );
                return Ok(false);
            }
        };

        if cursor.offset > file_len {
            eprintln!(
                "AppFS adapter HIGH: illegal action sink truncation detected for {rel}: offset={} file_len={file_len}; skipping rewritten content and waiting for future append",
                cursor.offset
            );
            cursor.offset = file_len;
            cursor.boundary_probe = None;
            cursor.pending_multiline_eof_len = None;
        } else if cursor.offset == file_len {
            return Ok(false);
        }

        let bytes = match fs::read(action_path) {
            Ok(bytes) => bytes,
            Err(err) => {
                if is_transient_action_sink_busy(&err) {
                    return Ok(false);
                }
                eprintln!(
                    "AppFS adapter rejected action payload for {rel}: validation={ERR_INVALID_PAYLOAD} reason={}",
                    err
                );
                return Ok(false);
            }
        };
        let file_len = bytes.len() as u64;

        if cursor.offset > file_len {
            eprintln!(
                "AppFS adapter HIGH: illegal action sink truncation detected for {rel}: offset={} file_len={file_len}; skipping rewritten content and waiting for future append",
                cursor.offset
            );
            cursor.offset = file_len;
            cursor.boundary_probe = None;
            cursor.pending_multiline_eof_len = None;
        } else if cursor.offset > 0 && cursor.boundary_probe.is_some() {
            let expected = cursor.boundary_probe.as_deref().unwrap_or_default();
            let current = boundary_probe_from_bytes(&bytes, cursor.offset);
            if current.as_deref() != Some(expected) {
                eprintln!(
                    "AppFS adapter HIGH: illegal action sink overwrite detected for {rel}: offset={} (probe mismatch); skipping rewritten content and waiting for future append",
                    cursor.offset
                );
                cursor.offset = file_len;
                cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                cursor.pending_multiline_eof_len = None;
            }
        }

        let mut position = cursor.offset as usize;
        while position < bytes.len() {
            while position < bytes.len() && bytes[position] == 0 {
                // PowerShell 5 `>>` (Out-File) may leave a trailing UTF-16 newline NUL byte
                // after our `\n` delimiter split. Consume it so the cursor can progress.
                position += 1;
                cursor.offset = position as u64;
                cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                cursor.pending_multiline_eof_len = None;
            }
            if position >= bytes.len() {
                break;
            }

            let Some(rel_idx) = bytes[position..].iter().position(|b| *b == b'\n') else {
                break;
            };
            let line_end = position + rel_idx + 1;
            let line_bytes = &bytes[position..line_end];
            let mut payload = match decode_jsonl_line(line_bytes, position == 0) {
                Ok(Some(line)) => line,
                Ok(None) => {
                    cursor.offset = line_end as u64;
                    cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                    cursor.pending_multiline_eof_len = None;
                    position = line_end;
                    continue;
                }
                Err(reason) => {
                    let len = line_bytes.len();
                    eprintln!(
                        "AppFS adapter rejected action payload for {rel}: validation={ERR_INVALID_PAYLOAD} len={len} reason={reason}"
                    );
                    cursor.offset = line_end as u64;
                    cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                    cursor.pending_multiline_eof_len = None;
                    position = line_end;
                    continue;
                }
            };
            let mut payload_line_end = line_end;
            let mut client_token_override = None;

            if matches!(spec.input_mode, InputMode::Json)
                && serde_json::from_str::<JsonValue>(&payload).is_err()
                && has_odd_unescaped_quotes(&payload)
            {
                match classify_multiline_json_payload(&bytes, &payload, line_end, &spec) {
                    Some(MultilineRecoveryOutcome::Recovered {
                        merged_payload,
                        merged_line_end,
                        consumed_lines,
                    }) => {
                        eprintln!(
                            "AppFS adapter normalized shell-expanded newline for {rel}: consumed_lines={consumed_lines}"
                        );
                        payload = merged_payload;
                        payload_line_end = merged_line_end;
                        cursor.pending_multiline_eof_len = None;
                    }
                    Some(MultilineRecoveryOutcome::PendingAtEof) => {
                        let pending_len = bytes.len() as u64;
                        if cursor.pending_multiline_eof_len == Some(pending_len) {
                            cursor.pending_multiline_eof_len = None;
                        } else {
                            eprintln!(
                                "AppFS adapter deferred incomplete multiline payload for {rel} at offset={}",
                                cursor.offset
                            );
                            cursor.pending_multiline_eof_len = Some(pending_len);
                            break;
                        }
                    }
                    None => {
                        cursor.pending_multiline_eof_len = None;
                    }
                }
            } else {
                cursor.pending_multiline_eof_len = None;
            }

            match action_dispatcher::normalize_actionline_v2_payload(
                &payload,
                self.actionline_v2_strict,
            ) {
                Ok(Some(parsed)) => {
                    client_token_override = Some(parsed.client_token);
                    payload = parsed.payload_json;
                }
                Ok(None) => {}
                Err(validation) => {
                    let len = payload.len();
                    eprintln!(
                        "AppFS adapter rejected action payload for {rel}: validation={} len={len} reason={}",
                        validation.code, validation.reason
                    );
                    cursor.offset = payload_line_end as u64;
                    cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                    cursor.pending_multiline_eof_len = None;
                    position = payload_line_end;
                    continue;
                }
            }

            match self.process_action(&rel, &spec, &payload, client_token_override)? {
                ProcessOutcome::Consumed => {
                    cursor.offset = payload_line_end as u64;
                    cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                    cursor.pending_multiline_eof_len = None;
                    position = payload_line_end;
                }
                ProcessOutcome::RetryPending => {
                    eprintln!(
                        "AppFS adapter deferred action retry for {rel} at offset={}",
                        cursor.offset
                    );
                    break;
                }
            }
        }

        let changed = cursor != original_cursor;
        if changed {
            self.action_cursors.insert(rel, cursor);
        }
        Ok(changed)
    }

    fn rel_path_for_log(&self, action_path: &Path) -> String {
        action_path
            .strip_prefix(&self.app_dir)
            .unwrap_or(action_path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    fn process_action(
        &mut self,
        rel: &str,
        spec: &ActionSpec,
        payload: &str,
        client_token_override: Option<String>,
    ) -> Result<ProcessOutcome> {
        if let Err(code) = action_dispatcher::validate_submit_payload(spec, payload) {
            eprintln!(
                "AppFS adapter rejected action payload for {rel}: validation={code} len={}",
                payload.len()
            );
            return Ok(ProcessOutcome::Consumed);
        }

        let normalized_path = format!("/{}", rel);
        let request_id = Self::new_request_id();
        let client_token = client_token_override.or_else(|| extract_client_token(payload));

        match action_dispatcher::route_action(&normalized_path, payload) {
            Ok(action_dispatcher::DispatchRoute::PagingFetchNext(request)) => {
                if !is_handle_format_valid(&request.handle_id) {
                    eprintln!(
                        "AppFS adapter rejected invalid handle format at submit-time: {}",
                        normalized_path
                    );
                    return Ok(ProcessOutcome::Consumed);
                }
                return self.handle_fetch_next(
                    &normalized_path,
                    &request_id,
                    &request.handle_id,
                    request.session_id.as_deref(),
                    client_token,
                );
            }
            Ok(action_dispatcher::DispatchRoute::PagingClose(request)) => {
                if !is_handle_format_valid(&request.handle_id) {
                    eprintln!(
                        "AppFS adapter rejected invalid close handle format at submit-time: {}",
                        normalized_path
                    );
                    return Ok(ProcessOutcome::Consumed);
                }
                return self.handle_close_handle(
                    &normalized_path,
                    &request_id,
                    &request.handle_id,
                    request.session_id.as_deref(),
                    client_token,
                );
            }
            Ok(action_dispatcher::DispatchRoute::SnapshotRefresh(request)) => {
                return self.handle_snapshot_refresh(
                    &normalized_path,
                    &request_id,
                    &request.resource_path,
                    client_token,
                );
            }
            Ok(action_dispatcher::DispatchRoute::EnterScope(request)) => {
                return self.handle_enter_scope(
                    &normalized_path,
                    &request_id,
                    &request.target_scope,
                    client_token,
                );
            }
            Ok(action_dispatcher::DispatchRoute::StructureRefresh(request)) => {
                return self.handle_refresh_structure(
                    &normalized_path,
                    &request_id,
                    request.target_scope.as_deref(),
                    client_token,
                );
            }
            Ok(action_dispatcher::DispatchRoute::BusinessSubmit) => {}
            Err(action_dispatcher::DispatchRouteParseError::PagingFetchNext) => {
                eprintln!(
                    "AppFS adapter rejected malformed paging handle at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
            Err(action_dispatcher::DispatchRouteParseError::PagingClose) => {
                eprintln!(
                    "AppFS adapter rejected malformed paging close handle at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
            Err(action_dispatcher::DispatchRouteParseError::SnapshotRefresh) => {
                eprintln!(
                    "AppFS adapter rejected malformed snapshot refresh payload at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
            Err(action_dispatcher::DispatchRouteParseError::EnterScope) => {
                eprintln!(
                    "AppFS adapter rejected malformed enter_scope payload at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
            Err(action_dispatcher::DispatchRouteParseError::StructureRefresh) => {
                eprintln!(
                    "AppFS adapter rejected malformed structure refresh payload at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
        }

        let request_ctx = ConnectorContext {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.clone(),
            client_token: client_token.clone(),
            trace_id: None,
        };
        let execution_mode = match spec.execution_mode {
            ExecutionMode::Inline => ActionExecutionMode::Inline,
            ExecutionMode::Streaming => ActionExecutionMode::Streaming,
        };
        let payload_json: JsonValue =
            serde_json::from_str(payload).context("validated JSON payload must parse")?;

        match self.connector.submit_action(
            SubmitActionRequest {
                path: normalized_path.clone(),
                payload: payload_json,
                execution_mode,
            },
            &request_ctx,
        ) {
            Ok(SubmitActionResponse {
                outcome: SubmitActionOutcome::Completed { content },
                ..
            }) => {
                self.emit_event(
                    &normalized_path,
                    &request_id,
                    "action.completed",
                    Some(content),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Ok(SubmitActionResponse {
                outcome: SubmitActionOutcome::Streaming { plan },
                ..
            }) => {
                self.enqueue_streaming_job(
                    &normalized_path,
                    &request_id,
                    client_token,
                    agentfs_sdk::AdapterStreamingPlanV1 {
                        accepted_content: plan.accepted_content,
                        progress_content: plan.progress_content,
                        terminal_content: plan.terminal_content,
                    },
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(ConnectorError {
                code,
                message,
                retryable,
                ..
            }) => {
                if is_transient_connector_failure(&code, retryable) {
                    eprintln!(
                        "AppFS adapter transient connector failure for {normalized_path}: code={code} message={message}; will retry without advancing cursor"
                    );
                    return Ok(ProcessOutcome::RetryPending);
                }
                self.emit_failed_with_retryable(
                    &normalized_path,
                    &request_id,
                    &code,
                    &message,
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
        }
    }

    fn find_action_spec(&self, rel_path: &str) -> Option<&ActionSpec> {
        self.action_specs
            .iter()
            .filter(|spec| action_template_matches(&spec.template, rel_path))
            .max_by_key(|spec| template_specificity(&spec.template))
    }

    pub(super) fn find_snapshot_spec(&self, rel_path: &str) -> Option<&SnapshotSpec> {
        self.snapshot_specs
            .iter()
            .filter(|spec| action_template_matches(&spec.template, rel_path))
            .max_by_key(|spec| template_specificity(&spec.template))
    }

    pub(super) fn load_manifest_contract(manifest_path: &Path) -> Result<ManifestContract> {
        let manifest_json = fs::read_to_string(manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        parse_manifest_contract_json(&manifest_json, &manifest_path.display().to_string())
    }
}

pub(super) fn build_app_connector(
    app_id: &str,
    bridge_config: &AppfsBridgeConfig,
) -> Result<Box<dyn AppConnector>> {
    let normalized_http_endpoint = bridge_config
        .adapter_http_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let normalized_grpc_endpoint = bridge_config
        .adapter_grpc_endpoint
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    if normalized_http_endpoint.is_some() && normalized_grpc_endpoint.is_some() {
        anyhow::bail!(
            "Only one bridge endpoint can be configured at a time: --adapter-http-endpoint or --adapter-grpc-endpoint"
        );
    }

    let connector: Box<dyn AppConnector> = if let Some(endpoint) = normalized_grpc_endpoint {
        eprintln!("AppFS adapter using gRPC bridge endpoint: {endpoint}");
        Box::new(
            GrpcBridgeConnector::new(
                app_id.to_string(),
                endpoint,
                Duration::from_millis(bridge_config.adapter_grpc_timeout_ms.max(1)),
                bridge_config.runtime_options,
            )
            .map_err(|err| {
                anyhow::anyhow!(
                    "failed to initialize gRPC bridge connector: {}: {}",
                    err.code,
                    err.message
                )
            })?,
        )
    } else if let Some(endpoint) = normalized_http_endpoint {
        eprintln!("AppFS adapter using HTTP bridge endpoint: {endpoint}");
        Box::new(HttpBridgeConnector::new(
            app_id.to_string(),
            endpoint,
            Duration::from_millis(bridge_config.adapter_http_timeout_ms.max(1)),
            bridge_config.runtime_options,
        ))
    } else {
        Box::new(DemoAppConnector::new(app_id.to_string()))
    };

    let connector_info = connector
        .connector_id()
        .map_err(|err| anyhow::anyhow!("connector_id failed: {}: {}", err.code, err.message))?;
    if connector_info.app_id != app_id {
        anyhow::bail!(
            "Connector app_id mismatch: connector={} runtime={}",
            connector_info.app_id,
            app_id
        );
    }
    Ok(connector)
}

pub(super) fn parse_manifest_contract_json(
    manifest_json: &str,
    source: &str,
) -> Result<ManifestContract> {
    let manifest: ManifestDoc =
        serde_json::from_str(manifest_json).with_context(|| format!("Failed to parse {source}"))?;

    let mut action_specs = Vec::new();
    let mut action_templates = HashSet::new();
    let mut snapshot_specs = Vec::new();
    let mut requires_paging_controls = false;

    for (template, node) in manifest.nodes {
        match node.kind.as_str() {
            "action" => {
                if !template.ends_with(".act") {
                    continue;
                }

                let input_mode = match node.input_mode.as_deref() {
                    Some("json") => InputMode::Json,
                    Some(other) => {
                        anyhow::bail!(
                            "AppFS adapter requires input_mode=json for all action sinks, template={template}, found input_mode={other}"
                        );
                    }
                    None => {
                        anyhow::bail!(
                            "AppFS adapter requires explicit input_mode=json for action template={template}"
                        );
                    }
                };

                let execution_mode = match node.execution_mode.as_deref() {
                    Some("streaming") => ExecutionMode::Streaming,
                    Some("inline") | None => ExecutionMode::Inline,
                    Some(other) => {
                        eprintln!(
                            "AppFS adapter unknown execution_mode='{other}' for action template={template}, defaulting to inline"
                        );
                        ExecutionMode::Inline
                    }
                };

                let normalized = template.trim_start_matches('/').to_string();
                action_templates.insert(normalized.clone());
                action_specs.push(ActionSpec {
                    template: normalized,
                    input_mode,
                    execution_mode,
                    max_payload_bytes: node.max_payload_bytes,
                });
            }
            "resource" => {
                let output_mode = node.output_mode.as_deref().unwrap_or("json");
                let paging_enabled = node
                    .paging
                    .as_ref()
                    .and_then(|paging| paging.enabled)
                    .unwrap_or(false);
                let paging_mode = node
                    .paging
                    .as_ref()
                    .and_then(|paging| paging.mode.as_deref())
                    .unwrap_or("snapshot");

                match output_mode {
                    "jsonl" => {
                        if !template.ends_with(".res.jsonl") {
                            anyhow::bail!(
                                "snapshot jsonl resource template must end with .res.jsonl: {template}"
                            );
                        }
                        if paging_enabled
                            || node
                                .paging
                                .as_ref()
                                .and_then(|paging| paging.mode.as_deref())
                                .is_some()
                        {
                            anyhow::bail!(
                                "snapshot jsonl resource must not declare paging metadata: {template}"
                            );
                        }
                        let max_materialized_bytes = node
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.max_materialized_bytes)
                            .unwrap_or(DEFAULT_SNAPSHOT_MAX_MATERIALIZED_BYTES);
                        if max_materialized_bytes == 0 {
                            anyhow::bail!(
                                "snapshot.max_materialized_bytes must be > 0 for resource template={template}"
                            );
                        }
                        let prewarm = node
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.prewarm)
                            .unwrap_or(true);
                        let prewarm_timeout_ms = node
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.prewarm_timeout_ms)
                            .unwrap_or(DEFAULT_SNAPSHOT_PREWARM_TIMEOUT_MS);
                        if prewarm_timeout_ms == 0 {
                            anyhow::bail!(
                                "snapshot.prewarm_timeout_ms must be > 0 for resource template={template}"
                            );
                        }
                        let read_through_timeout_ms = node
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.read_through_timeout_ms)
                            .unwrap_or(DEFAULT_SNAPSHOT_READ_THROUGH_TIMEOUT_MS);
                        if read_through_timeout_ms == 0 {
                            anyhow::bail!(
                                "snapshot.read_through_timeout_ms must be > 0 for resource template={template}"
                            );
                        }
                        let on_timeout = parse_snapshot_on_timeout_policy(
                            node.snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.on_timeout.as_deref()),
                        );
                        snapshot_specs.push(SnapshotSpec {
                            template: template.trim_start_matches('/').to_string(),
                            max_materialized_bytes,
                            prewarm,
                            prewarm_timeout_ms,
                            read_through_timeout_ms,
                            on_timeout,
                        });
                    }
                    "json" => {
                        if paging_enabled {
                            if paging_mode != "live" {
                                anyhow::bail!(
                                    "pageable resource must declare paging.mode=live in v0.1 for template={template}"
                                );
                            }
                            requires_paging_controls = true;
                        }
                    }
                    other => {
                        anyhow::bail!(
                            "unsupported output_mode='{other}' for resource template={template}; expected json or jsonl"
                        );
                    }
                }
            }
            _ => {}
        }
    }

    if requires_paging_controls {
        for required in ["_paging/fetch_next.act", "_paging/close.act"] {
            if !action_templates.contains(required) {
                anyhow::bail!(
                    "manifest declares live pageable resources but missing required action template={required}"
                );
            }
        }
    }

    if action_specs.is_empty() {
        eprintln!("AppFS adapter warning: no action definitions found in {source}");
    }

    Ok(ManifestContract {
        action_specs,
        snapshot_specs,
        requires_paging_controls,
    })
}

impl AppfsAdapter {
    pub(super) fn save_cursor(&self) -> Result<()> {
        let tmp_path = self.cursor_path.with_extension("res.json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.cursor)?;
        fs::write(&tmp_path, bytes)
            .with_context(|| format!("Failed to write cursor temp file {}", tmp_path.display()))?;
        if self.cursor_path.exists() {
            fs::remove_file(&self.cursor_path).with_context(|| {
                format!(
                    "Failed to remove old cursor file {}",
                    self.cursor_path.display()
                )
            })?;
        }
        fs::rename(&tmp_path, &self.cursor_path).with_context(|| {
            format!(
                "Failed to move cursor temp file {} to {}",
                tmp_path.display(),
                self.cursor_path.display()
            )
        })?;
        Ok(())
    }

    fn load_cursor(path: &Path) -> Result<CursorState> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut cursor: CursorState = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        if cursor.retention_hint_sec <= 0 {
            cursor.retention_hint_sec = DEFAULT_RETENTION_HINT_SEC;
        }
        Ok(cursor)
    }

    fn load_streaming_jobs(path: &Path) -> Result<Vec<StreamingJob>> {
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let jobs: Vec<StreamingJob> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(jobs)
    }

    pub(super) fn save_streaming_jobs(&self) -> Result<()> {
        let tmp_path = self.jobs_path.with_extension("res.json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.streaming_jobs)?;
        fs::write(&tmp_path, bytes)
            .with_context(|| format!("Failed to write jobs temp file {}", tmp_path.display()))?;
        if self.jobs_path.exists() {
            fs::remove_file(&self.jobs_path).with_context(|| {
                format!(
                    "Failed to remove old jobs file {}",
                    self.jobs_path.display()
                )
            })?;
        }
        fs::rename(&tmp_path, &self.jobs_path).with_context(|| {
            format!(
                "Failed to move jobs temp file {} to {}",
                tmp_path.display(),
                self.jobs_path.display()
            )
        })?;
        Ok(())
    }

    fn load_action_cursors(path: &Path) -> Result<HashMap<String, ActionCursorState>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }

        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let doc: ActionCursorDoc = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(doc.actions)
    }

    fn save_action_cursors(&self) -> Result<()> {
        let tmp_path = self.action_cursors_path.with_extension("res.json.tmp");
        let doc = ActionCursorDoc {
            actions: self.action_cursors.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&doc)?;
        fs::write(&tmp_path, bytes).with_context(|| {
            format!(
                "Failed to write action cursor temp file {}",
                tmp_path.display()
            )
        })?;

        if self.action_cursors_path.exists() {
            fs::remove_file(&self.action_cursors_path).with_context(|| {
                format!(
                    "Failed to remove old action cursor file {}",
                    self.action_cursors_path.display()
                )
            })?;
        }

        fs::rename(&tmp_path, &self.action_cursors_path).with_context(|| {
            format!(
                "Failed to move action cursor temp file {} to {}",
                tmp_path.display(),
                self.action_cursors_path.display()
            )
        })?;
        Ok(())
    }

    fn collect_action_files(&self) -> Result<Vec<PathBuf>> {
        let mut out = Vec::new();
        collect_files_with_suffix(&self.app_dir, ".act", &mut out)?;
        Ok(out)
    }

    pub(super) fn reload_manifest_contract(&mut self) -> Result<()> {
        let manifest_path = self.app_dir.join("_meta").join("manifest.res.json");
        let manifest_contract = Self::load_manifest_contract(&manifest_path)?;
        if manifest_contract.requires_paging_controls {
            let fetch_path = self.app_dir.join("_paging").join("fetch_next.act");
            let close_path = self.app_dir.join("_paging").join("close.act");
            if !fetch_path.exists() || !close_path.exists() {
                anyhow::bail!(
                    "live pageable resources require paging control files to exist: {} and {}",
                    fetch_path.display(),
                    close_path.display()
                );
            }
        }

        self.action_specs = manifest_contract.action_specs;
        self.snapshot_specs = manifest_contract.snapshot_specs;
        self.snapshot_states.clear();
        self.initialize_snapshot_states();
        self.prepare_action_sinks()?;
        Ok(())
    }

    pub(super) fn handle_enter_scope(
        &mut self,
        action_path: &str,
        request_id: &str,
        target_scope: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let root = self
            .app_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("app directory has no parent root"))?;
        match refresh_app_structure(
            root,
            &self.app_id,
            &self.session_id,
            &mut *self.connector,
            AppStructureSyncReason::EnterScope,
            Some(target_scope.to_string()),
            Some(action_path.to_string()),
        ) {
            Ok(outcome) => {
                self.reload_manifest_contract()?;
                self.emit_event(
                    action_path,
                    request_id,
                    "action.completed",
                    Some(serde_json::json!({
                        "refreshed": outcome.changed,
                        "active_scope": outcome.active_scope,
                        "revision": outcome.revision,
                    })),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(err) => {
                let (code, retryable) = match err.downcast_ref::<ConnectorError>() {
                    Some(connector_err)
                        if connector_err.code == connector_error_codes::NOT_SUPPORTED =>
                    {
                        (connector_error_codes::NOT_SUPPORTED, false)
                    }
                    _ => ("STRUCTURE_SYNC_FAILED", true),
                };
                self.emit_failed_with_retryable(
                    action_path,
                    request_id,
                    code,
                    &format!("structure enter_scope failed: {err}"),
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
        }
    }

    pub(super) fn handle_refresh_structure(
        &mut self,
        action_path: &str,
        request_id: &str,
        target_scope: Option<&str>,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let root = self
            .app_dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("app directory has no parent root"))?;
        match refresh_app_structure(
            root,
            &self.app_id,
            &self.session_id,
            &mut *self.connector,
            AppStructureSyncReason::Refresh,
            target_scope.map(ToOwned::to_owned),
            Some(action_path.to_string()),
        ) {
            Ok(outcome) => {
                self.reload_manifest_contract()?;
                self.emit_event(
                    action_path,
                    request_id,
                    "action.completed",
                    Some(serde_json::json!({
                        "refreshed": outcome.changed,
                        "active_scope": outcome.active_scope,
                        "revision": outcome.revision,
                    })),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(err) => {
                let (code, retryable) = match err.downcast_ref::<ConnectorError>() {
                    Some(connector_err)
                        if connector_err.code == connector_error_codes::NOT_SUPPORTED =>
                    {
                        (connector_error_codes::NOT_SUPPORTED, false)
                    }
                    _ => ("STRUCTURE_SYNC_FAILED", true),
                };
                self.emit_failed_with_retryable(
                    action_path,
                    request_id,
                    code,
                    &format!("structure refresh failed: {err}"),
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
        }
    }

    fn new_request_id() -> String {
        let uuid = Uuid::new_v4().simple().to_string();
        format!("req-{}", &uuid[..8])
    }
}

#[cfg(test)]
mod tests {
    use super::{map_adapter_error_v1_to_connector_error, LegacyAdapterConnector};
    use super::{AppfsAdapter, AppfsBridgeConfig};
    use crate::cmd::appfs::bridge_resilience::BridgeRuntimeOptions;
    use crate::cmd::appfs::{ACTION_CURSORS_FILENAME, SNAPSHOT_EXPAND_JOURNAL_FILENAME};
    use agentfs_sdk::{
        AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
        AdapterInputModeV1, AdapterSubmitOutcomeV1, AppAdapterV1, AppConnector, ConnectorContext,
        ConnectorTransport, FetchLivePageRequest, RequestContextV1,
    };
    use serde_json::{json, Value as JsonValue};
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::mpsc;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::thread;
    use tempfile::TempDir;
    use tonic::{Request, Response, Status};

    struct PagingCompatAdapter {
        seen_pages: Vec<u64>,
    }

    impl AppAdapterV1 for PagingCompatAdapter {
        fn app_id(&self) -> &str {
            "aiim"
        }

        fn submit_action(
            &mut self,
            _path: &str,
            _payload: &str,
            _input_mode: AdapterInputModeV1,
            _execution_mode: AdapterExecutionModeV1,
            _ctx: &RequestContextV1,
        ) -> std::result::Result<AdapterSubmitOutcomeV1, AdapterErrorV1> {
            Ok(AdapterSubmitOutcomeV1::Completed {
                content: json!({"ok": true}),
            })
        }

        fn submit_control_action(
            &mut self,
            _path: &str,
            action: AdapterControlActionV1,
            _ctx: &RequestContextV1,
        ) -> std::result::Result<AdapterControlOutcomeV1, AdapterErrorV1> {
            let AdapterControlActionV1::PagingFetchNext {
                handle_id,
                page_no,
                has_more,
            } = action
            else {
                unreachable!("test only exercises fetch_next");
            };
            self.seen_pages.push(page_no);
            Ok(AdapterControlOutcomeV1::Completed {
                content: json!({
                    "items": [{"id": format!("item-{page_no}")}],
                    "page": {
                        "handle_id": handle_id,
                        "page_no": page_no,
                        "has_more": has_more,
                        "mode": "live"
                    }
                }),
            })
        }
    }

    fn ctx() -> ConnectorContext {
        ConnectorContext {
            app_id: "aiim".to_string(),
            session_id: "sess-1".to_string(),
            request_id: "req-1".to_string(),
            client_token: None,
            trace_id: None,
        }
    }

    #[test]
    fn legacy_connector_live_paging_advances_without_cursor_parse() {
        let adapter: Box<dyn AppAdapterV1> = Box::new(PagingCompatAdapter {
            seen_pages: Vec::new(),
        });
        let mut connector = LegacyAdapterConnector::new(
            "aiim".to_string(),
            ConnectorTransport::HttpBridge,
            adapter,
        );

        let first = connector
            .fetch_live_page(
                FetchLivePageRequest {
                    resource_path: "/chats/chat-001/messages.res.json".to_string(),
                    handle_id: Some("ph_abc".to_string()),
                    cursor: None,
                    page_size: 50,
                },
                &ctx(),
            )
            .expect("first page should succeed");
        assert_eq!(first.page.page_no, 1);
        assert_eq!(first.page.next_cursor.as_deref(), Some("legacy-page-2"));

        let second = connector
            .fetch_live_page(
                FetchLivePageRequest {
                    resource_path: "/chats/chat-001/messages.res.json".to_string(),
                    handle_id: Some("ph_abc".to_string()),
                    cursor: None,
                    page_size: 50,
                },
                &ctx(),
            )
            .expect("second page should succeed");
        assert_eq!(second.page.page_no, 2);
        assert_eq!(second.page.next_cursor.as_deref(), Some("legacy-page-3"));
    }

    #[test]
    fn adapter_internal_maps_to_retryable_connector_internal() {
        let err = map_adapter_error_v1_to_connector_error(AdapterErrorV1::Internal {
            message: "transport disconnected".to_string(),
        });
        assert_eq!(err.code, "INTERNAL");
        assert!(err.retryable);
    }

    fn copy_dir_recursive(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).expect("create dst dir");
        for entry in fs::read_dir(src).expect("read src dir") {
            let entry = entry.expect("dir entry");
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());
            if entry.file_type().expect("file type").is_dir() {
                copy_dir_recursive(&src_path, &dst_path);
            } else {
                fs::copy(&src_path, &dst_path).expect("copy fixture file");
            }
        }
    }

    fn bridge_config() -> AppfsBridgeConfig {
        AppfsBridgeConfig {
            adapter_http_endpoint: None,
            adapter_http_timeout_ms: 5_000,
            adapter_grpc_endpoint: None,
            adapter_grpc_timeout_ms: 5_000,
            runtime_options: BridgeRuntimeOptions::from_cli(2, 100, 1_000, 5, 3_000),
        }
    }

    fn http_bridge_config(endpoint: String) -> AppfsBridgeConfig {
        AppfsBridgeConfig {
            adapter_http_endpoint: Some(endpoint),
            adapter_http_timeout_ms: 5_000,
            adapter_grpc_endpoint: None,
            adapter_grpc_timeout_ms: 5_000,
            runtime_options: BridgeRuntimeOptions::from_cli(2, 100, 1_000, 5, 3_000),
        }
    }

    fn grpc_bridge_config(endpoint: String) -> AppfsBridgeConfig {
        AppfsBridgeConfig {
            adapter_http_endpoint: None,
            adapter_http_timeout_ms: 5_000,
            adapter_grpc_endpoint: Some(endpoint),
            adapter_grpc_timeout_ms: 5_000,
            runtime_options: BridgeRuntimeOptions::from_cli(2, 100, 1_000, 5, 3_000),
        }
    }

    fn fixture_adapter() -> (TempDir, AppfsAdapter) {
        let temp = TempDir::new().expect("tempdir");
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/appfs/aiim");
        let app_dir = temp.path().join("aiim");
        copy_dir_recursive(&fixture, &app_dir);

        fs::write(app_dir.join("_stream").join("events.evt.jsonl"), "").expect("reset events");
        fs::write(
            app_dir.join("_stream").join("cursor.res.json"),
            "{\n  \"min_seq\": 0,\n  \"max_seq\": 0,\n  \"retention_hint_sec\": 86400\n}\n",
        )
        .expect("reset cursor");
        let _ = fs::remove_file(app_dir.join("_stream").join(ACTION_CURSORS_FILENAME));
        let _ = fs::remove_file(app_dir.join("_stream").join("inflight.jobs.res.json"));
        let _ = fs::remove_file(
            app_dir
                .join("_stream")
                .join(SNAPSHOT_EXPAND_JOURNAL_FILENAME),
        );
        let replay_dir = app_dir.join("_stream").join("from-seq");
        let _ = fs::remove_dir_all(&replay_dir);
        fs::create_dir_all(&replay_dir).expect("reset replay dir");
        fs::write(
            app_dir
                .join("contacts")
                .join("zhangsan")
                .join("send_message.act"),
            "",
        )
        .expect("reset action");

        let adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            bridge_config(),
        )
        .expect("fixture adapter");

        (temp, adapter)
    }

    fn structured_adapter() -> (TempDir, AppfsAdapter) {
        let temp = TempDir::new().expect("tempdir");
        let adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            bridge_config(),
        )
        .expect("structured adapter");

        (temp, adapter)
    }

    struct TestHttpStructureBridge {
        endpoint: String,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl TestHttpStructureBridge {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind test bridge");
            let addr = listener.local_addr().expect("listener addr");
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = Arc::clone(&stop);

            let thread = thread::spawn(move || {
                while let Ok((mut stream, _)) = listener.accept() {
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    handle_test_http_connection(&mut stream);
                }
            });

            Self {
                endpoint: format!("http://{}", addr),
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestHttpStructureBridge {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            let _ = TcpStream::connect(self.endpoint.trim_start_matches("http://"));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    #[derive(Default)]
    struct TestGrpcConnectorV2Service;

    #[tonic::async_trait]
    impl super::super::grpc_bridge_adapter::proto_v2::appfs_connector_v2_server::AppfsConnectorV2
        for TestGrpcConnectorV2Service
    {
        async fn get_connector_info(
            &self,
            _request: Request<super::super::grpc_bridge_adapter::proto_v2::GetConnectorInfoRequest>,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v2::GetConnectorInfoResponse>,
            Status,
        > {
            Ok(Response::new(
                super::super::grpc_bridge_adapter::proto_v2::GetConnectorInfoResponse {
                    result: Some(
                        super::super::grpc_bridge_adapter::proto_v2::get_connector_info_response::Result::Info(
                            super::super::grpc_bridge_adapter::proto_v2::ConnectorInfoV2 {
                                connector_id: "test-grpc-v2".to_string(),
                                version: "0.4.0-test".to_string(),
                                app_id: "aiim".to_string(),
                                transport: super::super::grpc_bridge_adapter::proto_v2::ConnectorTransportV2::GrpcBridge as i32,
                                supports_snapshot: true,
                                supports_live: true,
                                supports_action: true,
                                optional_features: vec!["structure_sync_v3".to_string()],
                            },
                        ),
                    ),
                },
            ))
        }

        async fn health(
            &self,
            _request: Request<super::super::grpc_bridge_adapter::proto_v2::HealthRequest>,
        ) -> Result<Response<super::super::grpc_bridge_adapter::proto_v2::HealthResponse>, Status>
        {
            Ok(Response::new(
                super::super::grpc_bridge_adapter::proto_v2::HealthResponse {
                    result: Some(
                        super::super::grpc_bridge_adapter::proto_v2::health_response::Result::Status(
                            super::super::grpc_bridge_adapter::proto_v2::HealthStatusV2 {
                                healthy: true,
                                auth_status: super::super::grpc_bridge_adapter::proto_v2::AuthStatusV2::Valid as i32,
                                message: Some("ok".to_string()),
                                checked_at: "2026-03-24T00:00:00Z".to_string(),
                            },
                        ),
                    ),
                },
            ))
        }

        async fn prewarm_snapshot_meta(
            &self,
            _request: Request<
                super::super::grpc_bridge_adapter::proto_v2::PrewarmSnapshotMetaRequest,
            >,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v2::PrewarmSnapshotMetaResponse>,
            Status,
        > {
            Err(Status::unimplemented("not used in core structure tests"))
        }

        async fn fetch_snapshot_chunk(
            &self,
            _request: Request<
                super::super::grpc_bridge_adapter::proto_v2::FetchSnapshotChunkRequest,
            >,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v2::FetchSnapshotChunkResponse>,
            Status,
        > {
            Err(Status::unimplemented("not used in core structure tests"))
        }

        async fn fetch_live_page(
            &self,
            _request: Request<super::super::grpc_bridge_adapter::proto_v2::FetchLivePageRequest>,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v2::FetchLivePageResponse>,
            Status,
        > {
            Err(Status::unimplemented("not used in core structure tests"))
        }

        async fn submit_action(
            &self,
            _request: Request<super::super::grpc_bridge_adapter::proto_v2::SubmitActionRequest>,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v2::SubmitActionResponse>,
            Status,
        > {
            Err(Status::unimplemented("not used in core structure tests"))
        }
    }

    #[derive(Default)]
    struct TestGrpcConnectorV3Service;

    #[tonic::async_trait]
    impl super::super::grpc_bridge_adapter::proto_v3::appfs_connector_v3_server::AppfsConnectorV3
        for TestGrpcConnectorV3Service
    {
        async fn get_app_structure(
            &self,
            request: Request<super::super::grpc_bridge_adapter::proto_v3::GetAppStructureRequest>,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v3::GetAppStructureResponse>,
            Status,
        > {
            let req = request
                .into_inner()
                .request
                .ok_or_else(|| Status::invalid_argument("missing request"))?;
            let result = if req.known_revision.as_deref() == Some("demo-structure-chat-001") {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncResultV3 {
                    kind: Some(
                        super::super::grpc_bridge_adapter::proto_v3::app_structure_sync_result_v3::Kind::Unchanged(
                            super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncUnchangedV3 {
                                app_id: req.app_id,
                                revision: "demo-structure-chat-001".to_string(),
                                active_scope: Some("chat-001".to_string()),
                            },
                        ),
                    ),
                }
            } else {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncResultV3 {
                    kind: Some(
                        super::super::grpc_bridge_adapter::proto_v3::app_structure_sync_result_v3::Kind::Snapshot(
                            super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncSnapshotV3 {
                                snapshot: Some(grpc_structure_snapshot("chat-001")),
                            },
                        ),
                    ),
                }
            };
            Ok(Response::new(
                super::super::grpc_bridge_adapter::proto_v3::GetAppStructureResponse {
                    result: Some(
                        super::super::grpc_bridge_adapter::proto_v3::get_app_structure_response::Result::Response(
                            result,
                        ),
                    ),
                },
            ))
        }

        async fn refresh_app_structure(
            &self,
            request: Request<
                super::super::grpc_bridge_adapter::proto_v3::RefreshAppStructureRequest,
            >,
        ) -> Result<
            Response<super::super::grpc_bridge_adapter::proto_v3::RefreshAppStructureResponse>,
            Status,
        > {
            let req = request
                .into_inner()
                .request
                .ok_or_else(|| Status::invalid_argument("missing request"))?;
            if req.reason
                == super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncReasonV3::EnterScope
                    as i32
                && req.target_scope.is_none()
            {
                return Ok(Response::new(
                    super::super::grpc_bridge_adapter::proto_v3::RefreshAppStructureResponse {
                        result: Some(
                            super::super::grpc_bridge_adapter::proto_v3::refresh_app_structure_response::Result::Error(
                                super::super::grpc_bridge_adapter::proto_v3::ConnectorErrorV3 {
                                    code: "STRUCTURE_SCOPE_INVALID".to_string(),
                                    message: "target_scope is required for enter_scope refresh"
                                        .to_string(),
                                    retryable: false,
                                    details: None,
                                },
                            ),
                        ),
                    },
                ));
            }

            let target_scope = req.target_scope.unwrap_or_else(|| "chat-001".to_string());
            let snapshot = grpc_structure_snapshot(&target_scope);
            let result = if req.known_revision.as_deref() == Some(snapshot.revision.as_str()) {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncResultV3 {
                    kind: Some(
                        super::super::grpc_bridge_adapter::proto_v3::app_structure_sync_result_v3::Kind::Unchanged(
                            super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncUnchangedV3 {
                                app_id: req.app_id,
                                revision: snapshot.revision,
                                active_scope: snapshot.active_scope,
                            },
                        ),
                    ),
                }
            } else {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncResultV3 {
                    kind: Some(
                        super::super::grpc_bridge_adapter::proto_v3::app_structure_sync_result_v3::Kind::Snapshot(
                            super::super::grpc_bridge_adapter::proto_v3::AppStructureSyncSnapshotV3 {
                                snapshot: Some(snapshot),
                            },
                        ),
                    ),
                }
            };
            Ok(Response::new(
                super::super::grpc_bridge_adapter::proto_v3::RefreshAppStructureResponse {
                    result: Some(
                        super::super::grpc_bridge_adapter::proto_v3::refresh_app_structure_response::Result::Response(
                            result,
                        ),
                    ),
                },
            ))
        }
    }

    struct TestGrpcStructureBridge {
        endpoint: String,
        shutdown: Option<tokio::sync::oneshot::Sender<()>>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl TestGrpcStructureBridge {
        fn start() -> Self {
            let std_listener =
                std::net::TcpListener::bind("127.0.0.1:0").expect("bind grpc test bridge");
            std_listener
                .set_nonblocking(true)
                .expect("set nonblocking listener");
            let addr = std_listener.local_addr().expect("listener addr");
            let (shutdown_tx, shutdown_rx) = mpsc::channel();

            let thread = thread::spawn(move || {
                let runtime = tokio::runtime::Runtime::new().expect("grpc test runtime");
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(std_listener)
                        .expect("tokio listener");
                    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                    shutdown_tx.send(tx).expect("send shutdown handle");
                    tonic::transport::Server::builder()
                        .add_service(
                            super::super::grpc_bridge_adapter::proto_v2::appfs_connector_v2_server::AppfsConnectorV2Server::new(
                                TestGrpcConnectorV2Service,
                            ),
                        )
                        .add_service(
                            super::super::grpc_bridge_adapter::proto_v3::appfs_connector_v3_server::AppfsConnectorV3Server::new(
                                TestGrpcConnectorV3Service,
                            ),
                        )
                        .serve_with_incoming_shutdown(
                            tokio_stream::wrappers::TcpListenerStream::new(listener),
                            async move {
                                let _ = rx.await;
                            },
                        )
                        .await
                        .expect("run grpc test server");
                });
            });

            Self {
                endpoint: format!("http://{}", addr),
                shutdown: Some(shutdown_rx.recv().expect("recv shutdown handle")),
                thread: Some(thread),
            }
        }
    }

    impl Drop for TestGrpcStructureBridge {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn grpc_node_kind(
        kind: &str,
    ) -> super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3 {
        match kind {
            "directory" => {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3::Directory
            }
            "action_file" => {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3::ActionFile
            }
            "snapshot_resource" => super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3::SnapshotResource,
            "live_resource" => {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3::LiveResource
            }
            "static_json_resource" => {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3::StaticJsonResource
            }
            _ => {
                super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeKindV3::Unspecified
            }
        }
    }

    fn grpc_structure_snapshot(
        scope: &str,
    ) -> super::super::grpc_bridge_adapter::proto_v3::AppStructureSnapshotV3 {
        let snapshot = structure_snapshot(scope);
        let ownership_prefixes = snapshot
            .get("ownership_prefixes")
            .and_then(|value| value.as_array())
            .expect("ownership prefixes")
            .iter()
            .filter_map(|value| value.as_str().map(ToString::to_string))
            .collect::<Vec<_>>();
        let nodes = snapshot
            .get("nodes")
            .and_then(|value| value.as_array())
            .expect("nodes")
            .iter()
            .map(
                |node| super::super::grpc_bridge_adapter::proto_v3::AppStructureNodeV3 {
                    path: node
                        .get("path")
                        .and_then(|value| value.as_str())
                        .expect("node path")
                        .to_string(),
                    kind: grpc_node_kind(
                        node.get("kind")
                            .and_then(|value| value.as_str())
                            .expect("node kind"),
                    ) as i32,
                    manifest_entry_json: node
                        .get("manifest_entry")
                        .cloned()
                        .map(|value| serde_json::to_string(&value).expect("manifest json")),
                    seed_content_json: node
                        .get("seed_content")
                        .cloned()
                        .map(|value| serde_json::to_string(&value).expect("seed json")),
                    r#mutable: node
                        .get("mutable")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false),
                    scope: node
                        .get("scope")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string),
                },
            )
            .collect::<Vec<_>>();

        super::super::grpc_bridge_adapter::proto_v3::AppStructureSnapshotV3 {
            app_id: snapshot
                .get("app_id")
                .and_then(|value| value.as_str())
                .expect("snapshot app_id")
                .to_string(),
            revision: snapshot
                .get("revision")
                .and_then(|value| value.as_str())
                .expect("snapshot revision")
                .to_string(),
            active_scope: snapshot
                .get("active_scope")
                .and_then(|value| value.as_str())
                .map(ToString::to_string),
            ownership_prefixes,
            nodes,
        }
    }

    fn connector_info_body() -> JsonValue {
        json!({
            "connector_id": "test-http-v3",
            "version": "0.4.0-test",
            "app_id": "aiim",
            "transport": "http_bridge",
            "supports_snapshot": true,
            "supports_live": true,
            "supports_action": true,
            "optional_features": ["structure_sync_v3"],
        })
    }

    fn action_manifest(template: &str) -> JsonValue {
        json!({
            "kind": "action",
            "input_mode": "json",
            "execution_mode": "inline",
            "template": template,
        })
    }

    fn snapshot_manifest(template: &str, max_bytes: u64) -> JsonValue {
        json!({
            "template": template,
            "kind": "resource",
            "output_mode": "jsonl",
            "consistency": "read_through",
            "snapshot": {
                "max_materialized_bytes": max_bytes,
                "prewarm": true,
                "prewarm_timeout_ms": 5000,
                "read_through_timeout_ms": 10000,
                "on_timeout": "return_stale"
            }
        })
    }

    fn structure_snapshot(scope: &str) -> JsonValue {
        let mut nodes = vec![
            json!({"path":"contacts","kind":"directory","mutable":false}),
            json!({"path":"contacts/zhangsan","kind":"directory","mutable":false}),
            json!({
                "path":"contacts/zhangsan/send_message.act",
                "kind":"action_file",
                "manifest_entry": action_manifest("contacts/{contact_id}/send_message.act"),
                "mutable": true
            }),
            json!({"path":"_app","kind":"directory","mutable":false}),
            json!({
                "path":"_app/enter_scope.act",
                "kind":"action_file",
                "manifest_entry": action_manifest("_app/enter_scope.act"),
                "mutable": true
            }),
            json!({
                "path":"_app/refresh_structure.act",
                "kind":"action_file",
                "manifest_entry": action_manifest("_app/refresh_structure.act"),
                "mutable": true
            }),
        ];

        match scope {
            "chat-long" => {
                nodes.extend([
                    json!({"path":"chats","kind":"directory","mutable":false,"scope":"chat-long"}),
                    json!({"path":"chats/chat-long","kind":"directory","mutable":false,"scope":"chat-long"}),
                    json!({
                        "path":"chats/chat-long/messages.res.jsonl",
                        "kind":"snapshot_resource",
                        "manifest_entry": snapshot_manifest("chats/chat-long/messages.res.jsonl", 1024),
                        "mutable": false,
                        "scope":"chat-long"
                    }),
                ]);
            }
            _ => {
                nodes.extend([
                    json!({"path":"chats","kind":"directory","mutable":false,"scope":"chat-001"}),
                    json!({"path":"chats/chat-001","kind":"directory","mutable":false,"scope":"chat-001"}),
                    json!({
                        "path":"chats/chat-001/messages.res.jsonl",
                        "kind":"snapshot_resource",
                        "manifest_entry": snapshot_manifest("chats/chat-001/messages.res.jsonl", 10 * 1024 * 1024),
                        "mutable": false,
                        "scope":"chat-001"
                    }),
                ]);
            }
        }

        json!({
            "app_id":"aiim",
            "revision": format!("demo-structure-{scope}"),
            "active_scope": scope,
            "ownership_prefixes":["_meta","contacts","chats","_app"],
            "nodes": nodes,
        })
    }

    fn parse_http_request(stream: &mut TcpStream) -> Option<(String, JsonValue)> {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }

        let split = buffer.windows(4).position(|window| window == b"\r\n\r\n")?;
        let (header, body) = buffer.split_at(split + 4);
        let header_text = String::from_utf8_lossy(header);
        let request_line = header_text.lines().next()?;
        let mut parts = request_line.split_whitespace();
        let _method = parts.next()?;
        let route = parts.next()?.to_string();
        let content_length = header_text
            .lines()
            .find_map(|line| {
                line.split_once(':').and_then(|(name, value)| {
                    if name.eq_ignore_ascii_case("Content-Length") {
                        value.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(0);

        let mut body_bytes = body.to_vec();
        while body_bytes.len() < content_length {
            let read = stream.read(&mut chunk).ok()?;
            if read == 0 {
                break;
            }
            body_bytes.extend_from_slice(&chunk[..read]);
        }

        let payload = if content_length == 0 {
            json!({})
        } else {
            serde_json::from_slice(&body_bytes[..content_length]).ok()?
        };
        Some((route, payload))
    }

    fn write_http_json(stream: &mut TcpStream, status: u16, body: &JsonValue) {
        let body_bytes = serde_json::to_vec(body).expect("encode body");
        let response = format!(
            "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            body_bytes.len()
        );
        stream
            .write_all(response.as_bytes())
            .expect("write headers");
        stream.write_all(&body_bytes).expect("write body");
        stream.flush().expect("flush response");
    }

    fn handle_test_http_connection(stream: &mut TcpStream) {
        let Some((route, payload)) = parse_http_request(stream) else {
            return;
        };

        let (status, body) = match route.as_str() {
            "/v2/connector/info" => (200, connector_info_body()),
            "/v3/connector/structure/get" => {
                let known_revision = payload
                    .get("request")
                    .and_then(|value| value.get("known_revision"))
                    .and_then(|value| value.as_str());
                if known_revision == Some("demo-structure-chat-001") {
                    (
                        200,
                        json!({
                            "result": {
                                "kind": "unchanged",
                                "app_id": "aiim",
                                "revision": "demo-structure-chat-001",
                                "active_scope": "chat-001"
                            }
                        }),
                    )
                } else {
                    (
                        200,
                        json!({
                            "result": {
                                "kind": "snapshot",
                                "snapshot": structure_snapshot("chat-001")
                            }
                        }),
                    )
                }
            }
            "/v3/connector/structure/refresh" => {
                let request = payload.get("request").cloned().unwrap_or_else(|| json!({}));
                let target_scope = request
                    .get("target_scope")
                    .and_then(|value| value.as_str())
                    .unwrap_or("chat-001");
                let snapshot = structure_snapshot(target_scope);
                let revision = snapshot
                    .get("revision")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let known_revision = request
                    .get("known_revision")
                    .and_then(|value| value.as_str());
                if known_revision == Some(revision.as_str()) {
                    (
                        200,
                        json!({
                            "result": {
                                "kind": "unchanged",
                                "app_id": "aiim",
                                "revision": revision,
                                "active_scope": snapshot.get("active_scope").and_then(|value| value.as_str())
                            }
                        }),
                    )
                } else {
                    (
                        200,
                        json!({"result": {"kind": "snapshot", "snapshot": snapshot}}),
                    )
                }
            }
            _ => (
                404,
                json!({
                    "code":"NOT_SUPPORTED",
                    "message": format!("unknown path: {route}"),
                    "retryable": false
                }),
            ),
        };

        write_http_json(stream, status, &body);
    }

    fn append_text(path: &Path, text: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open append");
        file.write_all(text.as_bytes()).expect("append text");
        file.flush().expect("flush append");
    }

    fn token_events(events_path: &Path, token: &str) -> Vec<JsonValue> {
        let content = fs::read_to_string(events_path).expect("read events");
        content
            .lines()
            .filter(|line| line.contains(token))
            .map(|line| serde_json::from_str(line).expect("event json"))
            .collect()
    }

    #[test]
    fn structured_bootstrap_exposes_app_control_actions() {
        let (_temp, adapter) = structured_adapter();

        assert!(adapter.app_dir.join("_app/enter_scope.act").exists());
        assert!(adapter.app_dir.join("_app/refresh_structure.act").exists());
        assert!(adapter
            .action_specs
            .iter()
            .any(|spec| spec.template == "_app/enter_scope.act"));
        assert!(adapter
            .action_specs
            .iter()
            .any(|spec| spec.template == "_app/refresh_structure.act"));
        assert!(adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-001/messages.res.jsonl"));
    }

    #[test]
    fn enter_scope_action_refreshes_structure_and_reloads_manifest() {
        let (_temp, mut adapter) = structured_adapter();
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("_app/enter_scope.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");

        append_text(
            &action_path,
            "{\"target_scope\":\"chat-long\",\"client_token\":\"scope-001\"}\n",
        );
        adapter.poll_once().expect("poll enter scope");

        assert!(adapter.app_dir.join("chats/chat-long").exists());
        assert!(!adapter.app_dir.join("chats/chat-001").exists());
        assert!(adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-long/messages.res.jsonl"));
        assert!(!adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-001/messages.res.jsonl"));

        let events = token_events(&events_path, "scope-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
        let content = events[0]
            .get("content")
            .expect("enter_scope completion content");
        assert_eq!(
            content.get("active_scope").and_then(|value| value.as_str()),
            Some("chat-long")
        );
        assert_eq!(
            content.get("refreshed").and_then(|value| value.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn refresh_structure_action_reports_unchanged_revision_when_scope_is_same() {
        let (_temp, mut adapter) = structured_adapter();
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("_app/refresh_structure.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");

        append_text(&action_path, "{\"client_token\":\"refresh-001\"}\n");
        adapter.poll_once().expect("poll refresh structure");

        let events = token_events(&events_path, "refresh-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
        let content = events[0]
            .get("content")
            .expect("refresh completion content");
        assert_eq!(
            content.get("active_scope").and_then(|value| value.as_str()),
            Some("chat-001")
        );
        assert_eq!(
            content.get("refreshed").and_then(|value| value.as_bool()),
            Some(false)
        );
        assert!(adapter.app_dir.join("chats/chat-001").exists());
    }

    #[test]
    fn http_bridge_structure_bootstrap_exposes_app_control_actions() {
        let bridge = TestHttpStructureBridge::start();
        let temp = TempDir::new().expect("tempdir");
        let adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            http_bridge_config(bridge.endpoint.clone()),
        )
        .expect("http structured adapter");

        assert!(adapter.app_dir.join("_app/enter_scope.act").exists());
        assert!(adapter.app_dir.join("_app/refresh_structure.act").exists());
        assert!(adapter.app_dir.join("chats/chat-001").exists());
        assert!(adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-001/messages.res.jsonl"));
    }

    #[test]
    fn http_bridge_enter_scope_refreshes_structure_and_reloads_manifest() {
        let bridge = TestHttpStructureBridge::start();
        let temp = TempDir::new().expect("tempdir");
        let mut adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            http_bridge_config(bridge.endpoint.clone()),
        )
        .expect("http structured adapter");
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("_app/enter_scope.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");

        append_text(
            &action_path,
            "{\"target_scope\":\"chat-long\",\"client_token\":\"scope-http-001\"}\n",
        );
        adapter.poll_once().expect("poll enter scope");

        assert!(adapter.app_dir.join("chats/chat-long").exists());
        assert!(!adapter.app_dir.join("chats/chat-001").exists());
        assert!(adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-long/messages.res.jsonl"));

        let events = token_events(&events_path, "scope-http-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
    }

    #[test]
    fn http_bridge_refresh_structure_reports_unchanged_revision() {
        let bridge = TestHttpStructureBridge::start();
        let temp = TempDir::new().expect("tempdir");
        let mut adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            http_bridge_config(bridge.endpoint.clone()),
        )
        .expect("http structured adapter");
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("_app/refresh_structure.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");

        append_text(&action_path, "{\"client_token\":\"refresh-http-001\"}\n");
        adapter.poll_once().expect("poll refresh");

        let events = token_events(&events_path, "refresh-http-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
        let content = events[0].get("content").expect("refresh content");
        assert_eq!(
            content.get("refreshed").and_then(|value| value.as_bool()),
            Some(false)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_structure_bootstrap_exposes_app_control_actions() {
        let bridge = TestGrpcStructureBridge::start();
        let temp = TempDir::new().expect("tempdir");
        let adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            grpc_bridge_config(bridge.endpoint.clone()),
        )
        .expect("grpc structured adapter");

        assert!(adapter.app_dir.join("_app/enter_scope.act").exists());
        assert!(adapter.app_dir.join("_app/refresh_structure.act").exists());
        assert!(adapter.app_dir.join("chats/chat-001").exists());
        assert!(adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-001/messages.res.jsonl"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_enter_scope_refreshes_structure_and_reloads_manifest() {
        let bridge = TestGrpcStructureBridge::start();
        let temp = TempDir::new().expect("tempdir");
        let mut adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            grpc_bridge_config(bridge.endpoint.clone()),
        )
        .expect("grpc structured adapter");
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("_app/enter_scope.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");

        append_text(
            &action_path,
            "{\"target_scope\":\"chat-long\",\"client_token\":\"scope-grpc-001\"}\n",
        );
        adapter.poll_once().expect("poll enter scope");

        assert!(adapter.app_dir.join("chats/chat-long").exists());
        assert!(!adapter.app_dir.join("chats/chat-001").exists());
        assert!(adapter
            .snapshot_specs
            .iter()
            .any(|spec| spec.template == "chats/chat-long/messages.res.jsonl"));

        let events = token_events(&events_path, "scope-grpc-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_refresh_structure_reports_unchanged_revision() {
        let bridge = TestGrpcStructureBridge::start();
        let temp = TempDir::new().expect("tempdir");
        let mut adapter = AppfsAdapter::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
            grpc_bridge_config(bridge.endpoint.clone()),
        )
        .expect("grpc structured adapter");
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("_app/refresh_structure.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");

        append_text(&action_path, "{\"client_token\":\"refresh-grpc-001\"}\n");
        adapter.poll_once().expect("poll refresh");

        let events = token_events(&events_path, "refresh-grpc-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
        let content = events[0].get("content").expect("refresh content");
        assert_eq!(
            content.get("refreshed").and_then(|value| value.as_bool()),
            Some(false)
        );
    }

    #[test]
    fn multiline_partial_write_defers_until_payload_is_complete() {
        let (_temp, mut adapter) = fixture_adapter();
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("contacts/zhangsan/send_message.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");
        let rel = "contacts/zhangsan/send_message.act";

        append_text(
            &action_path,
            "{\"client_token\":\"baseline\",\"text\":\"baseline\"}\n",
        );
        adapter.poll_once().expect("baseline poll");
        let baseline_offset = adapter
            .action_cursors
            .get(rel)
            .expect("baseline cursor")
            .offset;

        append_text(
            &action_path,
            "{\"client_token\":\"ct-ml-1\",\"text\":\"你好\n",
        );
        adapter.poll_once().expect("poll after first fragment");
        assert!(token_events(&events_path, "ct-ml-1").is_empty());
        let cursor = adapter
            .action_cursors
            .get(rel)
            .expect("cursor after first fragment");
        assert_eq!(cursor.offset, baseline_offset);
        assert_eq!(
            cursor.pending_multiline_eof_len,
            Some(fs::metadata(&action_path).expect("meta").len())
        );

        append_text(&action_path, "hello\n");
        adapter.poll_once().expect("poll after second fragment");
        assert!(token_events(&events_path, "ct-ml-1").is_empty());
        let cursor = adapter
            .action_cursors
            .get(rel)
            .expect("cursor after second fragment");
        assert_eq!(cursor.offset, baseline_offset);
        assert_eq!(
            cursor.pending_multiline_eof_len,
            Some(fs::metadata(&action_path).expect("meta").len())
        );

        append_text(&action_path, "好！\"}\n");
        adapter.poll_once().expect("poll after final fragment");

        let events = token_events(&events_path, "ct-ml-1");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
        let cursor = adapter
            .action_cursors
            .get(rel)
            .expect("cursor after completion");
        assert!(cursor.offset > baseline_offset);
        assert_eq!(cursor.pending_multiline_eof_len, None);
    }

    #[test]
    fn stale_incomplete_multiline_is_eventually_consumed_after_no_growth() {
        let (_temp, mut adapter) = fixture_adapter();
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("contacts/zhangsan/send_message.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");
        let rel = "contacts/zhangsan/send_message.act";

        append_text(
            &action_path,
            "{\"client_token\":\"ct-bad\",\"text\":\"broken\n",
        );
        adapter.poll_once().expect("first pending poll");
        let pending_offset = adapter
            .action_cursors
            .get(rel)
            .expect("pending cursor")
            .offset;
        assert_eq!(pending_offset, 0);
        assert!(token_events(&events_path, "ct-bad").is_empty());

        adapter
            .poll_once()
            .expect("second poll consumes stale fragment");
        let consumed_offset = adapter
            .action_cursors
            .get(rel)
            .expect("consumed cursor")
            .offset;
        assert!(consumed_offset > pending_offset);
        assert!(token_events(&events_path, "ct-bad").is_empty());

        append_text(
            &action_path,
            "\n{\"client_token\":\"ct-recover\",\"text\":\"ok\"}\n",
        );
        adapter.poll_once().expect("recovery poll");

        let events = token_events(&events_path, "ct-recover");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
    }

    #[test]
    fn broken_multiline_prefix_does_not_block_valid_append_behind_it() {
        let (_temp, mut adapter) = fixture_adapter();
        adapter.prepare_action_sinks().expect("prepare sinks");

        let action_path = adapter.app_dir.join("contacts/zhangsan/send_message.act");
        let events_path = adapter.app_dir.join("_stream/events.evt.jsonl");
        let rel = "contacts/zhangsan/send_message.act";

        append_text(
            &action_path,
            "{\"client_token\":\"ct-bad\",\"text\":\"broken\n",
        );
        adapter.poll_once().expect("first pending poll");
        assert_eq!(
            adapter
                .action_cursors
                .get(rel)
                .expect("pending cursor")
                .offset,
            0
        );

        append_text(
            &action_path,
            "{\"client_token\":\"ct-next\",\"text\":\"ok\"}\n",
        );
        adapter.poll_once().expect("second poll with valid append");

        assert!(token_events(&events_path, "ct-bad").is_empty());
        let events = token_events(&events_path, "ct-next");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
        let cursor = adapter
            .action_cursors
            .get(rel)
            .expect("cursor after recovery");
        assert!(cursor.offset > 0);
        assert_eq!(cursor.pending_multiline_eof_len, None);
    }
}
