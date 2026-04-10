use agentfs_sdk::AppConnector;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command as ProcessCommand};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
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
pub(crate) mod mount_runtime;
mod paging;
mod recovery;
pub(crate) mod registry;
mod registry_manager;
mod runtime_config;
mod runtime_entry;
mod runtime_manifest;
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
const DEFAULT_ACTION_FALLBACK_POLL_MS: u64 = 500;
const ACTION_CURSORS_FILENAME: &str = "action-cursors.res.json";
const ACTION_CURSOR_PROBE_WINDOW: usize = 64;
const MAX_RECOVERY_LINES: usize = 32;
const MAX_RECOVERY_BYTES: usize = 65536;
const DEFAULT_SNAPSHOT_MAX_MATERIALIZED_BYTES: usize = 10 * 1024 * 1024;
const DEFAULT_SNAPSHOT_PREWARM_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_SNAPSHOT_READ_THROUGH_TIMEOUT_MS: u64 = 10_000;
const SNAPSHOT_EXPAND_DELAY_ENV: &str = "APPFS_SNAPSHOT_EXPAND_DELAY_MS";
const SNAPSHOT_FORCE_EXPAND_ON_REFRESH_ENV: &str = "APPFS_SNAPSHOT_REFRESH_FORCE_EXPAND";
const SNAPSHOT_COALESCE_WINDOW_ENV: &str = "APPFS_SNAPSHOT_COALESCE_WINDOW_MS";
const SNAPSHOT_PUBLISH_DELAY_ENV: &str = "APPFS_SNAPSHOT_PUBLISH_DELAY_MS";
const DEFAULT_SNAPSHOT_COALESCE_WINDOW_MS: u64 = 120;
const SNAPSHOT_EXPAND_JOURNAL_FILENAME: &str = "snapshot-expand.state.res.json";
const APP_STRUCTURE_SYNC_STATE_FILENAME: &str = "app-structure-sync.state.res.json";
const APPFS_ATTACH_SCHEMA_ENV: &str = "APPFS_ATTACH_SCHEMA";
const APPFS_RUNTIME_MANIFEST_ENV: &str = "APPFS_RUNTIME_MANIFEST";
const APPFS_MOUNT_ROOT_ENV: &str = "APPFS_MOUNT_ROOT";
const APPFS_RUNTIME_SESSION_ID_ENV: &str = "APPFS_RUNTIME_SESSION_ID";
const APPFS_ATTACH_ID_ENV: &str = "APPFS_ATTACH_ID";
const APPFS_AGENT_ROLE_ENV: &str = "APPFS_AGENT_ROLE";

const MAX_SEGMENT_BYTES: usize = 255;

const ALLOWED_SEGMENT_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-~";

#[derive(Clone)]
pub struct ActionWakeHandle {
    inner: Arc<ActionWakeState>,
}

struct ActionWakeState {
    notify: tokio::sync::Notify,
    pending: AtomicBool,
}

impl Default for ActionWakeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for ActionWakeHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActionWakeHandle").finish_non_exhaustive()
    }
}

impl ActionWakeHandle {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ActionWakeState {
                notify: tokio::sync::Notify::new(),
                pending: AtomicBool::new(false),
            }),
        }
    }

    pub(crate) fn signal(&self) {
        if !self.inner.pending.swap(true, Ordering::SeqCst) {
            self.inner.notify.notify_one();
        }
    }

    async fn wait(&self) {
        loop {
            let notified = self.inner.notify.notified();
            if self.inner.pending.swap(false, Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppfsServeArgs {
    pub root: PathBuf,
    pub managed: bool,
    pub app_id: Option<String>,
    pub app_ids: Vec<String>,
    pub session_id: Option<String>,
    pub poll_ms: u64,
    pub action_wake: Option<ActionWakeHandle>,
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

#[derive(Debug, Clone)]
pub struct AppfsLaunchArgs {
    pub id_or_path: String,
    pub mountpoint: PathBuf,
    pub backend: crate::cmd::mount::MountBackend,
    pub allow_root: bool,
    pub allow_other: bool,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub poll_ms: u64,
    pub agent_bin: PathBuf,
    pub workspace: PathBuf,
    pub attach_id: Option<String>,
    pub attach_role: Option<String>,
    pub startup_timeout_ms: u64,
    pub agent_args: Vec<String>,
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
        action_wake: None,
    }
}

fn build_appfs_up_launcher_command(args: &AppfsLaunchArgs) -> Result<ProcessCommand> {
    let current_exe = std::env::current_exe()
        .context("failed to resolve current agentfs executable for launcher startup")?;
    let mut command = ProcessCommand::new(current_exe);
    command
        .arg("appfs")
        .arg("up")
        .arg(&args.id_or_path)
        .arg(&args.mountpoint)
        .arg("--backend")
        .arg(args.backend.to_string())
        .arg("--poll-ms")
        .arg(args.poll_ms.to_string())
        .arg("--auto-unmount");

    if args.allow_root {
        command.arg("--allow-root");
    }
    if args.allow_other {
        command.arg("--system");
    }
    if let Some(uid) = args.uid {
        command.arg("--uid").arg(uid.to_string());
    }
    if let Some(gid) = args.gid {
        command.arg("--gid").arg(gid.to_string());
    }

    Ok(command)
}

fn validate_workspace_path(workspace: &Path) -> Result<()> {
    if workspace.is_absolute() {
        anyhow::bail!(
            "launcher workspace path must be relative to the AppFS mount root: {}",
            workspace.display()
        );
    }

    for component in workspace.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir => {
                anyhow::bail!(
                    "launcher workspace path cannot escape the AppFS mount root: {}",
                    workspace.display()
                );
            }
            Component::Prefix(_) | Component::RootDir => {
                anyhow::bail!(
                    "launcher workspace path must stay inside the AppFS mount root: {}",
                    workspace.display()
                );
            }
        }
    }

    Ok(())
}

fn resolve_launch_workspace_path(mount_root: &Path, workspace: &Path) -> Result<PathBuf> {
    validate_workspace_path(workspace)?;
    Ok(mount_root.join(workspace))
}

fn generate_launcher_attach_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("agent-{}", &uuid[..8])
}

fn build_launch_agent_environment(
    manifest_path: &Path,
    manifest: &runtime_manifest::AppfsRuntimeManifestDoc,
    attach_id: &str,
    attach_role: Option<&str>,
) -> Vec<(String, String)> {
    let mut envs = vec![
        (APPFS_ATTACH_SCHEMA_ENV.to_string(), "1".to_string()),
        (
            APPFS_RUNTIME_MANIFEST_ENV.to_string(),
            manifest_path.display().to_string(),
        ),
        (
            APPFS_MOUNT_ROOT_ENV.to_string(),
            manifest.mount_root.display().to_string(),
        ),
        (
            APPFS_RUNTIME_SESSION_ID_ENV.to_string(),
            manifest.runtime_session_id.clone(),
        ),
        (APPFS_ATTACH_ID_ENV.to_string(), attach_id.to_string()),
    ];

    if let Some(role) = attach_role.filter(|role| !role.trim().is_empty()) {
        envs.push((APPFS_AGENT_ROLE_ENV.to_string(), role.to_string()));
    }

    envs
}

async fn wait_for_runtime_manifest_ready(
    mount_root: &Path,
    timeout: Duration,
    appfs_child: &mut Child,
) -> Result<(PathBuf, runtime_manifest::AppfsRuntimeManifestDoc)> {
    let manifest_path = runtime_manifest::runtime_manifest_path(mount_root);
    let started = Instant::now();
    let mut last_manifest_error: Option<String> = None;

    loop {
        if let Some(status) = appfs_child.try_wait()? {
            anyhow::bail!(
                "AppFS launcher child exited before runtime manifest became ready (status: {})",
                status
            );
        }

        if manifest_path.exists() {
            match std::fs::read(&manifest_path) {
                Ok(bytes) => match serde_json::from_slice(&bytes) {
                    Ok(manifest) => return Ok((manifest_path, manifest)),
                    Err(err) => {
                        last_manifest_error = Some(format!(
                            "failed to parse AppFS runtime manifest {}: {err}",
                            manifest_path.display()
                        ));
                    }
                },
                Err(err) => {
                    last_manifest_error = Some(format!(
                        "failed to read AppFS runtime manifest {}: {err}",
                        manifest_path.display()
                    ));
                }
            }
        }

        if started.elapsed() >= timeout {
            if let Some(last_error) = last_manifest_error {
                anyhow::bail!(
                    "timed out waiting for AppFS runtime manifest readiness at {} ({last_error})",
                    manifest_path.display()
                );
            }
            anyhow::bail!(
                "timed out waiting for AppFS runtime manifest readiness at {}",
                manifest_path.display()
            );
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> bool {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }

        if started.elapsed() >= timeout {
            return false;
        }

        std::thread::sleep(Duration::from_millis(100));
    }
}

fn terminate_launcher_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            if !wait_for_child_exit(child, Duration::from_secs(5)) {
                #[cfg(target_os = "windows")]
                {
                    let _ = ProcessCommand::new("taskkill")
                        .args(["/PID", &child.id().to_string(), "/T", "/F"])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status();
                    let _ = wait_for_child_exit(child, Duration::from_secs(5));
                }
            }
        }
        Err(_) => {}
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
        action_wake,
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
    let mut supervisor = AppfsRuntimeSupervisor::new(root, resolved_runtime_args, managed)?;
    supervisor.prepare_action_sinks()?;
    supervisor.sync_registry_to_disk(existing_registry.as_ref())?;
    supervisor.log_started();
    eprintln!("Press Ctrl+C to stop.");

    let fallback_poll_ms = poll_ms
        .max(DEFAULT_ACTION_FALLBACK_POLL_MS)
        .max(MIN_POLL_MS);
    let mut interval = tokio::time::interval(Duration::from_millis(fallback_poll_ms));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        if let Some(wake) = action_wake.as_ref() {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("AppFS adapter stopping...");
                    return Ok(());
                }
                _ = wake.wait() => {
                    if let Err(err) = supervisor.poll_once() {
                        eprintln!("AppFS adapter poll error: {err:#}");
                    }
                }
                _ = interval.tick() => {
                    if let Err(err) = supervisor.poll_once() {
                        eprintln!("AppFS adapter poll error: {err:#}");
                    }
                }
            }
        } else {
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
}

pub async fn handle_appfs_up_command(args: AppfsUpArgs) -> Result<()> {
    let action_wake = ActionWakeHandle::new();
    let mut mount_args = build_managed_mount_args(&args);
    mount_args.action_wake = Some(action_wake.clone());
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
        action_wake: Some(action_wake),
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

pub async fn handle_appfs_launch_command(args: AppfsLaunchArgs) -> Result<()> {
    let mut appfs_up_command = build_appfs_up_launcher_command(&args)?;
    let mut appfs_up_child = appfs_up_command
        .spawn()
        .context("failed to spawn AppFS launcher child process")?;

    let launch_result = async {
        let timeout = Duration::from_millis(args.startup_timeout_ms.max(1));
        let (manifest_path, manifest) =
            wait_for_runtime_manifest_ready(&args.mountpoint, timeout, &mut appfs_up_child).await?;

        let workspace_dir = resolve_launch_workspace_path(&manifest.mount_root, &args.workspace)?;
        std::fs::create_dir_all(&workspace_dir).with_context(|| {
            format!(
                "failed to prepare AppFS launcher workspace {}",
                workspace_dir.display()
            )
        })?;

        let attach_id = args
            .attach_id
            .clone()
            .unwrap_or_else(generate_launcher_attach_id);
        let launch_env = build_launch_agent_environment(
            &manifest_path,
            &manifest,
            &attach_id,
            args.attach_role.as_deref(),
        );

        let mut child_command = ProcessCommand::new(&args.agent_bin);
        child_command.current_dir(&workspace_dir);
        child_command.args(&args.agent_args);
        for (key, value) in launch_env {
            child_command.env(key, value);
        }

        let mut agent_child = child_command.spawn().with_context(|| {
            format!(
                "failed to spawn appfs-agent child {}",
                args.agent_bin.display()
            )
        })?;
        let status = agent_child
            .wait()
            .context("failed while waiting for appfs-agent child process")?;

        if !status.success() {
            anyhow::bail!("appfs-agent child exited with non-zero status: {status}");
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    terminate_launcher_child(&mut appfs_up_child);
    launch_result
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
    actionline_strict: bool,
    connector: Box<dyn AppConnector>,
}

#[cfg(test)]
mod supervisor_tests {
    use super::{
        build_runtime_cli_args, registry, resolve_runtime_cli_args, runtime_config,
        ActionWakeHandle, AppfsBridgeCliArgs, AppfsRuntimeSupervisor,
    };
    use serde_json::{json, Value as JsonValue};
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::time::Duration;
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

    #[test]
    fn action_wake_handle_does_not_drop_pending_signal() {
        let wake = ActionWakeHandle::new();
        wake.signal();

        crate::get_runtime().block_on(async move {
            tokio::time::timeout(Duration::from_millis(50), wake.wait())
                .await
                .expect("wake should resolve immediately when already pending");
        });
    }

    #[test]
    fn appfs_launch_builds_appfs_up_launcher_command() {
        let args = super::AppfsLaunchArgs {
            id_or_path: ".agentfs/demo.db".to_string(),
            mountpoint: std::path::PathBuf::from("C:\\mnt\\demo"),
            backend: crate::cmd::mount::MountBackend::Winfsp,
            allow_root: false,
            allow_other: true,
            uid: Some(1000),
            gid: Some(1000),
            poll_ms: 150,
            agent_bin: std::path::PathBuf::from("C:\\tools\\claw.exe"),
            workspace: std::path::PathBuf::from("workspace"),
            attach_id: Some("agent-a".to_string()),
            attach_role: Some("planner".to_string()),
            startup_timeout_ms: 15_000,
            agent_args: vec!["status".to_string()],
        };

        let command = super::build_appfs_up_launcher_command(&args)
            .expect("launcher command should be built");
        let rendered = format!("{command:?}");
        assert!(rendered.contains("appfs"));
        assert!(rendered.contains("up"));
        assert!(rendered.contains("--auto-unmount"));
        assert!(rendered.contains("--backend"));
        assert!(rendered.contains("winfsp"));
        assert!(rendered.contains("--system"));
        assert!(rendered.contains("--uid"));
        assert!(rendered.contains("--gid"));
        assert!(rendered.contains("--poll-ms"));
    }

    #[test]
    fn appfs_launch_workspace_path_stays_under_mount_root() {
        let mount_root = std::path::Path::new("C:\\mnt\\appfs");
        let workspace = std::path::Path::new("workspace\\nested");
        let resolved = super::resolve_launch_workspace_path(mount_root, workspace)
            .expect("workspace path should resolve");
        assert_eq!(
            resolved,
            std::path::PathBuf::from("C:\\mnt\\appfs").join("workspace\\nested")
        );
    }

    #[test]
    fn appfs_launch_workspace_path_rejects_parent_escape() {
        let mount_root = std::path::Path::new("C:\\mnt\\appfs");
        let workspace = std::path::Path::new("../outside");
        let err = super::resolve_launch_workspace_path(mount_root, workspace)
            .expect_err("workspace path escape must fail");
        assert!(err.to_string().contains("cannot escape"));
    }

    #[test]
    fn appfs_launch_builds_child_attach_environment_from_manifest() {
        let mount_root = std::env::temp_dir().join("appfs-launch-manifest");
        let manifest_path = super::runtime_manifest::runtime_manifest_path(&mount_root);
        let manifest =
            super::runtime_manifest::build_runtime_manifest(&mount_root, "rt-shared-01", true);
        let envs = super::build_launch_agent_environment(
            &manifest_path,
            &manifest,
            "agent-planner",
            Some("planner"),
        );
        let env_map: std::collections::HashMap<String, String> = envs.into_iter().collect();

        assert_eq!(
            env_map.get(super::APPFS_ATTACH_SCHEMA_ENV),
            Some(&"1".to_string())
        );
        assert_eq!(
            env_map.get(super::APPFS_RUNTIME_SESSION_ID_ENV),
            Some(&"rt-shared-01".to_string())
        );
        assert_eq!(
            env_map.get(super::APPFS_ATTACH_ID_ENV),
            Some(&"agent-planner".to_string())
        );
        assert_eq!(
            env_map.get(super::APPFS_AGENT_ROLE_ENV),
            Some(&"planner".to_string())
        );
        assert_eq!(
            env_map.get(super::APPFS_RUNTIME_MANIFEST_ENV),
            Some(&manifest_path.display().to_string())
        );
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
            false,
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
            false,
        )
        .expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");
        assert!(!super::runtime_manifest::runtime_manifest_path(temp.path()).exists());
        supervisor
            .sync_registry_to_disk(None)
            .expect("persist registry");

        let stored = registry::read_app_registry(temp.path())
            .expect("read registry")
            .expect("registry exists");
        assert_eq!(stored.apps.len(), 1);
        assert_eq!(stored.apps[0].app_id, "aiim");
        assert_eq!(stored.apps[0].session_id, "sess-aiim");
        let manifest_path = super::runtime_manifest::runtime_manifest_path(temp.path());
        assert!(manifest_path.exists());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).expect("read runtime manifest"))
                .expect("parse runtime manifest");
        assert!(manifest["runtime_session_id"]
            .as_str()
            .unwrap_or_default()
            .starts_with("rt-"));
        assert_eq!(
            manifest["multi_agent_mode"].as_str(),
            Some(super::runtime_manifest::APPFS_MULTI_AGENT_MODE_SHARED)
        );
    }

    #[test]
    fn supervisor_poll_does_not_rewrite_runtime_manifest_without_changes() {
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
            false,
        )
        .expect("supervisor");
        supervisor.prepare_action_sinks().expect("prepare sinks");
        supervisor
            .sync_registry_to_disk(None)
            .expect("persist registry and manifest");

        let manifest_path = super::runtime_manifest::runtime_manifest_path(temp.path());
        let before = fs::read(&manifest_path).expect("read manifest before poll");

        supervisor.poll_once().expect("poll once");

        let after = fs::read(&manifest_path).expect("read manifest after poll");
        assert_eq!(
            before, after,
            "runtime manifest should remain byte-for-byte stable when poll_once() has no state changes"
        );
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
            false,
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
            AppfsRuntimeSupervisor::new(temp.path().to_path_buf(), Vec::new(), false)
                .expect("supervisor");
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
            false,
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
