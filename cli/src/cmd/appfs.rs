use agentfs_sdk::AppConnector;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

mod action_dispatcher;
mod bridge_resilience;
mod core;
mod errors;
mod events;
mod grpc_bridge_adapter;
mod http_bridge_adapter;
mod journal;
#[cfg(any(unix, target_os = "windows"))]
pub(crate) mod mount_runtime;
mod paging;
mod recovery;
pub(crate) mod registry;
mod registry_manager;
mod runtime_config;
mod runtime_entry;
mod runtime_supervisor;
mod shared;
mod snapshot_cache;
mod supervisor_control;
#[cfg(test)]
mod tests;
mod tree_sync;

use journal::SnapshotExpandJournalEntry;
pub(crate) use runtime_config::{
    build_appfs_bridge_config, build_runtime_cli_args, normalize_appfs_session_id,
    resolve_runtime_cli_args, AppfsBridgeCliArgs, AppfsBridgeConfig, AppfsRuntimeCliArgs,
    ResolvedAppfsRuntimeCliArgs,
};
use runtime_supervisor::AppfsRuntimeSupervisor;

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
    pub managed: bool,
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
pub struct AppfsUpArgs {
    pub id_or_path: String,
    pub mountpoint: PathBuf,
    pub backend: crate::cmd::mount::MountBackend,
    pub auto_unmount: bool,
    pub allow_root: bool,
    pub allow_other: bool,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub poll_ms: u64,
}

fn build_managed_mount_args(args: &AppfsUpArgs) -> crate::cmd::MountArgs {
    crate::cmd::MountArgs {
        id_or_path: args.id_or_path.clone(),
        mountpoint: args.mountpoint.clone(),
        auto_unmount: args.auto_unmount,
        allow_root: args.allow_root,
        allow_other: args.allow_other,
        foreground: true,
        uid: args.uid,
        gid: args.gid,
        backend: args.backend,
        appfs_app_id: None,
        appfs_app_ids: Vec::new(),
        managed_appfs: true,
        appfs_session: None,
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

pub async fn handle_appfs_adapter_command(args: AppfsServeArgs) -> Result<()> {
    let AppfsServeArgs {
        root,
        managed,
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

    let bridge_args = AppfsBridgeCliArgs {
        adapter_http_endpoint,
        adapter_http_timeout_ms,
        adapter_grpc_endpoint,
        adapter_grpc_timeout_ms,
        adapter_bridge_max_retries,
        adapter_bridge_initial_backoff_ms,
        adapter_bridge_max_backoff_ms,
        adapter_bridge_circuit_breaker_failures,
        adapter_bridge_circuit_breaker_cooldown_ms,
    };
    let (runtime_args, existing_registry) = if managed {
        if app_id.is_some()
            || !app_ids.is_empty()
            || session_id.is_some()
            || bridge_args.adapter_http_endpoint.is_some()
            || bridge_args.adapter_grpc_endpoint.is_some()
        {
            anyhow::bail!(
                "--managed does not accept explicit --app-id/--app/--session-id/adapter endpoint bootstrap flags; load them from the persisted AppFS registry instead"
            );
        }
        let existing = registry::read_app_registry(&root)?;
        let runtime_args = match existing.as_ref() {
            Some(doc) => registry::runtime_args_from_registry(doc)?,
            None => Vec::new(),
        };
        (runtime_args, existing)
    } else {
        (
            build_runtime_cli_args(app_id, app_ids, session_id, bridge_args, Some("aiim"))?,
            None,
        )
    };
    let resolved_runtime_args = resolve_runtime_cli_args(runtime_args);
    let mut supervisor = AppfsRuntimeSupervisor::new(root, resolved_runtime_args)?;
    supervisor.prepare_action_sinks()?;
    supervisor.sync_registry_to_disk(existing_registry.as_ref())?;
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

pub async fn handle_appfs_up_command(args: AppfsUpArgs) -> Result<()> {
    let mount_args = build_managed_mount_args(&args);
    let mountpoint = mount_args.mountpoint.clone();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let mount_thread = std::thread::spawn(move || {
        let startup_tx = ready_tx.clone();
        let result = crate::cmd::mount::mount_with_ready(mount_args, Some(startup_tx));
        if let Err(err) = &result {
            let _ = ready_tx.send(Err(anyhow::anyhow!(err.to_string())));
        }
        result
    });

    match ready_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            let _ = mount_thread.join();
            return Err(err);
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            return Err(anyhow::anyhow!(
                "AppFS mount did not report readiness within 10 seconds: {}",
                mountpoint.display()
            ));
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            return Err(anyhow::anyhow!(
                "AppFS mount exited before reporting readiness: {}",
                mountpoint.display()
            ));
        }
    }

    let runtime_result = handle_appfs_adapter_command(AppfsServeArgs {
        root: mountpoint,
        managed: true,
        app_id: None,
        app_ids: Vec::new(),
        session_id: None,
        poll_ms: args.poll_ms,
        adapter_http_endpoint: None,
        adapter_http_timeout_ms: 5_000,
        adapter_grpc_endpoint: None,
        adapter_grpc_timeout_ms: 5_000,
        adapter_bridge_max_retries: 2,
        adapter_bridge_initial_backoff_ms: 100,
        adapter_bridge_max_backoff_ms: 1_000,
        adapter_bridge_circuit_breaker_failures: 5,
        adapter_bridge_circuit_breaker_cooldown_ms: 3_000,
    })
    .await;

    if let Err(err) = runtime_result {
        return Err(err);
    }

    match mount_thread.join() {
        Ok(Ok(())) => runtime_result,
        Ok(Err(err)) => Err(err),
        Err(_) => Err(anyhow::anyhow!(
            "AppFS mount thread panicked during shutdown"
        )),
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
    connector: Box<dyn AppConnector>,
}

#[cfg(test)]
mod supervisor_tests {
    use super::{
        build_runtime_cli_args, registry, resolve_runtime_cli_args, runtime_config,
        AppfsBridgeCliArgs, AppfsRuntimeSupervisor,
    };
    use serde_json::{json, Value as JsonValue};
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

    #[test]
    fn appfs_up_builds_managed_mount_args() {
        let args = super::AppfsUpArgs {
            id_or_path: ".agentfs/demo.db".to_string(),
            mountpoint: std::path::PathBuf::from("C:\\mnt\\demo"),
            backend: crate::cmd::mount::MountBackend::Winfsp,
            auto_unmount: true,
            allow_root: false,
            allow_other: true,
            uid: Some(1000),
            gid: Some(1000),
            poll_ms: 150,
        };

        let mount_args = super::build_managed_mount_args(&args);
        assert_eq!(mount_args.id_or_path, ".agentfs/demo.db");
        assert_eq!(
            mount_args.mountpoint,
            std::path::PathBuf::from("C:\\mnt\\demo")
        );
        assert!(mount_args.managed_appfs);
        assert!(mount_args.foreground);
        assert!(mount_args.allow_other);
        assert!(mount_args.appfs_app_id.is_none());
        assert!(mount_args.appfs_app_ids.is_empty());
        assert!(mount_args.adapter_http_endpoint.is_none());
        assert!(mount_args.adapter_grpc_endpoint.is_none());
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

    fn control_events(temp: &TempDir, token: &str) -> Vec<JsonValue> {
        token_events(&temp.path().join("_appfs/_stream/events.evt.jsonl"), token)
    }

    #[test]
    fn normalize_app_ids_defaults_and_deduplicates() {
        let app_ids = runtime_config::normalize_appfs_app_ids(
            Some("aiim".to_string()),
            vec![" notion ".into(), "aiim".into()],
            Some("default"),
        )
        .expect("normalize app ids");
        assert_eq!(app_ids, vec!["aiim".to_string(), "notion".to_string()]);

        let defaulted = runtime_config::normalize_appfs_app_ids(None, Vec::new(), Some("aiim"))
            .expect("default app id");
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
            resolve_runtime_cli_args(runtime_args),
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

    #[test]
    fn supervisor_persists_registry_for_bootstrap_apps() {
        let temp = TempDir::new().expect("tempdir");
        let runtime_args = build_runtime_cli_args(
            Some("aiim".to_string()),
            Vec::new(),
            Some("sess-aiim".to_string()),
            bridge_args(),
            None,
        )
        .expect("build runtime args");
        let mut supervisor = AppfsRuntimeSupervisor::new(
            temp.path().to_path_buf(),
            resolve_runtime_cli_args(runtime_args),
        )
        .expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");
        supervisor
            .sync_registry_to_disk(None)
            .expect("persist registry");

        let stored = registry::read_app_registry(temp.path())
            .expect("read registry")
            .expect("registry exists");
        assert_eq!(stored.apps.len(), 1);
        assert_eq!(stored.apps[0].app_id, "aiim");
        assert_eq!(stored.apps[0].session_id, "sess-aiim");
    }

    #[test]
    fn supervisor_preserves_existing_registry_registration_time() {
        let temp = TempDir::new().expect("tempdir");
        let runtime_args = build_runtime_cli_args(
            Some("aiim".to_string()),
            Vec::new(),
            Some("sess-aiim".to_string()),
            bridge_args(),
            None,
        )
        .expect("build runtime args");
        let mut supervisor = AppfsRuntimeSupervisor::new(
            temp.path().to_path_buf(),
            resolve_runtime_cli_args(runtime_args),
        )
        .expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");

        let existing = registry::AppfsAppsRegistryDoc {
            version: registry::APPFS_REGISTRY_VERSION,
            apps: vec![registry::AppfsRegisteredAppDoc {
                app_id: "aiim".to_string(),
                transport: registry::AppfsRegistryTransportDoc {
                    kind: registry::AppfsRegistryTransportKind::InProcess,
                    endpoint: None,
                    http_timeout_ms: 5000,
                    grpc_timeout_ms: 5000,
                    bridge_max_retries: 2,
                    bridge_initial_backoff_ms: 100,
                    bridge_max_backoff_ms: 1000,
                    bridge_circuit_breaker_failures: 5,
                    bridge_circuit_breaker_cooldown_ms: 3000,
                },
                session_id: "sess-old".to_string(),
                registered_at: "2026-03-25T00:00:00Z".to_string(),
                active_scope: Some("chat-001".to_string()),
            }],
        };
        registry::write_app_registry(temp.path(), &existing).expect("seed registry");

        let sync_state_path = temp
            .path()
            .join("aiim")
            .join("_meta")
            .join(super::APP_STRUCTURE_SYNC_STATE_FILENAME);
        fs::write(
            &sync_state_path,
            serde_json::to_vec(&json!({
                "active_scope": "chat-long"
            }))
            .expect("sync state json"),
        )
        .expect("write sync state");

        supervisor
            .sync_registry_to_disk(Some(&existing))
            .expect("persist registry");

        let stored = registry::read_app_registry(temp.path())
            .expect("read registry")
            .expect("registry exists");
        assert_eq!(stored.apps[0].registered_at, "2026-03-25T00:00:00Z");
        assert_eq!(stored.apps[0].active_scope.as_deref(), Some("chat-long"));
    }

    #[test]
    fn supervisor_can_register_app_dynamically_from_empty_runtime() {
        let temp = TempDir::new().expect("tempdir");
        let mut supervisor =
            AppfsRuntimeSupervisor::new(temp.path().to_path_buf(), Vec::new()).expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");

        append_text(
            &temp.path().join("_appfs/register_app.act"),
            "{\"app_id\":\"notion\",\"transport\":{\"kind\":\"in_process\",\"http_timeout_ms\":5000,\"grpc_timeout_ms\":5000,\"bridge_max_retries\":2,\"bridge_initial_backoff_ms\":100,\"bridge_max_backoff_ms\":1000,\"bridge_circuit_breaker_failures\":5,\"bridge_circuit_breaker_cooldown_ms\":3000},\"client_token\":\"reg-001\"}\n",
        );

        supervisor.poll_once().expect("poll register");

        assert!(supervisor.runtimes.contains_key("notion"));
        assert!(temp.path().join("notion/_meta/manifest.res.json").exists());
        let stored = registry::read_app_registry(temp.path())
            .expect("read registry")
            .expect("registry exists");
        assert_eq!(stored.apps.len(), 1);
        assert_eq!(stored.apps[0].app_id, "notion");

        let events = control_events(&temp, "reg-001");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("type").and_then(|value| value.as_str()),
            Some("action.completed")
        );
    }

    #[test]
    fn supervisor_can_list_and_unregister_apps_without_deleting_tree() {
        let temp = TempDir::new().expect("tempdir");
        let runtime_args = build_runtime_cli_args(
            Some("aiim".to_string()),
            Vec::new(),
            Some("sess-aiim".to_string()),
            bridge_args(),
            None,
        )
        .expect("build runtime args");
        let mut supervisor = AppfsRuntimeSupervisor::new(
            temp.path().to_path_buf(),
            resolve_runtime_cli_args(runtime_args),
        )
        .expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");
        supervisor
            .sync_registry_to_disk(None)
            .expect("persist registry");

        append_text(
            &temp.path().join("_appfs/list_apps.act"),
            "{\"client_token\":\"list-001\"}\n",
        );
        supervisor.poll_once().expect("poll list");
        let list_events = control_events(&temp, "list-001");
        assert_eq!(list_events.len(), 1);
        assert_eq!(
            list_events[0]
                .get("content")
                .and_then(|value| value.get("apps"))
                .and_then(|value| value.as_array())
                .map(|apps| apps.len()),
            Some(1)
        );

        append_text(
            &temp.path().join("_appfs/unregister_app.act"),
            "{\"app_id\":\"aiim\",\"client_token\":\"unreg-001\"}\n",
        );
        supervisor.poll_once().expect("poll unregister");

        assert!(!supervisor.runtimes.contains_key("aiim"));
        assert!(temp.path().join("aiim").exists());
        let stored = registry::read_app_registry(temp.path())
            .expect("read registry")
            .expect("registry exists");
        assert!(stored.apps.is_empty());

        let unregister_events = control_events(&temp, "unreg-001");
        assert_eq!(unregister_events.len(), 1);
        assert_eq!(
            unregister_events[0]
                .get("type")
                .and_then(|value| value.as_str()),
            Some("action.completed")
        );
    }
}
