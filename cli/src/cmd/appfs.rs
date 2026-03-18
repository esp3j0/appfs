use agentfs_sdk::{
    AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
    AdapterInputModeV1, AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1,
    DemoAppAdapterV1, RequestContextV1,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

mod bridge_resilience;
mod grpc_bridge_adapter;
mod http_bridge_adapter;

use bridge_resilience::BridgeRuntimeOptions;
use grpc_bridge_adapter::GrpcBridgeAdapterV1;
use http_bridge_adapter::HttpBridgeAdapterV1;

const DEFAULT_RETENTION_HINT_SEC: i64 = 86400;
const MIN_POLL_MS: u64 = 50;
const ACTION_CURSORS_FILENAME: &str = "action-cursors.res.json";
const ACTION_CURSOR_PROBE_WINDOW: usize = 64;

const ERR_PAGER_HANDLE_NOT_FOUND: &str = "PAGER_HANDLE_NOT_FOUND";
const ERR_PAGER_HANDLE_EXPIRED: &str = "PAGER_HANDLE_EXPIRED";
const ERR_PAGER_HANDLE_CLOSED: &str = "PAGER_HANDLE_CLOSED";
const ERR_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
const ERR_INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
const ERR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
const MAX_SEGMENT_BYTES: usize = 255;

const ALLOWED_SEGMENT_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-~";

#[derive(Debug, Clone)]
pub struct AppfsServeArgs {
    pub root: PathBuf,
    pub app_id: String,
    pub session_id: Option<String>,
    pub poll_ms: u64,
    pub adapter_http_endpoint: Option<String>,
    pub adapter_http_timeout_ms: u64,
    pub adapter_grpc_endpoint: Option<String>,
    pub adapter_grpc_timeout_ms: u64,
    pub adapter_bridge_max_retries: u32,
    pub adapter_bridge_initial_backoff_ms: u64,
    pub adapter_bridge_max_backoff_ms: u64,
    pub adapter_bridge_circuit_breaker_failures: u32,
    pub adapter_bridge_circuit_breaker_cooldown_ms: u64,
}

#[derive(Debug, Clone)]
struct AppfsBridgeConfig {
    adapter_http_endpoint: Option<String>,
    adapter_http_timeout_ms: u64,
    adapter_grpc_endpoint: Option<String>,
    adapter_grpc_timeout_ms: u64,
    runtime_options: BridgeRuntimeOptions,
}

pub async fn handle_appfs_adapter_command(args: AppfsServeArgs) -> Result<()> {
    let AppfsServeArgs {
        root,
        app_id,
        session_id,
        poll_ms,
        adapter_http_endpoint,
        adapter_http_timeout_ms,
        adapter_grpc_endpoint,
        adapter_grpc_timeout_ms,
        adapter_bridge_max_retries,
        adapter_bridge_initial_backoff_ms,
        adapter_bridge_max_backoff_ms,
        adapter_bridge_circuit_breaker_failures,
        adapter_bridge_circuit_breaker_cooldown_ms,
    } = args;

    let session_id = session_id.unwrap_or_else(|| {
        let uuid = Uuid::new_v4().simple().to_string();
        format!("sess-{}", &uuid[..8])
    });
    let bridge_runtime_options = BridgeRuntimeOptions::from_cli(
        adapter_bridge_max_retries,
        adapter_bridge_initial_backoff_ms,
        adapter_bridge_max_backoff_ms,
        adapter_bridge_circuit_breaker_failures,
        adapter_bridge_circuit_breaker_cooldown_ms,
    );
    let bridge_config = AppfsBridgeConfig {
        adapter_http_endpoint,
        adapter_http_timeout_ms,
        adapter_grpc_endpoint,
        adapter_grpc_timeout_ms,
        runtime_options: bridge_runtime_options,
    };

    let mut adapter = AppfsAdapter::new(root, app_id, session_id, bridge_config)?;
    adapter.prepare_action_sinks()?;

    eprintln!(
        "AppFS adapter started for {} (session={})",
        adapter.app_dir.display(),
        adapter.session_id
    );
    eprintln!("Press Ctrl+C to stop.");

    let mut interval = tokio::time::interval(Duration::from_millis(poll_ms.max(MIN_POLL_MS)));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("AppFS adapter stopping...");
                return Ok(());
            }
            _ = interval.tick() => {
                if let Err(err) = adapter.poll_once() {
                    eprintln!("AppFS adapter poll error: {err:#}");
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessOutcome {
    Consumed,
    RetryPending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionMode {
    Inline,
    Streaming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Json,
}

#[derive(Debug, Clone)]
struct ActionSpec {
    template: String,
    input_mode: InputMode,
    execution_mode: ExecutionMode,
    max_payload_bytes: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct ManifestDoc {
    #[serde(default)]
    nodes: HashMap<String, ManifestNodeDoc>,
}

#[derive(Debug, Deserialize)]
struct ManifestNodeDoc {
    kind: String,
    #[serde(default)]
    input_mode: Option<String>,
    #[serde(default)]
    execution_mode: Option<String>,
    #[serde(default)]
    max_payload_bytes: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CursorState {
    min_seq: i64,
    max_seq: i64,
    retention_hint_sec: i64,
}

#[derive(Debug, Clone)]
struct PagingHandle {
    page_no: u64,
    closed: bool,
    owner_session: String,
    expires_at_ts: Option<i64>,
}

#[derive(Debug, Clone)]
struct PagingRequest {
    handle_id: String,
    session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StreamingJob {
    request_id: String,
    path: String,
    #[serde(default)]
    client_token: Option<String>,
    #[serde(default)]
    accepted: Option<JsonValue>,
    #[serde(default)]
    progress: Option<JsonValue>,
    terminal: JsonValue,
    stage: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct ActionCursorState {
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    boundary_probe: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ActionCursorDoc {
    #[serde(default)]
    actions: HashMap<String, ActionCursorState>,
}

struct AppfsAdapter {
    app_id: String,
    session_id: String,
    app_dir: PathBuf,
    action_specs: Vec<ActionSpec>,
    events_path: PathBuf,
    cursor_path: PathBuf,
    replay_dir: PathBuf,
    jobs_path: PathBuf,
    action_cursors_path: PathBuf,
    cursor: CursorState,
    next_seq: i64,
    action_cursors: HashMap<String, ActionCursorState>,
    handles: HashMap<String, PagingHandle>,
    handle_aliases: HashMap<String, String>,
    streaming_jobs: Vec<StreamingJob>,
    business_adapter: Box<dyn AppAdapterV1>,
}

impl AppfsAdapter {
    fn new(
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
        let action_specs = Self::load_action_specs(&manifest_path)?;
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
            action_specs,
            events_path,
            cursor_path,
            replay_dir,
            jobs_path: jobs_path.clone(),
            action_cursors_path: action_cursors_path.clone(),
            cursor,
            next_seq,
            action_cursors: Self::load_action_cursors(&action_cursors_path)?,
            handles: HashMap::new(),
            handle_aliases: HashMap::new(),
            streaming_jobs: Self::load_streaming_jobs(&jobs_path)?,
            business_adapter,
        };
        adapter.load_known_handles()?;
        Ok(adapter)
    }

    fn prepare_action_sinks(&mut self) -> Result<()> {
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

    fn poll_once(&mut self) -> Result<()> {
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
            let payload = match decode_jsonl_line(line_bytes, position == 0) {
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

            match self.process_action(&rel, &spec, &payload)? {
                ProcessOutcome::Consumed => {
                    cursor.offset = line_end as u64;
                    cursor.boundary_probe = boundary_probe_from_bytes(&bytes, cursor.offset);
                    position = line_end;
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
    ) -> Result<ProcessOutcome> {
        if let Err(code) = validate_payload(spec, payload) {
            eprintln!(
                "AppFS adapter rejected action payload for {rel}: validation={code} len={}",
                payload.len()
            );
            return Ok(ProcessOutcome::Consumed);
        }

        let normalized_path = format!("/{}", rel);
        let request_id = Self::new_request_id();
        let client_token = extract_client_token(payload);

        if normalized_path == "/_paging/fetch_next.act" {
            match parse_paging_request(payload) {
                Ok(request) => {
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
                Err(_) => {
                    eprintln!(
                        "AppFS adapter rejected malformed paging handle at submit-time: {}",
                        normalized_path
                    );
                    return Ok(ProcessOutcome::Consumed);
                }
            };
        }

        if normalized_path == "/_paging/close.act" {
            match parse_paging_request(payload) {
                Ok(request) => {
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
                Err(_) => {
                    eprintln!(
                        "AppFS adapter rejected malformed paging close handle at submit-time: {}",
                        normalized_path
                    );
                    return Ok(ProcessOutcome::Consumed);
                }
            };
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

    fn handle_fetch_next(
        &mut self,
        action_path: &str,
        request_id: &str,
        handle_id: &str,
        requester_session_id: Option<&str>,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        if !is_handle_format_valid(handle_id) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "invalid handle_id format",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let handle_key = self.resolve_handle_key(handle_id);
        let (owner_session, expires_at_ts, closed) = match self.handles.get(&handle_key) {
            Some(h) => (h.owner_session.clone(), h.expires_at_ts, h.closed),
            None => {
                self.emit_failed(
                    action_path,
                    request_id,
                    ERR_PAGER_HANDLE_NOT_FOUND,
                    "handle not found",
                    client_token,
                )?;
                return Ok(ProcessOutcome::Consumed);
            }
        };

        let effective_session = requester_session_id.unwrap_or(self.session_id.as_str());
        if effective_session != owner_session {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PERMISSION_DENIED,
                "cross-session handle access denied",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if closed {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_CLOSED,
                "handle already closed",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if expires_at_ts.is_some_and(|expiry| Utc::now().timestamp() >= expiry) {
            if let Some(handle) = self.handles.get_mut(&handle_key) {
                // Tombstone expired handles on explicit close requests so any
                // subsequent fetch observes deterministic CLOSED semantics.
                handle.closed = true;
            }
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_EXPIRED,
                "handle expired",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let current_page_no = self
            .handles
            .get(&handle_key)
            .expect("paging handle should exist after precheck")
            .page_no;
        let page_no = current_page_no + 1;
        let has_more = page_no < 3;
        let request_ctx = RequestContextV1 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: client_token.clone(),
        };
        match self.business_adapter.submit_control_action(
            action_path,
            AdapterControlActionV1::PagingFetchNext {
                handle_id: handle_key.clone(),
                page_no,
                has_more,
            },
            &request_ctx,
        ) {
            Ok(AdapterControlOutcomeV1::Completed { content }) => {
                if let Some(handle) = self.handles.get_mut(&handle_key) {
                    handle.page_no = page_no;
                }
                self.emit_event(
                    action_path,
                    request_id,
                    "action.completed",
                    Some(content),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            }) => {
                self.emit_failed_with_retryable(
                    action_path,
                    request_id,
                    &code,
                    &message,
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Internal { message }) => {
                eprintln!(
                    "AppFS adapter transient bridge failure for {action_path}: {message}; will retry without advancing cursor"
                );
                Ok(ProcessOutcome::RetryPending)
            }
        }
    }

    fn handle_close_handle(
        &mut self,
        action_path: &str,
        request_id: &str,
        handle_id: &str,
        requester_session_id: Option<&str>,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        if !is_handle_format_valid(handle_id) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "invalid handle_id format",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let handle_key = self.resolve_handle_key(handle_id);
        let (owner_session, expires_at_ts, closed) = match self.handles.get(&handle_key) {
            Some(h) => (h.owner_session.clone(), h.expires_at_ts, h.closed),
            None => {
                self.emit_failed(
                    action_path,
                    request_id,
                    ERR_PAGER_HANDLE_NOT_FOUND,
                    "handle not found",
                    client_token,
                )?;
                return Ok(ProcessOutcome::Consumed);
            }
        };

        let effective_session = requester_session_id.unwrap_or(self.session_id.as_str());
        if effective_session != owner_session {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PERMISSION_DENIED,
                "cross-session handle access denied",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if closed {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_CLOSED,
                "handle already closed",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if expires_at_ts.is_some_and(|expiry| Utc::now().timestamp() >= expiry) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_EXPIRED,
                "handle expired",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let request_ctx = RequestContextV1 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: client_token.clone(),
        };
        match self.business_adapter.submit_control_action(
            action_path,
            AdapterControlActionV1::PagingClose {
                handle_id: handle_key.clone(),
            },
            &request_ctx,
        ) {
            Ok(AdapterControlOutcomeV1::Completed { content }) => {
                if let Some(handle) = self.handles.get_mut(&handle_key) {
                    handle.closed = true;
                }
                self.emit_event(
                    action_path,
                    request_id,
                    "action.completed",
                    Some(content),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            }) => {
                self.emit_failed_with_retryable(
                    action_path,
                    request_id,
                    &code,
                    &message,
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Internal { message }) => {
                eprintln!(
                    "AppFS adapter transient bridge failure for {action_path}: {message}; will retry without advancing cursor"
                );
                Ok(ProcessOutcome::RetryPending)
            }
        }
    }

    fn enqueue_streaming_job(
        &mut self,
        action_path: &str,
        request_id: &str,
        client_token: Option<String>,
        plan: AdapterStreamingPlanV1,
    ) -> Result<()> {
        self.streaming_jobs.push(StreamingJob {
            request_id: request_id.to_string(),
            path: action_path.to_string(),
            client_token,
            accepted: plan.accepted_content,
            progress: plan.progress_content,
            terminal: plan.terminal_content,
            stage: 0,
        });
        self.save_streaming_jobs()
    }

    fn drain_streaming_jobs(&mut self) -> Result<()> {
        if self.streaming_jobs.is_empty() {
            return Ok(());
        }

        let jobs = std::mem::take(&mut self.streaming_jobs);
        let mut next_jobs = Vec::with_capacity(jobs.len());
        for mut job in jobs {
            match job.stage {
                0 => {
                    self.emit_event(
                        &job.path,
                        &job.request_id,
                        "action.accepted",
                        Some(job.accepted.clone().unwrap_or_else(|| json!("accepted"))),
                        None,
                        job.client_token.clone(),
                    )?;
                    job.stage = 1;
                    next_jobs.push(job);
                }
                1 => {
                    self.emit_event(
                        &job.path,
                        &job.request_id,
                        "action.progress",
                        Some(
                            job.progress
                                .clone()
                                .unwrap_or_else(|| json!({ "percent": 50 })),
                        ),
                        None,
                        job.client_token.clone(),
                    )?;
                    job.stage = 2;
                    next_jobs.push(job);
                }
                _ => {
                    self.emit_event(
                        &job.path,
                        &job.request_id,
                        "action.completed",
                        Some(job.terminal),
                        None,
                        job.client_token,
                    )?;
                }
            }
        }
        self.streaming_jobs = next_jobs;
        self.save_streaming_jobs()
    }

    fn find_action_spec(&self, rel_path: &str) -> Option<&ActionSpec> {
        self.action_specs
            .iter()
            .find(|spec| action_template_matches(&spec.template, rel_path))
    }

    fn load_action_specs(manifest_path: &Path) -> Result<Vec<ActionSpec>> {
        let manifest_json = fs::read_to_string(manifest_path)
            .with_context(|| format!("Failed to read {}", manifest_path.display()))?;
        let manifest: ManifestDoc = serde_json::from_str(&manifest_json)
            .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

        let mut specs = Vec::new();
        for (template, node) in manifest.nodes {
            if node.kind != "action" || !template.ends_with(".act") {
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

            specs.push(ActionSpec {
                template: template.trim_start_matches('/').to_string(),
                input_mode,
                execution_mode,
                max_payload_bytes: node.max_payload_bytes,
            });
        }

        if specs.is_empty() {
            eprintln!(
                "AppFS adapter warning: no action definitions found in {}",
                manifest_path.display()
            );
        }

        Ok(specs)
    }

    fn emit_failed(
        &mut self,
        action_path: &str,
        request_id: &str,
        error_code: &str,
        message: &str,
        client_token: Option<String>,
    ) -> Result<()> {
        let retryable = error_code == ERR_PAGER_HANDLE_EXPIRED;
        self.emit_failed_with_retryable(
            action_path,
            request_id,
            error_code,
            message,
            retryable,
            client_token,
        )
    }

    fn emit_failed_with_retryable(
        &mut self,
        action_path: &str,
        request_id: &str,
        error_code: &str,
        message: &str,
        retryable: bool,
        client_token: Option<String>,
    ) -> Result<()> {
        self.emit_event(
            action_path,
            request_id,
            "action.failed",
            None,
            Some(json!({
                "code": error_code,
                "message": message,
                "retryable": retryable,
            })),
            client_token,
        )
    }

    fn emit_event(
        &mut self,
        action_path: &str,
        request_id: &str,
        event_type: &str,
        content: Option<JsonValue>,
        error: Option<JsonValue>,
        client_token: Option<String>,
    ) -> Result<()> {
        let seq = self.next_seq;
        self.next_seq += 1;

        let mut event = json!({
            "seq": seq,
            "event_id": format!("evt-{}", seq),
            "ts": Utc::now().to_rfc3339(),
            "app": self.app_id,
            "session_id": self.session_id,
            "request_id": request_id,
            "path": action_path,
            "type": event_type,
        });

        if let Some(content) = content {
            event["content"] = content;
        }
        if let Some(error) = error {
            event["error"] = error;
        }
        if let Some(token) = client_token {
            event["client_token"] = json!(token);
        }

        let line = serde_json::to_string(&event)?;
        self.publish_event(seq, &line)
    }

    fn publish_event(&mut self, seq: i64, line: &str) -> Result<()> {
        let mut events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)
            .with_context(|| {
                format!("Failed to open stream file {}", self.events_path.display())
            })?;
        writeln!(events, "{line}")?;
        events.flush()?;

        let replay_file = self.replay_dir.join(format!("{seq}.evt.jsonl"));
        fs::write(&replay_file, format!("{line}\n"))
            .with_context(|| format!("Failed to write replay file {}", replay_file.display()))?;

        self.cursor.max_seq = seq;
        if self.cursor.min_seq <= 0 {
            self.cursor.min_seq = seq;
        }
        if self.cursor.retention_hint_sec <= 0 {
            self.cursor.retention_hint_sec = DEFAULT_RETENTION_HINT_SEC;
        }
        self.save_cursor()?;
        Ok(())
    }

    fn save_cursor(&self) -> Result<()> {
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

    fn save_streaming_jobs(&self) -> Result<()> {
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

    fn load_known_handles(&mut self) -> Result<()> {
        let mut resources = Vec::new();
        collect_files_with_suffix(&self.app_dir, ".res.json", &mut resources)?;
        for path in resources {
            if path.starts_with(self.app_dir.join("_stream")) {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let json: JsonValue = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(handle_id) = json
                .get("page")
                .and_then(|p| p.get("handle_id"))
                .and_then(|h| h.as_str())
            {
                let normalized_handle = normalize_runtime_handle_id(handle_id);
                if normalized_handle != handle_id {
                    self.handle_aliases
                        .insert(handle_id.to_string(), normalized_handle.clone());
                }
                self.handles.insert(
                    normalized_handle,
                    PagingHandle {
                        page_no: 0,
                        closed: false,
                        owner_session: json
                            .get("page")
                            .and_then(|p| p.get("session_id"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .unwrap_or(self.session_id.as_str())
                            .to_string(),
                        expires_at_ts: json
                            .get("page")
                            .and_then(|p| p.get("expires_at"))
                            .and_then(|v| v.as_str())
                            .and_then(parse_rfc3339_timestamp),
                    },
                );
            }
        }
        Ok(())
    }

    fn resolve_handle_key(&self, requested: &str) -> String {
        if let Some(alias) = self.handle_aliases.get(requested) {
            return alias.clone();
        }
        let normalized = normalize_runtime_handle_id(requested);
        if self.handles.contains_key(&normalized) {
            return normalized;
        }
        requested.to_string()
    }

    fn new_request_id() -> String {
        let uuid = Uuid::new_v4().simple().to_string();
        format!("req-{}", &uuid[..8])
    }
}

fn collect_files_with_suffix(dir: &Path, suffix: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("Failed to read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_files_with_suffix(&path, suffix, out)?;
            continue;
        }

        if file_type.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|name| name.ends_with(suffix))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn boundary_probe_from_bytes(bytes: &[u8], offset: u64) -> Option<String> {
    if offset == 0 {
        return None;
    }
    let end = offset.min(bytes.len() as u64) as usize;
    if end == 0 {
        return None;
    }
    let start = end.saturating_sub(ACTION_CURSOR_PROBE_WINDOW);
    let hash = fnv1a_64(&bytes[start..end]);
    Some(format!("{hash:016x}"))
}

fn decode_jsonl_line(
    line_bytes: &[u8],
    allow_bom: bool,
) -> std::result::Result<Option<String>, String> {
    if line_bytes.is_empty() {
        return Ok(None);
    }

    let mut slice = line_bytes;
    if allow_bom && slice.starts_with(&[0xEF, 0xBB, 0xBF]) {
        slice = &slice[3..];
    }
    if slice.ends_with(b"\n") {
        slice = &slice[..slice.len().saturating_sub(1)];
    }
    if slice.ends_with(b"\r") {
        slice = &slice[..slice.len().saturating_sub(1)];
    }
    if slice.is_empty() {
        return Ok(None);
    }

    if let Some(decoded_utf16) = try_decode_utf16_line(slice, allow_bom) {
        if decoded_utf16.is_empty() {
            return Ok(None);
        }
        return Ok(Some(decoded_utf16));
    }

    let decoded = std::str::from_utf8(slice)
        .map_err(|err| format!("utf8 decode failed for JSONL line: {err}"))?;
    Ok(Some(decoded.to_string()))
}

#[derive(Clone, Copy)]
enum Utf16Endian {
    Le,
    Be,
}

fn try_decode_utf16_line(slice: &[u8], allow_bom: bool) -> Option<String> {
    if !slice.contains(&0x00) {
        return None;
    }

    let mut bytes = slice;
    let mut endian: Option<Utf16Endian> = None;

    if allow_bom && bytes.starts_with(&[0xFF, 0xFE]) {
        bytes = &bytes[2..];
        endian = Some(Utf16Endian::Le);
    } else if allow_bom && bytes.starts_with(&[0xFE, 0xFF]) {
        bytes = &bytes[2..];
        endian = Some(Utf16Endian::Be);
    }

    if bytes.is_empty() {
        return Some(String::new());
    }

    if bytes.len() % 2 != 0 {
        return None;
    }

    if endian.is_none() {
        let pair_count = bytes.len() / 2;
        if pair_count == 0 {
            return None;
        }
        let odd_zeros = bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count();
        let even_zeros = bytes.iter().step_by(2).filter(|b| **b == 0).count();

        if odd_zeros * 2 >= pair_count {
            endian = Some(Utf16Endian::Le);
        } else if even_zeros * 2 >= pair_count {
            endian = Some(Utf16Endian::Be);
        } else {
            return None;
        }
    }

    let mut units = Vec::with_capacity(bytes.len() / 2);
    match endian.expect("utf16 endianness should be detected") {
        Utf16Endian::Le => {
            for pair in bytes.chunks_exact(2) {
                units.push(u16::from_le_bytes([pair[0], pair[1]]));
            }
        }
        Utf16Endian::Be => {
            for pair in bytes.chunks_exact(2) {
                units.push(u16::from_be_bytes([pair[0], pair[1]]));
            }
        }
    }

    while matches!(units.last(), Some(0x000d | 0x000a)) {
        units.pop();
    }
    if units.is_empty() {
        return Some(String::new());
    }

    let mut out = String::with_capacity(units.len());
    for ch in std::char::decode_utf16(units.into_iter()) {
        let ch = ch.ok()?;
        out.push(ch);
    }
    Some(out)
}

fn is_transient_action_sink_busy(err: &std::io::Error) -> bool {
    if !matches!(err.kind(), ErrorKind::PermissionDenied | ErrorKind::WouldBlock) {
        return false;
    }

    #[cfg(windows)]
    {
        matches!(err.raw_os_error(), Some(5 | 32 | 33))
    }

    #[cfg(not(windows))]
    {
        false
    }
}

fn validate_payload(spec: &ActionSpec, payload: &str) -> std::result::Result<(), &'static str> {
    if let Some(max) = spec.max_payload_bytes {
        if payload.len() > max {
            return Err("EMSGSIZE");
        }
    }

    if payload.trim().is_empty() {
        return Err(ERR_INVALID_ARGUMENT);
    }

    match spec.input_mode {
        InputMode::Json => {
            serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_PAYLOAD)?;
            Ok(())
        }
    }
}

fn action_template_matches(template: &str, rel_path: &str) -> bool {
    let template = template.trim_matches('/');
    let rel_path = rel_path.trim_matches('/');
    if template.is_empty() || rel_path.is_empty() {
        return false;
    }

    let template_segments: Vec<&str> = template.split('/').collect();
    let rel_segments: Vec<&str> = rel_path.split('/').collect();
    if template_segments.len() != rel_segments.len() {
        return false;
    }

    template_segments
        .iter()
        .zip(rel_segments.iter())
        .all(|(t, r)| {
            if is_template_placeholder(t) {
                !r.is_empty()
            } else {
                *t == *r
            }
        })
}

fn is_template_placeholder(segment: &str) -> bool {
    segment.len() >= 3 && segment.starts_with('{') && segment.ends_with('}')
}

fn is_safe_action_rel_path(rel_path: &str) -> bool {
    let path = rel_path.trim_matches('/');
    if path.is_empty() {
        return false;
    }

    path.split('/').all(is_safe_segment)
}

fn is_safe_segment(segment: &str) -> bool {
    if segment.is_empty() || segment == "." || segment == ".." {
        return false;
    }
    if segment.contains('\\') || segment.contains('\0') {
        return false;
    }
    if is_drive_letter_segment(segment) {
        return false;
    }
    if is_windows_reserved_name(segment) {
        return false;
    }
    if segment.len() > MAX_SEGMENT_BYTES {
        return false;
    }

    segment.chars().all(|c| ALLOWED_SEGMENT_CHARS.contains(c))
}

fn is_drive_letter_segment(segment: &str) -> bool {
    segment.len() >= 2
        && segment.as_bytes()[0].is_ascii_alphabetic()
        && segment.as_bytes()[1] == b':'
}

fn is_windows_reserved_name(segment: &str) -> bool {
    let upper = segment.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn extract_client_token(payload: &str) -> Option<String> {
    let json = serde_json::from_str::<JsonValue>(payload).ok()?;
    json.get("client_token")
        .and_then(|v| v.as_str())
        .map(ToOwned::to_owned)
}

fn parse_paging_request(payload: &str) -> std::result::Result<PagingRequest, &'static str> {
    let json = serde_json::from_str::<JsonValue>(payload).map_err(|_| ERR_INVALID_ARGUMENT)?;
    let handle_id = json
        .get("handle_id")
        .and_then(|v| v.as_str())
        .ok_or(ERR_INVALID_ARGUMENT)?;
    let session_id = json
        .get("session_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned);
    Ok(PagingRequest {
        handle_id: handle_id.trim().to_string(),
        session_id,
    })
}

fn parse_rfc3339_timestamp(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.timestamp())
}

fn normalize_runtime_handle_id(handle_id: &str) -> String {
    deterministic_shorten_segment(handle_id, MAX_SEGMENT_BYTES)
}

fn deterministic_shorten_segment(segment: &str, max_bytes: usize) -> String {
    if segment.len() <= max_bytes {
        return segment.to_string();
    }

    let hash = format!("{:016x}", fnv1a_64(segment.as_bytes()));
    let suffix = format!("_{}", hash);
    let prefix_budget = max_bytes.saturating_sub(suffix.len());

    let mut prefix = String::new();
    let mut used = 0usize;
    for ch in segment.chars() {
        let ch_len = ch.len_utf8();
        if used + ch_len > prefix_budget {
            break;
        }
        prefix.push(ch);
        used += ch_len;
    }

    if prefix.is_empty() {
        return hash;
    }

    prefix.push_str(&suffix);
    prefix
}

fn fnv1a_64(input: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;

    let mut hash = OFFSET_BASIS;
    for byte in input {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

fn is_handle_format_valid(handle_id: &str) -> bool {
    if !handle_id.starts_with("ph_") {
        return false;
    }
    handle_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use super::{
        boundary_probe_from_bytes, decode_jsonl_line, deterministic_shorten_segment, extract_client_token,
        is_handle_format_valid, normalize_runtime_handle_id, parse_paging_request, validate_payload,
        ActionSpec, ExecutionMode, InputMode, MAX_SEGMENT_BYTES,
    };

    fn make_spec() -> ActionSpec {
        ActionSpec {
            template: "contacts/{contact_id}/send_message.act".to_string(),
            input_mode: InputMode::Json,
            execution_mode: ExecutionMode::Inline,
            max_payload_bytes: Some(8192),
        }
    }

    #[test]
    fn parse_handle_json_mode() {
        let req = parse_paging_request(r#"{"handle_id":"ph_abc"}"#).expect("expected handle");
        assert_eq!(req.handle_id, "ph_abc");
        assert_eq!(req.session_id, None);
    }

    #[test]
    fn parse_handle_json_with_session_mode() {
        let req = parse_paging_request(r#"{"handle_id":"ph_abc","session_id":"sess-other"}"#)
            .expect("expected handle");
        assert_eq!(req.handle_id, "ph_abc");
        assert_eq!(req.session_id.as_deref(), Some("sess-other"));
    }

    #[test]
    fn extract_token_from_json() {
        let token = extract_client_token(r#"{"client_token":"x-1"}"#).expect("token missing");
        assert_eq!(token, "x-1");
    }

    #[test]
    fn handle_format_validation() {
        assert!(is_handle_format_valid("ph_7f2c"));
        assert!(!is_handle_format_valid("bad/handle"));
    }

    #[test]
    fn normalize_runtime_handle_id_keeps_short_value() {
        let handle = "ph_short_handle";
        assert_eq!(normalize_runtime_handle_id(handle), handle);
    }

    #[test]
    fn deterministic_shorten_is_bounded_and_stable() {
        let long_handle = format!("ph_{}", "a".repeat(500));
        let shortened_a = deterministic_shorten_segment(&long_handle, MAX_SEGMENT_BYTES);
        let shortened_b = deterministic_shorten_segment(&long_handle, MAX_SEGMENT_BYTES);

        assert_eq!(shortened_a, shortened_b);
        assert!(shortened_a.starts_with("ph_"));
        assert!(shortened_a.as_bytes().len() <= MAX_SEGMENT_BYTES);
    }

    #[test]
    fn json_payload_validation_accepts_object() {
        let spec = make_spec();
        let payload = r#"{"text":"hello line 1\nhello line 2"}"#;
        assert!(validate_payload(&spec, payload).is_ok());
    }

    #[test]
    fn json_payload_validation_rejects_non_json() {
        let spec = make_spec();
        let payload = "hello line 1";
        assert!(validate_payload(&spec, payload).is_err());
    }

    #[test]
    fn decode_jsonl_line_supports_utf8_bom_on_first_line() {
        let bytes = b"\xEF\xBB\xBF{\"text\":\"hello\"}\n";
        let line = decode_jsonl_line(bytes, true).expect("decode should succeed");
        assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
    }

    #[test]
    fn decode_jsonl_line_trims_crlf() {
        let bytes = b"{\"text\":\"hello\"}\r\n";
        let line = decode_jsonl_line(bytes, false).expect("decode should succeed");
        assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
    }

    #[test]
    fn decode_jsonl_line_rejects_invalid_utf8() {
        let bytes = [0xFF, 0xFF, b'\n'];
        assert!(decode_jsonl_line(&bytes, false).is_err());
    }

    #[test]
    fn decode_jsonl_line_supports_utf16le_ps5_redirection() {
        let bytes = vec![
            0x7b, 0x00, 0x22, 0x00, 0x74, 0x00, 0x65, 0x00, 0x78, 0x00, 0x74, 0x00, 0x22,
            0x00, 0x3a, 0x00, 0x22, 0x00, 0x68, 0x00, 0x65, 0x00, 0x6c, 0x00, 0x6c, 0x00,
            0x6f, 0x00, 0x22, 0x00, 0x7d, 0x00, 0x0d, 0x00, 0x0a,
        ];
        let line = decode_jsonl_line(&bytes, false).expect("decode should succeed");
        assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
    }

    #[test]
    fn decode_jsonl_line_supports_utf16le_bom() {
        let bytes = vec![
            0xff, 0xfe, 0x7b, 0x00, 0x22, 0x00, 0x6f, 0x00, 0x6b, 0x00, 0x22, 0x00, 0x3a,
            0x00, 0x74, 0x00, 0x72, 0x00, 0x75, 0x00, 0x65, 0x00, 0x7d, 0x00, 0x0a,
        ];
        let line = decode_jsonl_line(&bytes, true).expect("decode should succeed");
        assert_eq!(line.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn parse_handle_rejects_non_json_payload() {
        assert!(parse_paging_request("ph_7f2c\n").is_err());
    }

    #[test]
    fn boundary_probe_is_stable() {
        let bytes = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let probe_a = boundary_probe_from_bytes(bytes, bytes.len() as u64).expect("probe");
        let probe_b = boundary_probe_from_bytes(bytes, bytes.len() as u64).expect("probe");
        assert_eq!(probe_a, probe_b);
    }
}
