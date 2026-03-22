use agentfs_sdk::{
    AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
    AdapterInputModeV1, AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1,
    DemoAppAdapterV1, RequestContextV1,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, ErrorKind};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};
use uuid::Uuid;

mod bridge_resilience;
mod action_dispatcher;
mod events;
mod grpc_bridge_adapter;
mod http_bridge_adapter;
mod journal;
mod paging;
mod recovery;

use bridge_resilience::BridgeRuntimeOptions;
use grpc_bridge_adapter::GrpcBridgeAdapterV1;
use http_bridge_adapter::HttpBridgeAdapterV1;
use journal::SnapshotExpandJournalEntry;

const DEFAULT_RETENTION_HINT_SEC: i64 = 86400;
const MIN_POLL_MS: u64 = 50;
const ACTION_CURSORS_FILENAME: &str = "action-cursors.res.json";
const ACTION_CURSOR_PROBE_WINDOW: usize = 64;
const MAX_RECOVERY_LINES: usize = 32;
const MAX_RECOVERY_BYTES: usize = 65536;
const DEFAULT_SNAPSHOT_MAX_MATERIALIZED_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_SNAPSHOT_READ_THROUGH_TIMEOUT_MS: u64 = 10_000;
const SNAPSHOT_EXPAND_DELAY_ENV: &str = "APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS";
const SNAPSHOT_FORCE_EXPAND_ON_REFRESH_ENV: &str = "APPFS_V2_SNAPSHOT_REFRESH_FORCE_EXPAND";
const SNAPSHOT_COALESCE_WINDOW_ENV: &str = "APPFS_V2_SNAPSHOT_COALESCE_WINDOW_MS";
const SNAPSHOT_PUBLISH_DELAY_ENV: &str = "APPFS_V2_SNAPSHOT_PUBLISH_DELAY_MS";
const DEFAULT_SNAPSHOT_COALESCE_WINDOW_MS: u64 = 120;
const SNAPSHOT_EXPAND_JOURNAL_FILENAME: &str = "snapshot-expand.state.res.json";

const ERR_PAGER_HANDLE_NOT_FOUND: &str = "PAGER_HANDLE_NOT_FOUND";
const ERR_PAGER_HANDLE_EXPIRED: &str = "PAGER_HANDLE_EXPIRED";
const ERR_PAGER_HANDLE_CLOSED: &str = "PAGER_HANDLE_CLOSED";
const ERR_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
const ERR_INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
const ERR_INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
const ERR_SNAPSHOT_TOO_LARGE: &str = "SNAPSHOT_TOO_LARGE";
const ERR_CACHE_MISS_EXPAND_FAILED: &str = "CACHE_MISS_EXPAND_FAILED";
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

#[derive(Debug, Clone)]
struct SnapshotSpec {
    template: String,
    max_materialized_bytes: usize,
    read_through_timeout_ms: u64,
    on_timeout: SnapshotOnTimeoutPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotOnTimeoutPolicy {
    ReturnStale,
    Fail,
}

impl SnapshotOnTimeoutPolicy {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReturnStale => "return_stale",
            Self::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SnapshotCacheState {
    Cold,
    Warming,
    Hot,
    Error,
}

impl SnapshotCacheState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warming => "warming",
            Self::Hot => "hot",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone)]
struct ManifestContract {
    action_specs: Vec<ActionSpec>,
    snapshot_specs: Vec<SnapshotSpec>,
    requires_paging_controls: bool,
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
    output_mode: Option<String>,
    #[serde(default)]
    input_mode: Option<String>,
    #[serde(default)]
    execution_mode: Option<String>,
    #[serde(default)]
    max_payload_bytes: Option<usize>,
    #[serde(default)]
    paging: Option<ManifestPagingDoc>,
    #[serde(default)]
    snapshot: Option<ManifestSnapshotDoc>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestPagingDoc {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ManifestSnapshotDoc {
    #[serde(default)]
    max_materialized_bytes: Option<usize>,
    #[serde(default)]
    read_through_timeout_ms: Option<u64>,
    #[serde(default)]
    on_timeout: Option<String>,
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
    snapshot_specs: Vec<SnapshotSpec>,
    events_path: PathBuf,
    cursor_path: PathBuf,
    replay_dir: PathBuf,
    jobs_path: PathBuf,
    action_cursors_path: PathBuf,
    snapshot_expand_journal_path: PathBuf,
    cursor: CursorState,
    next_seq: i64,
    action_cursors: HashMap<String, ActionCursorState>,
    handles: HashMap<String, PagingHandle>,
    handle_aliases: HashMap<String, String>,
    snapshot_states: HashMap<String, SnapshotCacheState>,
    snapshot_recent_expands: HashMap<String, Instant>,
    snapshot_expand_journal: HashMap<String, SnapshotExpandJournalEntry>,
    streaming_jobs: Vec<StreamingJob>,
    actionline_v2_strict: bool,
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

    fn initialize_snapshot_states(&mut self) {
        for spec in &self.snapshot_specs {
            let rel = spec.template.clone();
            let abs = self.app_dir.join(&rel);
            let state = match fs::metadata(abs) {
                Ok(_) => SnapshotCacheState::Hot,
                Err(_) => SnapshotCacheState::Cold,
            };
            self.snapshot_states.insert(rel, state);
        }
    }

    fn snapshot_state_for(&self, resource_rel: &str) -> SnapshotCacheState {
        self.snapshot_states
            .get(resource_rel)
            .copied()
            .unwrap_or(SnapshotCacheState::Cold)
    }

    fn transition_snapshot_state(&mut self, resource_rel: &str, next: SnapshotCacheState) {
        let prev = self.snapshot_state_for(resource_rel);
        if prev != next {
            eprintln!(
                "[cache] state resource=/{} from={} to={}",
                resource_rel,
                prev.as_str(),
                next.as_str()
            );
        }
        self.snapshot_states.insert(resource_rel.to_string(), next);
    }

    fn mark_snapshot_recent_expand(&mut self, resource_rel: &str) {
        self.snapshot_recent_expands
            .insert(resource_rel.to_string(), Instant::now());
    }

    fn clear_snapshot_recent_expand(&mut self, resource_rel: &str) {
        self.snapshot_recent_expands.remove(resource_rel);
    }

    fn is_coalesced_snapshot_hit(&mut self, resource_rel: &str) -> bool {
        let window = Duration::from_millis(snapshot_coalesce_window_ms().max(1));
        match self.snapshot_recent_expands.get(resource_rel).copied() {
            Some(expanded_at) if expanded_at.elapsed() <= window => true,
            Some(_) => {
                self.snapshot_recent_expands.remove(resource_rel);
                false
            }
            None => false,
        }
    }

    fn snapshot_expand_temp_rel_path(&self, resource_rel: &str) -> String {
        let mut sanitized = String::with_capacity(resource_rel.len() + 16);
        for ch in resource_rel.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                sanitized.push(ch);
            } else {
                sanitized.push('_');
            }
        }
        if sanitized.is_empty() {
            sanitized.push_str("snapshot");
        }
        if sanitized.len() > 160 {
            sanitized = deterministic_shorten_segment(&sanitized, 160);
        }
        format!("/_stream/snapshot-expand-tmp/{}.pending.jsonl", sanitized)
    }

    fn snapshot_expand_temp_abs_path(&self, resource_rel: &str) -> PathBuf {
        self.app_dir.join(
            self.snapshot_expand_temp_rel_path(resource_rel)
                .trim_start_matches('/'),
        )
    }

    fn validate_stale_snapshot_jsonl(
        &self,
        stale_abs: &Path,
    ) -> std::result::Result<usize, String> {
        let file = fs::File::open(stale_abs).map_err(|err| format!("open_failed: {err}"))?;
        let mut reader = std::io::BufReader::new(file);
        let mut line_buf = Vec::new();
        let mut valid_lines = 0usize;
        let mut line_no = 0usize;

        loop {
            line_buf.clear();
            let read = reader
                .read_until(b'\n', &mut line_buf)
                .map_err(|err| format!("read_failed line={} err={err}", line_no + 1))?;
            if read == 0 {
                break;
            }
            line_no += 1;

            let Some(line) = decode_jsonl_line(&line_buf, line_no == 1)
                .map_err(|err| format!("decode_failed line={line_no} err={err}"))?
            else {
                continue;
            };

            let value: JsonValue = serde_json::from_str(&line)
                .map_err(|err| format!("parse_failed line={line_no} err={err}"))?;
            if !value.is_object() {
                return Err(format!("non_object_json line={line_no}"));
            }
            valid_lines += 1;
        }

        if valid_lines == 0 {
            return Err("empty_or_blank_snapshot".to_string());
        }

        Ok(valid_lines)
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

    fn handle_snapshot_refresh(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_path: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let Some(resource_rel) = normalize_resource_rel_path(resource_path) else {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "resource_path must be a non-empty .res.jsonl path",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        };
        if !is_safe_resource_rel_path(&resource_rel) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "unsafe snapshot resource_path",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let Some(snapshot_spec) = self.find_snapshot_spec(&resource_rel).cloned() else {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "snapshot resource is not declared in manifest",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        };

        let resource_abs = self.app_dir.join(&resource_rel);
        let force_expand = snapshot_force_expand_on_refresh();
        let (size_bytes, coalesced) = match fs::metadata(&resource_abs) {
            Ok(meta) => {
                let size_bytes = meta.len() as usize;
                if force_expand {
                    eprintln!(
                        "[cache] refresh forcing expand resource=/{} existing_bytes={size_bytes}",
                        resource_rel
                    );
                    self.clear_snapshot_recent_expand(&resource_rel);
                    return self.expand_snapshot_cache_read_through(
                        action_path,
                        request_id,
                        &resource_rel,
                        &snapshot_spec,
                        "forced_refresh",
                        client_token,
                    );
                }
                let coalesced = self.is_coalesced_snapshot_hit(&resource_rel);
                self.transition_snapshot_state(&resource_rel, SnapshotCacheState::Hot);
                eprintln!("[cache] hit resource=/{} bytes={size_bytes}", resource_rel);
                if coalesced {
                    eprintln!(
                        "[cache] coalesced concurrent miss resource=/{}",
                        resource_rel
                    );
                }
                (size_bytes, coalesced)
            }
            Err(err) => {
                let reason = match err.kind() {
                    ErrorKind::NotFound => format!("resource_missing: {err}"),
                    _ => format!("resource_unreadable: {err}"),
                };
                self.clear_snapshot_recent_expand(&resource_rel);
                return self.expand_snapshot_cache_read_through(
                    action_path,
                    request_id,
                    &resource_rel,
                    &snapshot_spec,
                    &reason,
                    client_token,
                );
            }
        };

        if size_bytes > snapshot_spec.max_materialized_bytes {
            let resource_path = format!("/{}", resource_rel);
            eprintln!(
                "[cache] snapshot_too_large resource={} size={} max={}",
                resource_path, size_bytes, snapshot_spec.max_materialized_bytes
            );
            self.emit_snapshot_too_large(
                action_path,
                request_id,
                &resource_path,
                size_bytes,
                snapshot_spec.max_materialized_bytes,
                "existing_cache",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        self.emit_event(
            action_path,
            request_id,
            "action.completed",
            Some(json!({
                "refreshed": true,
                "resource_path": format!("/{}", resource_rel),
                "bytes": size_bytes,
                "max_materialized_bytes": snapshot_spec.max_materialized_bytes,
                "cached": true,
                "coalesced": coalesced,
                "state": self.snapshot_state_for(&resource_rel).as_str(),
                "generated_at": Utc::now().to_rfc3339(),
            })),
            None,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
    }

    fn expand_snapshot_cache_read_through(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_rel: &str,
        snapshot_spec: &SnapshotSpec,
        reason: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let resource_path = format!("/{}", resource_rel);
        self.clear_snapshot_recent_expand(resource_rel);
        self.transition_snapshot_state(resource_rel, SnapshotCacheState::Cold);
        self.transition_snapshot_state(resource_rel, SnapshotCacheState::Warming);
        let temp_artifact = self.snapshot_expand_temp_rel_path(resource_rel);
        let temp_artifact_abs = self.snapshot_expand_temp_abs_path(resource_rel);
        self.update_snapshot_expand_journal(
            resource_rel,
            "warming",
            request_id,
            Some(temp_artifact.clone()),
        )?;

        eprintln!(
            "[cache] miss, expanding resource={} phase=read_through reason={} timeout_ms={} on_timeout={}",
            resource_path,
            reason,
            snapshot_spec.read_through_timeout_ms,
            snapshot_spec.on_timeout.as_str()
        );

        let started_at = Instant::now();
        let simulated_delay_ms = snapshot_expand_delay_ms();
        if simulated_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(simulated_delay_ms));
        }

        if simulated_delay_ms > snapshot_spec.read_through_timeout_ms {
            let mut stale_unavailable_reason: Option<&'static str> = None;
            let mut stale_unavailable_detail: Option<String> = None;
            if matches!(
                snapshot_spec.on_timeout,
                SnapshotOnTimeoutPolicy::ReturnStale
            ) {
                let stale_abs = self.app_dir.join(resource_rel);
                match fs::metadata(&stale_abs) {
                    Ok(meta) => {
                        let stale_bytes = meta.len() as usize;
                        if stale_bytes <= snapshot_spec.max_materialized_bytes {
                            match self.validate_stale_snapshot_jsonl(&stale_abs) {
                                Ok(valid_lines) => {
                                    self.transition_snapshot_state(
                                        resource_rel,
                                        SnapshotCacheState::Hot,
                                    );
                                    self.clear_snapshot_recent_expand(resource_rel);
                                    self.clear_snapshot_expand_journal_entry(resource_rel)?;
                                    eprintln!(
                                        "[cache] timeout_return_stale resource={} bytes={} timeout_ms={} elapsed_ms={} health=valid valid_lines={}",
                                        resource_path,
                                        stale_bytes,
                                        snapshot_spec.read_through_timeout_ms,
                                        simulated_delay_ms,
                                        valid_lines
                                    );
                                    self.emit_event(
                                        &resource_path,
                                        request_id,
                                        "cache.expand",
                                        Some(json!({
                                            "path": resource_path.clone(),
                                            "phase": "timeout",
                                            "state": self.snapshot_state_for(resource_rel).as_str(),
                                            "failure_reason": "timeout",
                                            "timeout_ms": snapshot_spec.read_through_timeout_ms,
                                            "elapsed_ms": simulated_delay_ms,
                                            "on_timeout": snapshot_spec.on_timeout.as_str(),
                                            "fallback": "return_stale",
                                            "stale_health": "valid",
                                            "stale_valid_lines": valid_lines,
                                        })),
                                        None,
                                        client_token.clone(),
                                    )?;
                                    self.emit_event(
                                        &resource_path,
                                        request_id,
                                        "cache.stale",
                                        Some(json!({
                                            "path": resource_path.clone(),
                                            "reason": "timeout",
                                            "bytes": stale_bytes,
                                            "max_size": snapshot_spec.max_materialized_bytes,
                                            "state": self.snapshot_state_for(resource_rel).as_str(),
                                            "stale_health": "valid",
                                            "stale_valid_lines": valid_lines,
                                        })),
                                        None,
                                        client_token.clone(),
                                    )?;
                                    self.emit_event(
                                        action_path,
                                        request_id,
                                        "action.completed",
                                        Some(json!({
                                            "refreshed": false,
                                            "resource_path": resource_path,
                                            "bytes": stale_bytes,
                                            "max_materialized_bytes": snapshot_spec.max_materialized_bytes,
                                            "cached": true,
                                            "coalesced": false,
                                            "stale": true,
                                            "degrade_reason": "timeout_return_stale",
                                            "stale_health": "valid",
                                            "stale_valid_lines": valid_lines,
                                            "state": self.snapshot_state_for(resource_rel).as_str(),
                                            "generated_at": Utc::now().to_rfc3339(),
                                        })),
                                        None,
                                        client_token,
                                    )?;
                                    return Ok(ProcessOutcome::Consumed);
                                }
                                Err(health_reason) => {
                                    stale_unavailable_reason = Some("stale_cache_unhealthy");
                                    stale_unavailable_detail = Some(health_reason.clone());
                                    eprintln!(
                                        "[cache] timeout_return_stale unavailable resource={} reason=stale_cache_unhealthy detail={}",
                                        resource_path, health_reason
                                    );
                                }
                            }
                        } else {
                            stale_unavailable_reason = Some("stale_cache_too_large");
                            stale_unavailable_detail = Some(format!(
                                "size={} max={}",
                                stale_bytes, snapshot_spec.max_materialized_bytes
                            ));
                            eprintln!(
                                "[cache] timeout_return_stale unavailable resource={} reason=stale_cache_too_large size={} max={}",
                                resource_path, stale_bytes, snapshot_spec.max_materialized_bytes
                            );
                        }
                    }
                    Err(_) => {
                        stale_unavailable_reason = Some("no_stale_cache");
                        eprintln!(
                            "[cache] timeout_return_stale unavailable resource={} reason=no_stale_cache",
                            resource_path
                        );
                    }
                }
            }
            self.transition_snapshot_state(resource_rel, SnapshotCacheState::Error);
            self.clear_snapshot_recent_expand(resource_rel);
            self.emit_event(
                &resource_path,
                request_id,
                "cache.expand",
                Some(json!({
                    "path": resource_path,
                    "phase": "failed",
                    "state": self.snapshot_state_for(resource_rel).as_str(),
                    "failure_reason": "timeout",
                    "timeout_ms": snapshot_spec.read_through_timeout_ms,
                    "on_timeout": snapshot_spec.on_timeout.as_str(),
                    "stale_reason": stale_unavailable_reason,
                    "stale_detail": stale_unavailable_detail,
                })),
                None,
                client_token.clone(),
            )?;
            let timeout_reason = if let Some(reason) = stale_unavailable_reason {
                let detail = stale_unavailable_detail.as_deref().unwrap_or("none");
                format!(
                    "expand_timeout elapsed_ms={} timeout_ms={} on_timeout={} stale_reason={} stale_detail={}",
                    simulated_delay_ms,
                    snapshot_spec.read_through_timeout_ms,
                    snapshot_spec.on_timeout.as_str(),
                    reason,
                    detail
                )
            } else {
                format!(
                    "expand_timeout elapsed_ms={} timeout_ms={} on_timeout={}",
                    simulated_delay_ms,
                    snapshot_spec.read_through_timeout_ms,
                    snapshot_spec.on_timeout.as_str()
                )
            };
            self.clear_snapshot_expand_journal_entry(resource_rel)?;
            return self.handle_snapshot_cache_expand_hook(
                action_path,
                request_id,
                resource_rel,
                "timeout",
                &timeout_reason,
                client_token,
            );
        }

        let expanded_jsonl = match self.fetch_snapshot_jsonl_from_upstream(resource_rel) {
            Ok(content) => content,
            Err(upstream_reason) => {
                self.transition_snapshot_state(resource_rel, SnapshotCacheState::Error);
                self.clear_snapshot_recent_expand(resource_rel);
                self.emit_event(
                    &resource_path,
                    request_id,
                    "cache.expand",
                    Some(json!({
                        "path": resource_path,
                        "phase": "failed",
                        "state": self.snapshot_state_for(resource_rel).as_str(),
                        "failure_reason": upstream_reason,
                    })),
                    None,
                    client_token.clone(),
                )?;
                self.clear_snapshot_expand_journal_entry(resource_rel)?;
                return self.handle_snapshot_cache_expand_hook(
                    action_path,
                    request_id,
                    resource_rel,
                    "expand_hook",
                    "upstream_connector_unavailable",
                    client_token,
                );
            }
        };

        let size_bytes = expanded_jsonl.len();
        if size_bytes > snapshot_spec.max_materialized_bytes {
            self.transition_snapshot_state(resource_rel, SnapshotCacheState::Error);
            self.clear_snapshot_recent_expand(resource_rel);
            eprintln!(
                "[cache] snapshot_too_large resource={} size={} max={}",
                resource_path, size_bytes, snapshot_spec.max_materialized_bytes
            );
            self.emit_event(
                &resource_path,
                request_id,
                "cache.expand",
                Some(json!({
                    "path": resource_path,
                    "phase": "failed",
                    "state": self.snapshot_state_for(resource_rel).as_str(),
                    "failure_reason": "snapshot_too_large",
                    "size": size_bytes,
                    "max_size": snapshot_spec.max_materialized_bytes,
                })),
                None,
                client_token.clone(),
            )?;
            self.emit_snapshot_too_large(
                action_path,
                request_id,
                &resource_path,
                size_bytes,
                snapshot_spec.max_materialized_bytes,
                "expand_publish",
                client_token,
            )?;
            self.clear_snapshot_expand_journal_entry(resource_rel)?;
            return Ok(ProcessOutcome::Consumed);
        }

        self.materialize_snapshot_file(
            resource_rel,
            &expanded_jsonl,
            &temp_artifact_abs,
            request_id,
        )?;
        self.transition_snapshot_state(resource_rel, SnapshotCacheState::Hot);
        self.mark_snapshot_recent_expand(resource_rel);
        self.clear_snapshot_expand_journal_entry(resource_rel)?;
        let elapsed_ms = started_at.elapsed().as_millis() as u64;

        eprintln!(
            "[cache] expanded resource={} bytes={} state=hot elapsed_ms={elapsed_ms}",
            resource_path, size_bytes
        );

        self.emit_event(
            &resource_path,
            request_id,
            "cache.expand",
            Some(json!({
                "path": resource_path,
                "phase": "completed",
                "state": self.snapshot_state_for(resource_rel).as_str(),
                "bytes": size_bytes,
                "elapsed_ms": elapsed_ms,
                "upstream_calls": 1,
            })),
            None,
            client_token.clone(),
        )?;

        self.emit_event(
            action_path,
            request_id,
            "action.completed",
            Some(json!({
                "refreshed": true,
                "resource_path": resource_path,
                "bytes": size_bytes,
                "max_materialized_bytes": snapshot_spec.max_materialized_bytes,
                "cached": false,
                "coalesced": false,
                "state": self.snapshot_state_for(resource_rel).as_str(),
                "generated_at": Utc::now().to_rfc3339(),
            })),
            None,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
    }

    fn handle_snapshot_cache_expand_hook(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_rel: &str,
        phase: &str,
        reason: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let resource_path = format!("/{}", resource_rel);
        eprintln!(
            "[cache] miss, expanding resource={} phase={} reason={}",
            resource_path, phase, reason
        );
        eprintln!(
            "[cache] expand failed resource={} phase={} reason={}",
            resource_path, phase, reason
        );
        self.emit_failed_with_retryable(
            action_path,
            request_id,
            ERR_CACHE_MISS_EXPAND_FAILED,
            &format!(
                "snapshot read-through skeleton not materialized yet: resource={} phase={} reason={}",
                resource_path, phase, reason
            ),
            true,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
    }

    fn fetch_snapshot_jsonl_from_upstream(
        &self,
        resource_rel: &str,
    ) -> std::result::Result<String, String> {
        if resource_rel != "chats/chat-001/messages.res.jsonl"
            && resource_rel != "chats/chat-oversize/messages.res.jsonl"
        {
            return Err("connector has no expansion mapping for resource".to_string());
        }

        eprintln!(
            "[cache.expand] fetch_snapshot_chunk resource=/{}",
            resource_rel
        );
        let mut out = String::new();
        for idx in 1..=100u32 {
            let second = (idx - 1) % 60;
            out.push_str(&format!(
                r#"{{"id":"m{idx:03}","text":"snapshot file expanded message {idx}","ts":"2026-03-20T00:00:{second:02}Z"}}"#
            ));
            out.push('\n');
        }
        Ok(out)
    }

    fn materialize_snapshot_file(
        &mut self,
        resource_rel: &str,
        content: &str,
        temp_path: &Path,
        request_id: &str,
    ) -> Result<()> {
        let abs = self.app_dir.join(resource_rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create snapshot parent directory {}",
                    parent.display()
                )
            })?;
        }

        if let Some(parent) = temp_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create snapshot temp parent directory {}",
                    parent.display()
                )
            })?;
        }

        fs::write(temp_path, content).with_context(|| {
            format!(
                "Failed to write snapshot expansion temp file {}",
                temp_path.display()
            )
        })?;
        let temp_rel = format!(
            "/{}",
            temp_path
                .strip_prefix(&self.app_dir)
                .unwrap_or(temp_path)
                .to_string_lossy()
                .replace('\\', "/")
        );
        self.update_snapshot_expand_journal(
            resource_rel,
            "publishing",
            request_id,
            Some(temp_rel),
        )?;

        let publish_delay_ms = snapshot_publish_delay_ms();
        if publish_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(publish_delay_ms));
        }

        if abs.exists() {
            fs::remove_file(&abs).with_context(|| {
                format!("Failed to remove stale snapshot file {}", abs.display())
            })?;
        }
        fs::rename(temp_path, &abs).with_context(|| {
            format!(
                "Failed to publish snapshot expansion from {} to {}",
                temp_path.display(),
                abs.display()
            )
        })?;
        Ok(())
    }

    fn find_action_spec(&self, rel_path: &str) -> Option<&ActionSpec> {
        self.action_specs
            .iter()
            .filter(|spec| action_template_matches(&spec.template, rel_path))
            .max_by_key(|spec| template_specificity(&spec.template))
    }

    fn find_snapshot_spec(&self, rel_path: &str) -> Option<&SnapshotSpec> {
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

fn has_odd_unescaped_quotes(s: &str) -> bool {
    let mut escaped = false;
    let mut quote_count = 0usize;
    for ch in s.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            quote_count += 1;
        }
    }
    quote_count % 2 == 1
}

fn recover_multiline_json_payload(
    bytes: &[u8],
    initial_payload: &str,
    initial_line_end: usize,
    spec: &ActionSpec,
) -> Option<(String, usize, usize)> {
    if !has_odd_unescaped_quotes(initial_payload) {
        return None;
    }

    let max_recovery_bytes = spec
        .max_payload_bytes
        .unwrap_or(MAX_RECOVERY_BYTES)
        .min(MAX_RECOVERY_BYTES);
    if initial_payload.len() >= max_recovery_bytes {
        return None;
    }

    let mut merged = initial_payload.to_string();
    let mut consumed_lines = 1usize;
    let mut next_position = initial_line_end;

    while consumed_lines < MAX_RECOVERY_LINES {
        while next_position < bytes.len() && bytes[next_position] == 0 {
            next_position += 1;
        }
        if next_position >= bytes.len() {
            break;
        }

        let Some(next_rel_idx) = bytes[next_position..].iter().position(|b| *b == b'\n') else {
            break;
        };
        let next_end = next_position + next_rel_idx + 1;
        let next_line_bytes = &bytes[next_position..next_end];
        let next_fragment = match decode_jsonl_line(next_line_bytes, next_position == 0) {
            Ok(Some(value)) => value,
            Ok(None) => String::new(),
            Err(_) => break,
        };

        let candidate_len = merged
            .len()
            .saturating_add(2)
            .saturating_add(next_fragment.len());
        if candidate_len > max_recovery_bytes {
            break;
        }

        merged.push_str("\\n");
        merged.push_str(&next_fragment);
        consumed_lines += 1;

        if serde_json::from_str::<JsonValue>(&merged).is_ok() {
            return Some((merged, next_end, consumed_lines));
        }
        next_position = next_end;
    }

    None
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

    if !bytes.len().is_multiple_of(2) {
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
    for ch in std::char::decode_utf16(units) {
        let ch = ch.ok()?;
        out.push(ch);
    }
    Some(out)
}

fn is_transient_action_sink_busy(err: &std::io::Error) -> bool {
    if !matches!(
        err.kind(),
        ErrorKind::PermissionDenied | ErrorKind::WouldBlock
    ) {
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

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false)
}

fn snapshot_expand_delay_ms() -> u64 {
    std::env::var(SNAPSHOT_EXPAND_DELAY_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn snapshot_publish_delay_ms() -> u64 {
    std::env::var(SNAPSHOT_PUBLISH_DELAY_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

fn snapshot_force_expand_on_refresh() -> bool {
    env_flag_enabled(SNAPSHOT_FORCE_EXPAND_ON_REFRESH_ENV)
}

fn snapshot_coalesce_window_ms() -> u64 {
    std::env::var(SNAPSHOT_COALESCE_WINDOW_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_SNAPSHOT_COALESCE_WINDOW_MS)
}

fn parse_snapshot_on_timeout_policy(value: Option<&str>) -> SnapshotOnTimeoutPolicy {
    let normalized = value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_lowercase());
    match normalized.as_deref() {
        Some("fail") => SnapshotOnTimeoutPolicy::Fail,
        Some("return_stale") | None => SnapshotOnTimeoutPolicy::ReturnStale,
        Some(other) => {
            eprintln!(
                "AppFS adapter unknown snapshot.on_timeout='{other}', defaulting to return_stale"
            );
            SnapshotOnTimeoutPolicy::ReturnStale
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

/// Specificity score for template matching.
///
/// Higher is more specific: prefer templates with more literal segments/bytes
/// over placeholder-heavy templates (for example, prefer
/// `chats/chat-oversize/messages.res.jsonl` over `chats/{chat_id}/messages.res.jsonl`).
fn template_specificity(template: &str) -> (usize, usize, usize) {
    let mut literal_segments = 0usize;
    let mut literal_bytes = 0usize;
    let mut total_segments = 0usize;
    for segment in template.trim_matches('/').split('/') {
        if segment.is_empty() {
            continue;
        }
        total_segments += 1;
        if !is_template_placeholder(segment) {
            literal_segments += 1;
            literal_bytes += segment.len();
        }
    }
    (literal_segments, literal_bytes, total_segments)
}

fn is_template_placeholder(segment: &str) -> bool {
    segment.len() >= 3 && segment.starts_with('{') && segment.ends_with('}')
}

fn is_safe_action_rel_path(rel_path: &str) -> bool {
    let path = rel_path.trim_matches('/');
    if path.is_empty() {
        return false;
    }

    path.ends_with(".act") && path.split('/').all(is_safe_segment)
}

fn is_safe_resource_rel_path(rel_path: &str) -> bool {
    let path = rel_path.trim_matches('/');
    if path.is_empty() {
        return false;
    }
    if !path.ends_with(".res.jsonl") {
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

fn normalize_resource_rel_path(path: &str) -> Option<String> {
    let normalized = path.trim().trim_start_matches('/').replace('\\', "/");
    if normalized.is_empty() {
        return None;
    }
    Some(normalized)
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
    use serde_json::Value;

    use super::{
        action_dispatcher::{
            parse_action_line_v2, parse_paging_request, parse_snapshot_refresh_request,
            validate_submit_payload as validate_payload,
        },
        action_template_matches, boundary_probe_from_bytes, decode_jsonl_line,
        deterministic_shorten_segment, extract_client_token, has_odd_unescaped_quotes,
        is_handle_format_valid, is_safe_resource_rel_path, normalize_resource_rel_path,
        normalize_runtime_handle_id, parse_snapshot_on_timeout_policy, recover_multiline_json_payload,
        template_specificity, ActionSpec,
        ExecutionMode, InputMode, SnapshotOnTimeoutPolicy, ERR_INVALID_ARGUMENT,
        ERR_INVALID_PAYLOAD, MAX_SEGMENT_BYTES,
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
    fn actionline_v2_parses_minimal_valid_line() {
        let parsed = parse_action_line_v2(
            r#"{"version":"2.0","client_token":"msg-001","payload":{"text":"hello"}}"#,
        )
        .expect("expected valid action line");
        assert_eq!(parsed.client_token, "msg-001");
        let payload: Value = serde_json::from_str(&parsed.payload_json).expect("json payload");
        assert_eq!(payload.get("text").and_then(|v| v.as_str()), Some("hello"));
    }

    #[test]
    fn actionline_v2_supports_multiple_jsonl_lines() {
        let bytes = br#"{"version":"2.0","client_token":"msg-001","payload":{"text":"a"}}
{"version":"2.0","client_token":"msg-002","payload":{"text":"b"}}
"#;
        let mut parsed_tokens = Vec::new();
        for (idx, line) in bytes.split(|b| *b == b'\n').enumerate() {
            if line.is_empty() {
                continue;
            }
            let mut line_bytes = line.to_vec();
            line_bytes.push(b'\n');
            let decoded = decode_jsonl_line(&line_bytes, idx == 0)
                .expect("decode")
                .expect("line");
            let parsed = parse_action_line_v2(&decoded).expect("parse");
            parsed_tokens.push(parsed.client_token);
        }
        assert_eq!(parsed_tokens, vec!["msg-001", "msg-002"]);
    }

    #[test]
    fn actionline_v2_rejects_raw_text() {
        let err = parse_action_line_v2("hello world").expect_err("raw text must be rejected");
        assert_eq!(err.code, ERR_INVALID_PAYLOAD);
    }

    #[test]
    fn actionline_v2_rejects_non_object_json() {
        let err = parse_action_line_v2(r#"["not","object"]"#)
            .expect_err("non-object json must be rejected");
        assert_eq!(err.code, ERR_INVALID_ARGUMENT);
    }

    #[test]
    fn actionline_v2_rejects_mode_field() {
        let err = parse_action_line_v2(
            r#"{"version":"2.0","mode":"text","client_token":"x","payload":{"text":"hi"}}"#,
        )
        .expect_err("mode must be rejected");
        assert_eq!(err.code, ERR_INVALID_ARGUMENT);
    }

    #[test]
    fn actionline_v2_rejects_missing_required_fields() {
        let missing_client = parse_action_line_v2(r#"{"version":"2.0","payload":{"text":"hi"}}"#)
            .expect_err("missing client_token");
        assert_eq!(missing_client.code, ERR_INVALID_ARGUMENT);

        let missing_payload = parse_action_line_v2(r#"{"version":"2.0","client_token":"x"}"#)
            .expect_err("missing payload");
        assert_eq!(missing_payload.code, ERR_INVALID_ARGUMENT);
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
            0x7b, 0x00, 0x22, 0x00, 0x74, 0x00, 0x65, 0x00, 0x78, 0x00, 0x74, 0x00, 0x22, 0x00,
            0x3a, 0x00, 0x22, 0x00, 0x68, 0x00, 0x65, 0x00, 0x6c, 0x00, 0x6c, 0x00, 0x6f, 0x00,
            0x22, 0x00, 0x7d, 0x00, 0x0d, 0x00, 0x0a,
        ];
        let line = decode_jsonl_line(&bytes, false).expect("decode should succeed");
        assert_eq!(line.as_deref(), Some("{\"text\":\"hello\"}"));
    }

    #[test]
    fn decode_jsonl_line_supports_utf16le_bom() {
        let bytes = vec![
            0xff, 0xfe, 0x7b, 0x00, 0x22, 0x00, 0x6f, 0x00, 0x6b, 0x00, 0x22, 0x00, 0x3a, 0x00,
            0x74, 0x00, 0x72, 0x00, 0x75, 0x00, 0x65, 0x00, 0x7d, 0x00, 0x0a,
        ];
        let line = decode_jsonl_line(&bytes, true).expect("decode should succeed");
        assert_eq!(line.as_deref(), Some("{\"ok\":true}"));
    }

    #[test]
    fn quote_parity_detects_shell_expanded_fragment() {
        assert!(has_odd_unescaped_quotes("{\"text\":\"hello"));
        assert!(!has_odd_unescaped_quotes("{\"text\":\"hello\\nworld\"}"));
    }

    #[test]
    fn multiline_recovery_merges_three_lines_into_one_json() {
        let spec = make_spec();
        let bytes = b"{\"client_token\":\"ct-ml-1\",\"text\":\"\xe4\xbd\xa0\xe5\xa5\xbd\nhello\n\xe5\xa5\xbd\xef\xbc\x81\"}\n";
        let first_line_end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|idx| idx + 1)
            .expect("newline");
        let first_payload = decode_jsonl_line(&bytes[..first_line_end], true)
            .expect("decode")
            .expect("payload");

        let recovered =
            recover_multiline_json_payload(bytes, &first_payload, first_line_end, &spec)
                .expect("should recover");
        assert_eq!(recovered.2, 3);
        assert_eq!(recovered.1, bytes.len());

        let parsed: Value = serde_json::from_str(&recovered.0).expect("valid json");
        assert_eq!(
            parsed.get("text").and_then(|v| v.as_str()),
            Some("你好\nhello\n好！")
        );
    }

    #[test]
    fn multiline_recovery_does_not_trigger_for_non_multiline_fragment() {
        let spec = make_spec();
        let bytes = b"{\"client_token\":\"ct-good\",\"text\":\"ok\"}\n{\"client_token\":\"ct-next\",\"text\":\"next\"}\n";
        let first_line_end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|idx| idx + 1)
            .expect("newline");
        let first_payload = decode_jsonl_line(&bytes[..first_line_end], true)
            .expect("decode")
            .expect("payload");

        let recovered =
            recover_multiline_json_payload(bytes, &first_payload, first_line_end, &spec);
        assert!(recovered.is_none());
    }

    #[test]
    fn multiline_recovery_stops_when_json_not_completed() {
        let spec = make_spec();
        let bytes = b"{\"client_token\":\"ct-bad\",\"text\":\"hello\nworld\n";
        let first_line_end = bytes
            .iter()
            .position(|b| *b == b'\n')
            .map(|idx| idx + 1)
            .expect("newline");
        let first_payload = decode_jsonl_line(&bytes[..first_line_end], true)
            .expect("decode")
            .expect("payload");

        let recovered =
            recover_multiline_json_payload(bytes, &first_payload, first_line_end, &spec);
        assert!(recovered.is_none());
    }
    #[test]
    fn parse_handle_rejects_non_json_payload() {
        assert!(parse_paging_request("ph_7f2c\n").is_err());
    }

    #[test]
    fn parse_snapshot_refresh_requires_resource_path() {
        assert!(parse_snapshot_refresh_request(
            r#"{"resource_path":"/chats/chat-001/messages.res.jsonl"}"#
        )
        .is_ok());
        assert!(parse_snapshot_refresh_request(r#"{"path":"bad"}"#).is_err());
    }

    #[test]
    fn snapshot_on_timeout_policy_defaults_to_return_stale() {
        assert_eq!(
            parse_snapshot_on_timeout_policy(None),
            SnapshotOnTimeoutPolicy::ReturnStale
        );
        assert_eq!(
            parse_snapshot_on_timeout_policy(Some("")),
            SnapshotOnTimeoutPolicy::ReturnStale
        );
        assert_eq!(
            parse_snapshot_on_timeout_policy(Some("return_stale")),
            SnapshotOnTimeoutPolicy::ReturnStale
        );
    }

    #[test]
    fn snapshot_on_timeout_policy_parses_fail() {
        assert_eq!(
            parse_snapshot_on_timeout_policy(Some("fail")),
            SnapshotOnTimeoutPolicy::Fail
        );
        assert_eq!(
            parse_snapshot_on_timeout_policy(Some("FAIL")),
            SnapshotOnTimeoutPolicy::Fail
        );
    }

    #[test]
    fn resource_path_normalization_and_safety() {
        assert_eq!(
            normalize_resource_rel_path("/chats/chat-001/messages.res.jsonl").as_deref(),
            Some("chats/chat-001/messages.res.jsonl")
        );
        assert!(is_safe_resource_rel_path(
            "chats/chat-001/messages.res.jsonl"
        ));
        assert!(!is_safe_resource_rel_path("../etc/passwd"));
        assert!(!is_safe_resource_rel_path(
            "chats/chat-001/messages.res.json"
        ));
    }

    #[test]
    fn template_specificity_prefers_concrete_snapshot_template() {
        let rel = "chats/chat-oversize/messages.res.jsonl";
        let generic = "chats/{chat_id}/messages.res.jsonl";
        let concrete = "chats/chat-oversize/messages.res.jsonl";
        assert!(action_template_matches(generic, rel));
        assert!(action_template_matches(concrete, rel));

        let selected = [generic, concrete]
            .into_iter()
            .filter(|template| action_template_matches(template, rel))
            .max_by_key(|template| template_specificity(template))
            .expect("expected at least one template match");
        assert_eq!(selected, concrete);
    }

    #[test]
    fn boundary_probe_is_stable() {
        let bytes = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let probe_a = boundary_probe_from_bytes(bytes, bytes.len() as u64).expect("probe");
        let probe_b = boundary_probe_from_bytes(bytes, bytes.len() as u64).expect("probe");
        assert_eq!(probe_a, probe_b);
    }
}
