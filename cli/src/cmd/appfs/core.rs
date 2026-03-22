use agentfs_sdk::{
    AdapterErrorV1, AdapterExecutionModeV1, AdapterInputModeV1, AdapterSubmitOutcomeV1,
    AppAdapterV1, DemoAppAdapterV1, RequestContextV1,
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
use super::errors::ERR_INVALID_PAYLOAD;
use super::grpc_bridge_adapter::GrpcBridgeAdapterV1;
use super::http_bridge_adapter::HttpBridgeAdapterV1;
use super::shared::{
    action_template_matches, boundary_probe_from_bytes, collect_files_with_suffix,
    decode_jsonl_line, env_flag_enabled, extract_client_token, has_odd_unescaped_quotes,
    is_handle_format_valid, is_safe_action_rel_path, is_transient_action_sink_busy,
    parse_snapshot_on_timeout_policy, recover_multiline_json_payload, template_specificity,
};
use super::{
    ActionCursorDoc, ActionCursorState, ActionSpec, AppfsAdapter, AppfsBridgeConfig, CursorState,
    ExecutionMode, InputMode, ManifestContract, ManifestDoc, ProcessOutcome, SnapshotSpec,
    StreamingJob, ACTION_CURSORS_FILENAME, DEFAULT_RETENTION_HINT_SEC,
    DEFAULT_SNAPSHOT_MAX_MATERIALIZED_BYTES, DEFAULT_SNAPSHOT_READ_THROUGH_TIMEOUT_MS,
    SNAPSHOT_EXPAND_JOURNAL_FILENAME,
};

impl AppfsAdapter {
    pub(super) fn new(
        root: PathBuf,
        app_id: String,
        session_id: String,
        bridge_config: AppfsBridgeConfig,
    ) -> Result<Self> {
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

        let business_adapter: Box<dyn AppAdapterV1> =
            if let Some(endpoint) = normalized_grpc_endpoint {
                eprintln!("AppFS adapter using gRPC bridge endpoint: {endpoint}");
                Box::new(
                    GrpcBridgeAdapterV1::new(
                        app_id.clone(),
                        endpoint,
                        Duration::from_millis(bridge_config.adapter_grpc_timeout_ms.max(1)),
                        bridge_config.runtime_options,
                    )
                    .map_err(|err| {
                        anyhow::anyhow!("failed to initialize gRPC bridge adapter: {err}")
                    })?,
                )
            } else if let Some(endpoint) = normalized_http_endpoint {
                eprintln!("AppFS adapter using HTTP bridge endpoint: {endpoint}");
                Box::new(HttpBridgeAdapterV1::new(
                    app_id.clone(),
                    endpoint,
                    Duration::from_millis(bridge_config.adapter_http_timeout_ms.max(1)),
                    bridge_config.runtime_options,
                ))
            } else {
                Box::new(DemoAppAdapterV1::new(app_id.clone()))
            };
        if business_adapter.app_id() != app_id {
            anyhow::bail!(
                "Adapter app_id mismatch: adapter={} runtime={}",
                business_adapter.app_id(),
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
            business_adapter,
        };
        adapter.initialize_snapshot_states();
        adapter.recover_snapshot_expand_journal()?;
        adapter.load_known_handles()?;
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
                if let Some((merged_payload, merged_line_end, consumed_lines)) =
                    recover_multiline_json_payload(&bytes, &payload, line_end, &spec)
                {
                    eprintln!(
                        "AppFS adapter normalized shell-expanded newline for {rel}: consumed_lines={consumed_lines}"
                    );
                    payload = merged_payload;
                    payload_line_end = merged_line_end;
                }
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
                    position = payload_line_end;
                    continue;
                }
            }

            match self.process_action(&rel, &spec, &payload, client_token_override)? {
                ProcessOutcome::Consumed => {
                    cursor.offset = payload_line_end as u64;
                    cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
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
            Ok(action_dispatcher::DispatchRoute::BusinessSubmit) => {}
            Err(action_dispatcher::DispatchRouteParseError::MalformedPagingFetchNext) => {
                eprintln!(
                    "AppFS adapter rejected malformed paging handle at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
            Err(action_dispatcher::DispatchRouteParseError::MalformedPagingClose) => {
                eprintln!(
                    "AppFS adapter rejected malformed paging close handle at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
            Err(action_dispatcher::DispatchRouteParseError::MalformedSnapshotRefresh) => {
                eprintln!(
                    "AppFS adapter rejected malformed snapshot refresh payload at submit-time: {}",
                    normalized_path
                );
                return Ok(ProcessOutcome::Consumed);
            }
        }

        let request_ctx = RequestContextV1 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.clone(),
            client_token: client_token.clone(),
        };
        let adapter_input_mode = match spec.input_mode {
            InputMode::Json => AdapterInputModeV1::Json,
        };
        let adapter_execution_mode = match spec.execution_mode {
            ExecutionMode::Inline => AdapterExecutionModeV1::Inline,
            ExecutionMode::Streaming => AdapterExecutionModeV1::Streaming,
        };

        match self.business_adapter.submit_action(
            &normalized_path,
            payload,
            adapter_input_mode,
            adapter_execution_mode,
            &request_ctx,
        ) {
            Ok(AdapterSubmitOutcomeV1::Completed { content }) => {
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
            Ok(AdapterSubmitOutcomeV1::Streaming { plan }) => {
                self.enqueue_streaming_job(&normalized_path, &request_id, client_token, plan)?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            }) => {
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
            Err(AdapterErrorV1::Internal { message }) => {
                eprintln!(
                    "AppFS adapter transient bridge failure for {normalized_path}: {message}; will retry without advancing cursor"
                );
                Ok(ProcessOutcome::RetryPending)
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

    fn load_manifest_contract(manifest_path: &Path) -> Result<ManifestContract> {
        let manifest_json = fs::read_to_string(manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest: ManifestDoc = serde_json::from_str(&manifest_json)
            .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

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
            eprintln!(
                "AppFS adapter warning: no action definitions found in {}",
                manifest_path.display()
            );
        }

        Ok(ManifestContract {
            action_specs,
            snapshot_specs,
            requires_paging_controls,
        })
    }

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

    fn new_request_id() -> String {
        let uuid = Uuid::new_v4().simple().to_string();
        format!("req-{}", &uuid[..8])
    }
}
