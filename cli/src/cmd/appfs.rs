use agentfs_sdk::{AppConnectorV2, AppConnectorV3};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;

mod action_dispatcher;
mod bridge_resilience;
mod core;
mod errors;
mod events;
mod grpc_bridge_adapter;
mod http_bridge_adapter;
mod journal;
#[cfg(any(unix, target_os = "windows"))]
pub(crate) mod mount_readthrough;
mod paging;
mod recovery;
mod shared;
mod snapshot_cache;
#[cfg(test)]
mod tests;
mod tree_sync;

use bridge_resilience::BridgeRuntimeOptions;
use journal::SnapshotExpandJournalEntry;

const DEFAULT_RETENTION_HINT_SEC: i64 = 86400;
const MIN_POLL_MS: u64 = 50;
const ACTION_CURSORS_FILENAME: &str = "action-cursors.res.json";
const ACTION_CURSOR_PROBE_WINDOW: usize = 64;
const MAX_RECOVERY_LINES: usize = 32;
const MAX_RECOVERY_BYTES: usize = 65536;
const DEFAULT_SNAPSHOT_MAX_MATERIALIZED_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_SNAPSHOT_PREWARM_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_SNAPSHOT_READ_THROUGH_TIMEOUT_MS: u64 = 10_000;
const SNAPSHOT_EXPAND_DELAY_ENV: &str = "APPFS_V2_SNAPSHOT_EXPAND_DELAY_MS";
const SNAPSHOT_FORCE_EXPAND_ON_REFRESH_ENV: &str = "APPFS_V2_SNAPSHOT_REFRESH_FORCE_EXPAND";
const SNAPSHOT_COALESCE_WINDOW_ENV: &str = "APPFS_V2_SNAPSHOT_COALESCE_WINDOW_MS";
const SNAPSHOT_PUBLISH_DELAY_ENV: &str = "APPFS_V2_SNAPSHOT_PUBLISH_DELAY_MS";
const DEFAULT_SNAPSHOT_COALESCE_WINDOW_MS: u64 = 120;
const SNAPSHOT_EXPAND_JOURNAL_FILENAME: &str = "snapshot-expand.state.res.json";
const APP_STRUCTURE_SYNC_STATE_FILENAME: &str = "app-structure-sync.state.res.json";

const MAX_SEGMENT_BYTES: usize = 255;

const ALLOWED_SEGMENT_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-~";

#[derive(Debug, Clone)]
pub struct AppfsServeArgs {
    pub root: PathBuf,
    pub app_id: Option<String>,
    pub app_ids: Vec<String>,
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
pub(crate) struct AppfsBridgeCliArgs {
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

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Debug, Clone)]
pub(crate) struct AppfsRuntimeCliArgs {
    pub app_id: String,
    pub session_id: Option<String>,
    pub bridge: AppfsBridgeCliArgs,
}

#[derive(Debug, Clone)]
pub(crate) struct AppfsBridgeConfig {
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
        app_ids,
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

    let runtime_args = build_runtime_cli_args(
        app_id,
        app_ids,
        session_id,
        AppfsBridgeCliArgs {
            adapter_http_endpoint: adapter_http_endpoint.clone(),
            adapter_http_timeout_ms,
            adapter_grpc_endpoint: adapter_grpc_endpoint.clone(),
            adapter_grpc_timeout_ms,
            adapter_bridge_max_retries,
            adapter_bridge_initial_backoff_ms,
            adapter_bridge_max_backoff_ms,
            adapter_bridge_circuit_breaker_failures,
            adapter_bridge_circuit_breaker_cooldown_ms,
        },
        Some("aiim"),
    )?;
    let bridge_config = build_appfs_bridge_config(AppfsBridgeCliArgs {
        adapter_http_endpoint,
        adapter_http_timeout_ms,
        adapter_grpc_endpoint,
        adapter_grpc_timeout_ms,
        adapter_bridge_max_retries,
        adapter_bridge_initial_backoff_ms,
        adapter_bridge_max_backoff_ms,
        adapter_bridge_circuit_breaker_failures,
        adapter_bridge_circuit_breaker_cooldown_ms,
    });

    let mut supervisor = AppfsRuntimeSupervisor::new(root, runtime_args, bridge_config)?;
    supervisor.prepare_action_sinks()?;
    supervisor.log_started();
    eprintln!("Press Ctrl+C to stop.");

    let mut interval = tokio::time::interval(Duration::from_millis(poll_ms.max(MIN_POLL_MS)));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("AppFS adapter stopping...");
                return Ok(());
            }
            _ = interval.tick() => {
                if let Err(err) = supervisor.poll_once() {
                    eprintln!("AppFS adapter poll error: {err:#}");
                }
            }
        }
    }
}

pub(crate) fn normalize_appfs_app_ids(
    primary_app_id: Option<String>,
    extra_app_ids: Vec<String>,
    default_app_id: Option<&str>,
) -> Result<Vec<String>> {
    let mut seen = HashMap::new();
    let mut ordered = Vec::new();

    fn push_unique_app_id(
        seen: &mut HashMap<String, ()>,
        ordered: &mut Vec<String>,
        raw: String,
    ) -> Result<()> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            anyhow::bail!("app id cannot be empty");
        }
        if seen.insert(trimmed.to_string(), ()).is_none() {
            ordered.push(trimmed.to_string());
        }
        Ok(())
    }

    if let Some(primary) = primary_app_id {
        push_unique_app_id(&mut seen, &mut ordered, primary)?;
    }
    for app_id in extra_app_ids {
        push_unique_app_id(&mut seen, &mut ordered, app_id)?;
    }

    if ordered.is_empty() {
        if let Some(default_app_id) = default_app_id {
            push_unique_app_id(&mut seen, &mut ordered, default_app_id.to_string())?;
        }
    }

    Ok(ordered)
}

pub(crate) fn build_runtime_cli_args(
    primary_app_id: Option<String>,
    extra_app_ids: Vec<String>,
    session_id: Option<String>,
    bridge: AppfsBridgeCliArgs,
    default_app_id: Option<&str>,
) -> Result<Vec<AppfsRuntimeCliArgs>> {
    let app_ids = normalize_appfs_app_ids(primary_app_id, extra_app_ids, default_app_id)?;
    if app_ids.len() > 1 && session_id.is_some() {
        anyhow::bail!(
            "multi-app AppFS runtime does not accept a single shared --session-id; omit it and runtime will generate isolated per-app sessions"
        );
    }

    Ok(app_ids
        .into_iter()
        .map(|app_id| AppfsRuntimeCliArgs {
            app_id,
            session_id: session_id.clone(),
            bridge: bridge.clone(),
        })
        .collect())
}

pub(crate) fn normalize_appfs_session_id(session_id: Option<String>) -> String {
    session_id.unwrap_or_else(|| {
        let uuid = Uuid::new_v4().simple().to_string();
        format!("sess-{}", &uuid[..8])
    })
}

pub(crate) fn build_appfs_bridge_config(args: AppfsBridgeCliArgs) -> AppfsBridgeConfig {
    let bridge_runtime_options = BridgeRuntimeOptions::from_cli(
        args.adapter_bridge_max_retries,
        args.adapter_bridge_initial_backoff_ms,
        args.adapter_bridge_max_backoff_ms,
        args.adapter_bridge_circuit_breaker_failures,
        args.adapter_bridge_circuit_breaker_cooldown_ms,
    );
    AppfsBridgeConfig {
        adapter_http_endpoint: args.adapter_http_endpoint,
        adapter_http_timeout_ms: args.adapter_http_timeout_ms,
        adapter_grpc_endpoint: args.adapter_grpc_endpoint,
        adapter_grpc_timeout_ms: args.adapter_grpc_timeout_ms,
        runtime_options: bridge_runtime_options,
    }
}

struct AppfsRuntimeSupervisor {
    adapters: Vec<AppfsAdapter>,
}

impl AppfsRuntimeSupervisor {
    fn new(
        root: PathBuf,
        runtime_args: Vec<AppfsRuntimeCliArgs>,
        bridge_config: AppfsBridgeConfig,
    ) -> Result<Self> {
        let mut adapters = Vec::with_capacity(runtime_args.len());
        for runtime in runtime_args {
            let session_id = normalize_appfs_session_id(runtime.session_id.clone());
            adapters.push(AppfsAdapter::new(
                root.clone(),
                runtime.app_id,
                session_id,
                bridge_config.clone(),
            )?);
        }
        Ok(Self { adapters })
    }

    fn prepare_action_sinks(&mut self) -> Result<()> {
        for adapter in &mut self.adapters {
            adapter.prepare_action_sinks()?;
        }
        Ok(())
    }

    fn poll_once(&mut self) -> Result<()> {
        for adapter in &mut self.adapters {
            adapter.poll_once()?;
        }
        Ok(())
    }

    fn log_started(&self) {
        for adapter in &self.adapters {
            eprintln!(
                "AppFS adapter started for {} (app_id={} session={})",
                adapter.app_dir.display(),
                adapter.app_id,
                adapter.session_id
            );
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
    prewarm: bool,
    prewarm_timeout_ms: u64,
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
    prewarm: Option<bool>,
    #[serde(default)]
    prewarm_timeout_ms: Option<u64>,
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
    page_no: u32,
    closed: bool,
    owner_session: String,
    expires_at_ts: Option<i64>,
    upstream_cursor: Option<String>,
    resource_path: String,
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
    #[serde(default)]
    pending_multiline_eof_len: Option<u64>,
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
    business_connector: Box<dyn AppConnectorV2>,
    structure_connector: Option<Box<dyn AppConnectorV3>>,
}

#[cfg(test)]
mod supervisor_tests {
    use super::{
        build_appfs_bridge_config, build_runtime_cli_args, normalize_appfs_app_ids,
        AppfsBridgeCliArgs, AppfsRuntimeSupervisor,
    };
    use serde_json::Value as JsonValue;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use tempfile::TempDir;

    fn bridge_args() -> AppfsBridgeCliArgs {
        AppfsBridgeCliArgs {
            adapter_http_endpoint: None,
            adapter_http_timeout_ms: 5_000,
            adapter_grpc_endpoint: None,
            adapter_grpc_timeout_ms: 5_000,
            adapter_bridge_max_retries: 2,
            adapter_bridge_initial_backoff_ms: 100,
            adapter_bridge_max_backoff_ms: 1_000,
            adapter_bridge_circuit_breaker_failures: 5,
            adapter_bridge_circuit_breaker_cooldown_ms: 3_000,
        }
    }

    fn append_text(path: &std::path::Path, text: &str) {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .expect("open append");
        file.write_all(text.as_bytes()).expect("append text");
        file.flush().expect("flush append");
    }

    fn token_events(events_path: &std::path::Path, token: &str) -> Vec<JsonValue> {
        let content = fs::read_to_string(events_path).expect("read events");
        content
            .lines()
            .filter(|line| line.contains(token))
            .map(|line| serde_json::from_str(line).expect("event json"))
            .collect()
    }

    #[test]
    fn normalize_app_ids_defaults_and_deduplicates() {
        let app_ids = normalize_appfs_app_ids(
            Some("aiim".to_string()),
            vec![" notion ".into(), "aiim".into()],
            Some("default"),
        )
        .expect("normalize app ids");
        assert_eq!(app_ids, vec!["aiim".to_string(), "notion".to_string()]);

        let defaulted =
            normalize_appfs_app_ids(None, Vec::new(), Some("aiim")).expect("default app id");
        assert_eq!(defaulted, vec!["aiim".to_string()]);
    }

    #[test]
    fn multi_app_runtime_rejects_single_shared_session_id() {
        let err = build_runtime_cli_args(
            Some("aiim".to_string()),
            vec!["notion".to_string()],
            Some("sess-shared".to_string()),
            bridge_args(),
            None,
        )
        .expect_err("multi-app shared session must be rejected");
        assert!(err.to_string().contains("single shared --session-id"));
    }

    #[test]
    fn supervisor_isolates_structure_refresh_per_app() {
        let temp = TempDir::new().expect("tempdir");
        let runtime_args = build_runtime_cli_args(
            Some("aiim".to_string()),
            vec!["notion".to_string()],
            None,
            bridge_args(),
            None,
        )
        .expect("build runtime args");
        let mut supervisor = AppfsRuntimeSupervisor::new(
            temp.path().to_path_buf(),
            runtime_args,
            build_appfs_bridge_config(bridge_args()),
        )
        .expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");

        let aiim_action = temp.path().join("aiim/_app/enter_scope.act");
        append_text(
            &aiim_action,
            "{\"target_scope\":\"chat-long\",\"client_token\":\"multi-001\"}\n",
        );

        supervisor.poll_once().expect("poll once");

        assert!(temp.path().join("aiim/chats/chat-long").exists());
        assert!(!temp.path().join("aiim/chats/chat-001").exists());
        assert!(temp.path().join("notion/chats/chat-001").exists());
        assert!(!temp.path().join("notion/chats/chat-long").exists());

        let aiim_events = token_events(
            &temp.path().join("aiim/_stream/events.evt.jsonl"),
            "multi-001",
        );
        assert_eq!(aiim_events.len(), 1);
        assert_eq!(
            aiim_events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );

        let notion_events = token_events(
            &temp.path().join("notion/_stream/events.evt.jsonl"),
            "multi-001",
        );
        assert!(notion_events.is_empty());
    }
}
