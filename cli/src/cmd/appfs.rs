use agentfs_sdk::{AppConnectorV2, AppConnectorV3};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
};
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
pub(crate) mod registry;
mod shared;
mod snapshot_cache;
mod supervisor_control;
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

#[derive(Debug, Clone)]
pub(crate) struct ResolvedAppfsRuntimeCliArgs {
    pub app_id: String,
    pub session_id: String,
    pub bridge: AppfsBridgeCliArgs,
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

pub(crate) fn resolve_runtime_cli_args(
    runtime_args: Vec<AppfsRuntimeCliArgs>,
) -> Vec<ResolvedAppfsRuntimeCliArgs> {
    runtime_args
        .into_iter()
        .map(|runtime| ResolvedAppfsRuntimeCliArgs {
            app_id: runtime.app_id,
            session_id: normalize_appfs_session_id(runtime.session_id),
            bridge: runtime.bridge,
        })
        .collect()
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

struct AppRuntimeEntry {
    runtime: ResolvedAppfsRuntimeCliArgs,
    adapter: AppfsAdapter,
}

struct AppfsRuntimeSupervisor {
    root: PathBuf,
    control_plane: supervisor_control::SupervisorControlPlane,
    runtimes: BTreeMap<String, AppRuntimeEntry>,
}

impl AppfsRuntimeSupervisor {
    fn new(root: PathBuf, runtime_args: Vec<ResolvedAppfsRuntimeCliArgs>) -> Result<Self> {
        let mut runtimes = BTreeMap::new();
        for runtime in runtime_args {
            let entry = Self::build_runtime_entry(&root, runtime)?;
            if runtimes
                .insert(entry.runtime.app_id.clone(), entry)
                .is_some()
            {
                anyhow::bail!("duplicate runtime app_id during supervisor bootstrap");
            }
        }
        Ok(Self {
            control_plane: supervisor_control::SupervisorControlPlane::new(
                root.clone(),
                std::env::var("APPFS_V2_ACTIONLINE_STRICT")
                    .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"))
                    .unwrap_or(false),
            )?,
            root,
            runtimes,
        })
    }

    fn prepare_action_sinks(&mut self) -> Result<()> {
        self.control_plane.prepare_action_sinks()?;
        for entry in self.runtimes.values_mut() {
            entry.adapter.prepare_action_sinks()?;
        }
        Ok(())
    }

    fn poll_once(&mut self) -> Result<()> {
        let invocations = self.control_plane.drain_invocations()?;
        for invocation in invocations {
            self.handle_control_invocation(invocation)?;
        }
        for entry in self.runtimes.values_mut() {
            entry.adapter.poll_once()?;
        }
        self.sync_registry_to_disk(None)?;
        Ok(())
    }

    fn log_started(&self) {
        for entry in self.runtimes.values() {
            let adapter = &entry.adapter;
            eprintln!(
                "AppFS adapter started for {} (app_id={} session={})",
                adapter.app_dir.display(),
                adapter.app_id,
                adapter.session_id
            );
        }
    }

    fn sync_registry_to_disk(
        &self,
        existing: Option<&registry::AppfsAppsRegistryDoc>,
    ) -> Result<()> {
        let existing = match existing {
            Some(existing) => Some(existing.clone()),
            None => registry::read_app_registry(&self.root)?,
        };
        let active_scopes = self
            .runtimes
            .iter()
            .map(|(app_id, entry)| (app_id.clone(), read_active_scope(&entry.adapter.app_dir)))
            .collect::<HashMap<_, _>>();
        let runtime_args = self
            .runtimes
            .values()
            .map(|entry| entry.runtime.clone())
            .collect::<Vec<_>>();
        let doc =
            registry::build_app_registry_doc(&runtime_args, &active_scopes, existing.as_ref());
        if existing.as_ref() == Some(&doc) {
            return Ok(());
        }
        registry::write_app_registry(&self.root, &doc)
    }

    fn handle_control_invocation(
        &mut self,
        invocation: supervisor_control::SupervisorControlInvocation,
    ) -> Result<()> {
        match invocation {
            supervisor_control::SupervisorControlInvocation::Register {
                request_id,
                client_token,
                request,
            } => self.handle_register_app(&request_id, client_token, request),
            supervisor_control::SupervisorControlInvocation::Unregister {
                request_id,
                client_token,
                request,
            } => self.handle_unregister_app(&request_id, client_token, request),
            supervisor_control::SupervisorControlInvocation::List {
                request_id,
                client_token,
            } => self.handle_list_apps(&request_id, client_token),
        }
    }

    fn handle_register_app(
        &mut self,
        request_id: &str,
        client_token: Option<String>,
        request: action_dispatcher::RegisterAppRequest,
    ) -> Result<()> {
        if self.runtimes.contains_key(&request.app_id) {
            self.control_plane.emit_failed(
                "/_appfs/register_app.act",
                request_id,
                "APP_ALREADY_REGISTERED",
                &format!("app {} is already registered", request.app_id),
                client_token,
            )?;
            return Ok(());
        }

        let runtime = match register_request_to_runtime(request) {
            Ok(runtime) => runtime,
            Err(err) => {
                self.control_plane.emit_failed(
                    "/_appfs/register_app.act",
                    request_id,
                    "APP_REGISTER_INVALID",
                    &err.to_string(),
                    client_token,
                )?;
                return Ok(());
            }
        };

        match Self::build_runtime_entry(&self.root, runtime.clone()) {
            Ok(mut entry) => {
                entry.adapter.prepare_action_sinks()?;
                let app_id = entry.runtime.app_id.clone();
                let session_id = entry.runtime.session_id.clone();
                let transport = transport_summary(&entry.runtime.bridge);
                self.runtimes.insert(app_id.clone(), entry);
                self.sync_registry_to_disk(None)?;
                self.control_plane.emit_completed(
                    "/_appfs/register_app.act",
                    request_id,
                    serde_json::json!({
                        "app_id": app_id,
                        "session_id": session_id,
                        "transport": transport,
                        "registered": true,
                    }),
                    client_token,
                )?;
            }
            Err(err) => {
                self.control_plane.emit_failed(
                    "/_appfs/register_app.act",
                    request_id,
                    "APP_REGISTER_FAILED",
                    &format!("failed to register app: {err}"),
                    client_token,
                )?;
            }
        }
        Ok(())
    }

    fn handle_unregister_app(
        &mut self,
        request_id: &str,
        client_token: Option<String>,
        request: action_dispatcher::UnregisterAppRequest,
    ) -> Result<()> {
        let Some(entry) = self.runtimes.remove(&request.app_id) else {
            self.control_plane.emit_failed(
                "/_appfs/unregister_app.act",
                request_id,
                "APP_NOT_REGISTERED",
                &format!("app {} is not registered", request.app_id),
                client_token,
            )?;
            return Ok(());
        };
        self.sync_registry_to_disk(None)?;
        self.control_plane.emit_completed(
            "/_appfs/unregister_app.act",
            request_id,
            serde_json::json!({
                "app_id": entry.runtime.app_id,
                "session_id": entry.runtime.session_id,
                "unregistered": true,
            }),
            client_token,
        )?;
        Ok(())
    }

    fn handle_list_apps(&mut self, request_id: &str, client_token: Option<String>) -> Result<()> {
        let apps = self
            .runtimes
            .values()
            .map(|entry| {
                serde_json::json!({
                    "app_id": entry.runtime.app_id,
                    "session_id": entry.runtime.session_id,
                    "transport": transport_summary(&entry.runtime.bridge),
                    "active_scope": read_active_scope(&entry.adapter.app_dir),
                })
            })
            .collect::<Vec<_>>();
        self.control_plane.emit_completed(
            "/_appfs/list_apps.act",
            request_id,
            serde_json::json!({ "apps": apps }),
            client_token,
        )?;
        Ok(())
    }

    fn build_runtime_entry(
        root: &Path,
        runtime: ResolvedAppfsRuntimeCliArgs,
    ) -> Result<AppRuntimeEntry> {
        let adapter = AppfsAdapter::new(
            root.to_path_buf(),
            runtime.app_id.clone(),
            runtime.session_id.clone(),
            build_appfs_bridge_config(runtime.bridge.clone()),
        )?;
        Ok(AppRuntimeEntry { runtime, adapter })
    }
}

fn register_request_to_runtime(
    request: action_dispatcher::RegisterAppRequest,
) -> Result<ResolvedAppfsRuntimeCliArgs> {
    let session_id = normalize_appfs_session_id(request.session_id);
    let doc = registry::AppfsAppsRegistryDoc {
        version: registry::APPFS_REGISTRY_VERSION,
        apps: vec![registry::AppfsRegisteredAppDoc {
            app_id: request.app_id,
            transport: request.transport,
            session_id: session_id.clone(),
            registered_at: chrono::Utc::now().to_rfc3339(),
            active_scope: None,
        }],
    };
    let mut runtimes = resolve_runtime_cli_args(registry::runtime_args_from_registry(&doc)?);
    let runtime = runtimes
        .pop()
        .ok_or_else(|| anyhow::anyhow!("register request did not resolve any runtime args"))?;
    Ok(ResolvedAppfsRuntimeCliArgs {
        session_id,
        ..runtime
    })
}

fn read_active_scope(app_dir: &Path) -> Option<String> {
    let state_path = app_dir
        .join("_meta")
        .join(APP_STRUCTURE_SYNC_STATE_FILENAME);
    fs::read_to_string(&state_path)
        .ok()
        .and_then(|content| serde_json::from_str::<JsonValue>(&content).ok())
        .and_then(|value| {
            value
                .get("active_scope")
                .and_then(|scope| scope.as_str())
                .map(ToString::to_string)
        })
}

fn transport_summary(bridge: &AppfsBridgeCliArgs) -> JsonValue {
    if let Some(endpoint) = &bridge.adapter_http_endpoint {
        serde_json::json!({
            "kind": "http",
            "endpoint": endpoint,
        })
    } else if let Some(endpoint) = &bridge.adapter_grpc_endpoint {
        serde_json::json!({
            "kind": "grpc",
            "endpoint": endpoint,
        })
    } else {
        serde_json::json!({
            "kind": "in_process",
        })
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
        build_runtime_cli_args, normalize_appfs_app_ids, registry, resolve_runtime_cli_args,
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
