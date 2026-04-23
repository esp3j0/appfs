use crate::cmd::mount::MountBackend;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

const COMPOSE_VERSION: u32 = 1;
const DEFAULT_HTTP_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_GRPC_TIMEOUT_MS: u64 = 5_000;
const DEFAULT_BRIDGE_MAX_RETRIES: u32 = 2;
const DEFAULT_BRIDGE_INITIAL_BACKOFF_MS: u64 = 100;
const DEFAULT_BRIDGE_MAX_BACKOFF_MS: u64 = 1_000;
const DEFAULT_BRIDGE_CIRCUIT_BREAKER_FAILURES: u32 = 5;
const DEFAULT_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS: u64 = 3_000;
const DEFAULT_HEALTHCHECK_INTERVAL_MS: u64 = 1_000;
const DEFAULT_HEALTHCHECK_TIMEOUT_MS: u64 = 3_000;
const DEFAULT_HEALTHCHECK_MAX_ATTEMPTS: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeDoc {
    pub(crate) source_path: PathBuf,
    pub(crate) base_dir: PathBuf,
    pub(crate) version: u32,
    pub(crate) name: Option<String>,
    pub(crate) runtime: AppfsComposeRuntime,
    pub(crate) connectors: BTreeMap<String, AppfsComposeConnector>,
    pub(crate) apps: BTreeMap<String, AppfsComposeApp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeRuntime {
    pub(crate) db: PathBuf,
    pub(crate) mountpoint: PathBuf,
    pub(crate) backend: MountBackend,
    pub(crate) init: AppfsComposeInitMode,
    pub(crate) reset: bool,
    pub(crate) auto_unmount: bool,
    pub(crate) allow_root: bool,
    pub(crate) allow_other: bool,
    pub(crate) uid: Option<u32>,
    pub(crate) gid: Option<u32>,
    pub(crate) poll_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppfsComposeInitMode {
    IfMissing,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeConnector {
    pub(crate) mode: AppfsComposeConnectorMode,
    pub(crate) transport: AppfsComposeTransportKind,
    pub(crate) endpoint: String,
    pub(crate) healthcheck: Option<AppfsComposeConnectorHealthcheck>,
    pub(crate) command: Option<AppfsComposeCommand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppfsComposeConnectorMode {
    External,
    Command,
    ExternalOrCommand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppfsComposeTransportKind {
    Http,
    Grpc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeConnectorHealthcheck {
    pub(crate) kind: AppfsComposeHealthcheckKind,
    pub(crate) interval_ms: u64,
    pub(crate) timeout_ms: u64,
    pub(crate) max_attempts: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppfsComposeHealthcheckKind {
    Connector,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeCommand {
    pub(crate) cwd: PathBuf,
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<String>,
    pub(crate) env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeApp {
    pub(crate) connector: String,
    pub(crate) session_id: Option<String>,
    pub(crate) transport: AppfsComposeAppTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppfsComposeAppTransport {
    pub(crate) http_timeout_ms: u64,
    pub(crate) grpc_timeout_ms: u64,
    pub(crate) bridge_max_retries: u32,
    pub(crate) bridge_initial_backoff_ms: u64,
    pub(crate) bridge_max_backoff_ms: u64,
    pub(crate) bridge_circuit_breaker_failures: u32,
    pub(crate) bridge_circuit_breaker_cooldown_ms: u64,
}

impl Default for AppfsComposeAppTransport {
    fn default() -> Self {
        Self {
            http_timeout_ms: DEFAULT_HTTP_TIMEOUT_MS,
            grpc_timeout_ms: DEFAULT_GRPC_TIMEOUT_MS,
            bridge_max_retries: DEFAULT_BRIDGE_MAX_RETRIES,
            bridge_initial_backoff_ms: DEFAULT_BRIDGE_INITIAL_BACKOFF_MS,
            bridge_max_backoff_ms: DEFAULT_BRIDGE_MAX_BACKOFF_MS,
            bridge_circuit_breaker_failures: DEFAULT_BRIDGE_CIRCUIT_BREAKER_FAILURES,
            bridge_circuit_breaker_cooldown_ms: DEFAULT_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS,
        }
    }
}

pub(crate) fn parse_compose_doc(yaml: &str, source_path: PathBuf) -> Result<AppfsComposeDoc> {
    let raw: RawAppfsComposeDoc =
        serde_yaml::from_str(yaml).context("failed to parse AppFS compose YAML")?;
    normalize_compose_doc(raw, source_path)
}

fn normalize_compose_doc(raw: RawAppfsComposeDoc, source_path: PathBuf) -> Result<AppfsComposeDoc> {
    if raw.version != COMPOSE_VERSION {
        anyhow::bail!(
            "unsupported AppFS compose version {} (expected {})",
            raw.version,
            COMPOSE_VERSION
        );
    }

    let base_dir = source_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "compose source path {} does not have a parent directory",
            source_path.display()
        )
    })?;
    let base_dir = base_dir.to_path_buf();

    let runtime = normalize_runtime(raw.runtime, &base_dir)?;
    let connectors = normalize_connectors(raw.connectors, &base_dir)?;
    let apps = normalize_apps(raw.apps, &connectors)?;
    let name = normalize_optional_string(raw.name, "compose name")?;

    Ok(AppfsComposeDoc {
        source_path,
        base_dir,
        version: COMPOSE_VERSION,
        name,
        runtime,
        connectors,
        apps,
    })
}

fn normalize_runtime(raw: RawAppfsComposeRuntime, base_dir: &Path) -> Result<AppfsComposeRuntime> {
    Ok(AppfsComposeRuntime {
        db: normalize_required_path(raw.db, base_dir, "runtime.db")?,
        mountpoint: normalize_required_path(raw.mountpoint, base_dir, "runtime.mountpoint")?,
        backend: normalize_backend(raw.backend)?,
        init: normalize_init_mode(raw.init)?,
        reset: raw.reset,
        auto_unmount: raw.auto_unmount,
        allow_root: raw.allow_root,
        allow_other: raw.system,
        uid: raw.uid,
        gid: raw.gid,
        poll_ms: raw.poll_ms,
    })
}

fn normalize_connectors(
    raw_connectors: BTreeMap<String, RawAppfsComposeConnector>,
    base_dir: &Path,
) -> Result<BTreeMap<String, AppfsComposeConnector>> {
    let mut connectors = BTreeMap::new();
    for (raw_name, raw_connector) in raw_connectors {
        let name = normalize_map_key(&raw_name, "connector")?;
        if connectors.contains_key(&name) {
            anyhow::bail!("duplicate compose connector {}", name);
        }
        let connector = normalize_connector(&name, raw_connector, base_dir)?;
        connectors.insert(name, connector);
    }
    Ok(connectors)
}

fn normalize_connector(
    connector_name: &str,
    raw: RawAppfsComposeConnector,
    base_dir: &Path,
) -> Result<AppfsComposeConnector> {
    let mode = normalize_connector_mode(raw.mode)?;
    let transport = normalize_transport_kind(raw.transport)?;
    let endpoint = normalize_required_string(
        raw.endpoint,
        &format!("connectors.{connector_name}.endpoint"),
    )?;

    let command = raw
        .command
        .map(|command| normalize_command(connector_name, command, base_dir))
        .transpose()?;
    match mode {
        AppfsComposeConnectorMode::External => {
            if command.is_some() {
                anyhow::bail!(
                    "compose connector {} in external mode cannot define command",
                    connector_name
                );
            }
        }
        AppfsComposeConnectorMode::Command | AppfsComposeConnectorMode::ExternalOrCommand => {
            if command.is_none() {
                anyhow::bail!(
                    "compose connector {} in {} mode requires command",
                    connector_name,
                    connector_mode_label(mode)
                );
            }
        }
    }
    let healthcheck = raw
        .healthcheck
        .map(|healthcheck| normalize_healthcheck(connector_name, healthcheck))
        .transpose()?;

    match transport {
        AppfsComposeTransportKind::Http | AppfsComposeTransportKind::Grpc => {}
    }

    Ok(AppfsComposeConnector {
        mode,
        transport,
        endpoint,
        healthcheck,
        command,
    })
}

fn normalize_command(
    connector_name: &str,
    raw: RawAppfsComposeCommand,
    base_dir: &Path,
) -> Result<AppfsComposeCommand> {
    let cwd = match raw.cwd {
        Some(path) => normalize_required_path(
            path,
            base_dir,
            &format!("connectors.{connector_name}.command.cwd"),
        )?,
        None => base_dir.to_path_buf(),
    };
    let program = raw.program.ok_or_else(|| {
        anyhow::anyhow!(
            "compose connector {} command.program is required",
            connector_name
        )
    })?;
    if program.as_os_str().is_empty() {
        anyhow::bail!(
            "compose connector {} command.program cannot be empty",
            connector_name
        );
    }
    let env = normalize_env_map(connector_name, raw.env)?;

    Ok(AppfsComposeCommand {
        cwd,
        program,
        args: raw.args,
        env,
    })
}

fn normalize_env_map(
    connector_name: &str,
    raw_env: BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>> {
    let mut env = BTreeMap::new();
    for (raw_key, raw_value) in raw_env {
        let key = normalize_map_key(
            &raw_key,
            &format!("connectors.{connector_name}.command.env"),
        )?;
        if raw_value.trim().is_empty() {
            anyhow::bail!(
                "compose connector {} command.env.{} cannot be empty",
                connector_name,
                key
            );
        }
        env.insert(key, raw_value);
    }
    Ok(env)
}

fn normalize_healthcheck(
    connector_name: &str,
    raw: RawAppfsComposeHealthcheck,
) -> Result<AppfsComposeConnectorHealthcheck> {
    let kind = normalize_healthcheck_kind(raw.kind)?;
    let interval_ms = raw.interval_ms.unwrap_or(DEFAULT_HEALTHCHECK_INTERVAL_MS);
    let timeout_ms = raw.timeout_ms.unwrap_or(DEFAULT_HEALTHCHECK_TIMEOUT_MS);
    let max_attempts = raw.max_attempts.unwrap_or(DEFAULT_HEALTHCHECK_MAX_ATTEMPTS);
    if interval_ms == 0 {
        anyhow::bail!(
            "compose connector {} healthcheck.interval_ms must be > 0",
            connector_name
        );
    }
    if timeout_ms == 0 {
        anyhow::bail!(
            "compose connector {} healthcheck.timeout_ms must be > 0",
            connector_name
        );
    }
    if max_attempts == 0 {
        anyhow::bail!(
            "compose connector {} healthcheck.max_attempts must be > 0",
            connector_name
        );
    }
    Ok(AppfsComposeConnectorHealthcheck {
        kind,
        interval_ms,
        timeout_ms,
        max_attempts,
    })
}

fn normalize_apps(
    raw_apps: BTreeMap<String, RawAppfsComposeApp>,
    connectors: &BTreeMap<String, AppfsComposeConnector>,
) -> Result<BTreeMap<String, AppfsComposeApp>> {
    let mut apps = BTreeMap::new();
    let mut connector_usage = BTreeMap::<String, Vec<String>>::new();
    for (raw_app_id, raw_app) in raw_apps {
        let app_id = normalize_map_key(&raw_app_id, "app")?;
        if apps.contains_key(&app_id) {
            anyhow::bail!("duplicate compose app {}", app_id);
        }
        let connector = normalize_required_string(
            Some(raw_app.connector),
            &format!("apps.{app_id}.connector"),
        )?;
        if !connectors.contains_key(&connector) {
            anyhow::bail!(
                "compose app {} references unknown connector {}",
                app_id,
                connector
            );
        }
        let session_id =
            normalize_optional_string(raw_app.session_id, format!("apps.{app_id}.session_id"))?;
        connector_usage
            .entry(connector.clone())
            .or_default()
            .push(app_id.clone());
        apps.insert(
            app_id,
            AppfsComposeApp {
                connector,
                session_id,
                transport: normalize_app_transport(raw_app.transport),
            },
        );
    }
    for (connector, app_ids) in connector_usage {
        if app_ids.len() > 1 {
            anyhow::bail!(
                "compose connector {} is referenced by multiple apps ({}) but v1 requires one connector per app",
                connector,
                app_ids.join(", ")
            );
        }
    }
    Ok(apps)
}

fn normalize_app_transport(raw: RawAppfsComposeAppTransport) -> AppfsComposeAppTransport {
    AppfsComposeAppTransport {
        http_timeout_ms: raw.http_timeout_ms.unwrap_or(DEFAULT_HTTP_TIMEOUT_MS),
        grpc_timeout_ms: raw.grpc_timeout_ms.unwrap_or(DEFAULT_GRPC_TIMEOUT_MS),
        bridge_max_retries: raw.bridge_max_retries.unwrap_or(DEFAULT_BRIDGE_MAX_RETRIES),
        bridge_initial_backoff_ms: raw
            .bridge_initial_backoff_ms
            .unwrap_or(DEFAULT_BRIDGE_INITIAL_BACKOFF_MS),
        bridge_max_backoff_ms: raw
            .bridge_max_backoff_ms
            .unwrap_or(DEFAULT_BRIDGE_MAX_BACKOFF_MS),
        bridge_circuit_breaker_failures: raw
            .bridge_circuit_breaker_failures
            .unwrap_or(DEFAULT_BRIDGE_CIRCUIT_BREAKER_FAILURES),
        bridge_circuit_breaker_cooldown_ms: raw
            .bridge_circuit_breaker_cooldown_ms
            .unwrap_or(DEFAULT_BRIDGE_CIRCUIT_BREAKER_COOLDOWN_MS),
    }
}

fn normalize_required_path(raw: PathBuf, base_dir: &Path, field_name: &str) -> Result<PathBuf> {
    if raw.as_os_str().is_empty() {
        anyhow::bail!("{field_name} cannot be empty");
    }
    let path = if raw.is_absolute() {
        raw
    } else {
        base_dir.join(raw)
    };
    Ok(lexically_normalize_path(path))
}

fn normalize_required_string(raw: Option<String>, field_name: &str) -> Result<String> {
    let value = raw.ok_or_else(|| anyhow::anyhow!("{field_name} is required"))?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{field_name} cannot be empty");
    }
    if trimmed != value {
        anyhow::bail!("{field_name} cannot contain leading or trailing whitespace");
    }
    Ok(value)
}

fn normalize_optional_string(
    raw: Option<String>,
    field_name: impl AsRef<str>,
) -> Result<Option<String>> {
    match raw {
        Some(value) => Ok(Some(normalize_required_string(
            Some(value),
            field_name.as_ref(),
        )?)),
        None => Ok(None),
    }
}

fn normalize_map_key(raw: &str, entity_name: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        anyhow::bail!("compose {entity_name} key cannot be empty");
    }
    if trimmed != raw {
        anyhow::bail!(
            "compose {entity_name} key {:?} cannot contain leading or trailing whitespace",
            raw
        );
    }
    Ok(raw.to_string())
}

fn lexically_normalize_path(path: PathBuf) -> PathBuf {
    let mut prefix: Option<OsString> = None;
    let mut has_root = false;
    let mut segments: Vec<OsString> = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(value) => {
                prefix = Some(value.as_os_str().to_os_string());
            }
            std::path::Component::RootDir => {
                has_root = true;
                segments.clear();
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(segments.last(), Some(last) if last != "..") {
                    segments.pop();
                } else if !has_root {
                    segments.push(OsString::from(".."));
                }
            }
            std::path::Component::Normal(value) => segments.push(value.to_os_string()),
        }
    }

    let mut normalized = match prefix {
        Some(prefix) => PathBuf::from(prefix),
        None => PathBuf::new(),
    };
    if has_root {
        normalized.push(Path::new(std::path::MAIN_SEPARATOR_STR));
    }
    for segment in segments {
        normalized.push(segment);
    }
    normalized
}

fn normalize_backend(raw: RawMountBackend) -> Result<MountBackend> {
    Ok(match raw {
        RawMountBackend::Fuse => MountBackend::Fuse,
        RawMountBackend::Nfs => MountBackend::Nfs,
        RawMountBackend::Winfsp => MountBackend::Winfsp,
    })
}

fn normalize_init_mode(raw: RawAppfsComposeInitMode) -> Result<AppfsComposeInitMode> {
    Ok(match raw {
        RawAppfsComposeInitMode::IfMissing => AppfsComposeInitMode::IfMissing,
        RawAppfsComposeInitMode::Always => AppfsComposeInitMode::Always,
        RawAppfsComposeInitMode::Never => AppfsComposeInitMode::Never,
    })
}

fn normalize_connector_mode(
    raw: RawAppfsComposeConnectorMode,
) -> Result<AppfsComposeConnectorMode> {
    Ok(match raw {
        RawAppfsComposeConnectorMode::External => AppfsComposeConnectorMode::External,
        RawAppfsComposeConnectorMode::Command => AppfsComposeConnectorMode::Command,
        RawAppfsComposeConnectorMode::ExternalOrCommand => {
            AppfsComposeConnectorMode::ExternalOrCommand
        }
    })
}

fn connector_mode_label(mode: AppfsComposeConnectorMode) -> &'static str {
    match mode {
        AppfsComposeConnectorMode::External => "external",
        AppfsComposeConnectorMode::Command => "command",
        AppfsComposeConnectorMode::ExternalOrCommand => "external_or_command",
    }
}

fn normalize_transport_kind(
    raw: RawAppfsComposeTransportKind,
) -> Result<AppfsComposeTransportKind> {
    Ok(match raw {
        RawAppfsComposeTransportKind::Http => AppfsComposeTransportKind::Http,
        RawAppfsComposeTransportKind::Grpc => AppfsComposeTransportKind::Grpc,
    })
}

fn normalize_healthcheck_kind(
    raw: Option<RawAppfsComposeHealthcheckKind>,
) -> Result<AppfsComposeHealthcheckKind> {
    Ok(
        match raw.unwrap_or(RawAppfsComposeHealthcheckKind::Connector) {
            RawAppfsComposeHealthcheckKind::Connector => AppfsComposeHealthcheckKind::Connector,
        },
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeDoc {
    version: u32,
    #[serde(default)]
    name: Option<String>,
    runtime: RawAppfsComposeRuntime,
    #[serde(default)]
    connectors: BTreeMap<String, RawAppfsComposeConnector>,
    #[serde(default)]
    apps: BTreeMap<String, RawAppfsComposeApp>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeRuntime {
    db: PathBuf,
    mountpoint: PathBuf,
    backend: RawMountBackend,
    #[serde(default)]
    init: RawAppfsComposeInitMode,
    #[serde(default)]
    reset: bool,
    #[serde(default = "default_true")]
    auto_unmount: bool,
    #[serde(default)]
    allow_root: bool,
    #[serde(default)]
    system: bool,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    gid: Option<u32>,
    #[serde(default)]
    poll_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawMountBackend {
    Fuse,
    Nfs,
    Winfsp,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum RawAppfsComposeInitMode {
    #[default]
    IfMissing,
    Always,
    Never,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeConnector {
    mode: RawAppfsComposeConnectorMode,
    transport: RawAppfsComposeTransportKind,
    endpoint: Option<String>,
    #[serde(default)]
    healthcheck: Option<RawAppfsComposeHealthcheck>,
    #[serde(default)]
    command: Option<RawAppfsComposeCommand>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawAppfsComposeConnectorMode {
    External,
    Command,
    ExternalOrCommand,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawAppfsComposeTransportKind {
    Http,
    Grpc,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeHealthcheck {
    #[serde(default)]
    kind: Option<RawAppfsComposeHealthcheckKind>,
    #[serde(default)]
    interval_ms: Option<u64>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    max_attempts: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RawAppfsComposeHealthcheckKind {
    Connector,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeCommand {
    #[serde(default)]
    cwd: Option<PathBuf>,
    program: Option<PathBuf>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeApp {
    connector: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    transport: RawAppfsComposeAppTransport,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct RawAppfsComposeAppTransport {
    #[serde(default)]
    http_timeout_ms: Option<u64>,
    #[serde(default)]
    grpc_timeout_ms: Option<u64>,
    #[serde(default)]
    bridge_max_retries: Option<u32>,
    #[serde(default)]
    bridge_initial_backoff_ms: Option<u64>,
    #[serde(default)]
    bridge_max_backoff_ms: Option<u64>,
    #[serde(default)]
    bridge_circuit_breaker_failures: Option<u32>,
    #[serde(default)]
    bridge_circuit_breaker_cooldown_ms: Option<u64>,
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::{
        parse_compose_doc, AppfsComposeConnectorMode, AppfsComposeInitMode,
        AppfsComposeTransportKind,
    };
    use std::path::PathBuf;

    #[test]
    fn parses_and_normalizes_relative_compose_paths() {
        let source_path = if cfg!(windows) {
            PathBuf::from(r"C:\work\agentfs\appfs-compose.yaml")
        } else {
            PathBuf::from("/work/agentfs/appfs-compose.yaml")
        };
        let doc = parse_compose_doc(
            r#"
version: 1
name: local-demo
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: winfsp
connectors:
  demo-http:
    mode: external_or_command
    transport: http
    endpoint: http://127.0.0.1:8080
    command:
      cwd: ./examples/appfs/bridges/http-python
      program: uv
      args: ["run", "python", "bridge_server.py"]
      env:
        APPFS_HTTP_BRIDGE_BACKEND: aiim
apps:
  aiim:
    connector: demo-http
"#,
            source_path,
        )
        .expect("compose should parse");

        assert_eq!(doc.version, 1);
        assert_eq!(doc.name.as_deref(), Some("local-demo"));
        let connector = doc.connectors.get("demo-http").expect("connector");
        if cfg!(windows) {
            assert_eq!(
                doc.runtime.db,
                PathBuf::from(r"C:\work\agentfs\.agentfs\demo.db")
            );
            assert_eq!(
                doc.runtime.mountpoint,
                PathBuf::from(r"C:\work\agentfs\mnt\appfs")
            );
            assert_eq!(
                connector.command.as_ref().expect("command").cwd,
                PathBuf::from(r"C:\work\agentfs\examples\appfs\bridges\http-python")
            );
        } else {
            assert_eq!(
                doc.runtime.db,
                PathBuf::from("/work/agentfs/.agentfs/demo.db")
            );
            assert_eq!(
                doc.runtime.mountpoint,
                PathBuf::from("/work/agentfs/mnt/appfs")
            );
            assert_eq!(
                connector.command.as_ref().expect("command").cwd,
                PathBuf::from("/work/agentfs/examples/appfs/bridges/http-python")
            );
        }
        assert_eq!(doc.runtime.init, AppfsComposeInitMode::IfMissing);
        assert!(doc.runtime.auto_unmount);
        assert_eq!(connector.mode, AppfsComposeConnectorMode::ExternalOrCommand);
        assert_eq!(connector.transport, AppfsComposeTransportKind::Http);
        assert_eq!(connector.endpoint, "http://127.0.0.1:8080");
        assert_eq!(
            doc.apps.get("aiim").expect("app").transport.http_timeout_ms,
            5000
        );
    }

    #[test]
    fn rejects_unknown_connector_reference() {
        let err = parse_compose_doc(
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
apps:
  aiim:
    connector: missing
"#,
            PathBuf::from("/tmp/appfs-compose.yaml"),
        )
        .expect_err("compose should fail");

        assert!(err.to_string().contains("unknown connector"));
    }

    #[test]
    fn rejects_missing_command_for_command_mode() {
        let err = parse_compose_doc(
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
connectors:
  demo-http:
    mode: command
    transport: http
    endpoint: http://127.0.0.1:8080
"#,
            PathBuf::from("/tmp/appfs-compose.yaml"),
        )
        .expect_err("compose should fail");

        assert!(err.to_string().contains("requires command"));
    }

    #[test]
    fn rejects_missing_endpoint_for_command_mode() {
        let err = parse_compose_doc(
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
connectors:
  demo-http:
    mode: command
    transport: http
    command:
      program: uv
"#,
            PathBuf::from("/tmp/appfs-compose.yaml"),
        )
        .expect_err("compose should fail");

        assert!(err.to_string().contains("endpoint is required"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let err = parse_compose_doc(
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
  typo: true
"#,
            PathBuf::from("/tmp/appfs-compose.yaml"),
        )
        .expect_err("compose should fail");

        assert!(format!("{err:#}").contains("unknown field"));
    }

    #[test]
    fn rejects_multiple_apps_sharing_one_connector() {
        let err = parse_compose_doc(
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
connectors:
  shared-http:
    mode: external
    transport: http
    endpoint: http://127.0.0.1:8080
apps:
  aiim:
    connector: shared-http
  huoyan:
    connector: shared-http
"#,
            PathBuf::from("/tmp/appfs-compose.yaml"),
        )
        .expect_err("compose should fail");

        assert!(err.to_string().contains("requires one connector per app"));
    }

    #[test]
    fn preserves_absolute_runtime_mountpoint() {
        let yaml = if cfg!(windows) {
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: C:\mnt\appfs
  backend: winfsp
"#
        } else {
            r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: /tmp/appfs
  backend: fuse
"#
        };
        let source_path = if cfg!(windows) {
            PathBuf::from(r"C:\work\agentfs\appfs-compose.yaml")
        } else {
            PathBuf::from("/work/agentfs/appfs-compose.yaml")
        };

        let doc = parse_compose_doc(yaml, source_path).expect("compose should parse");

        if cfg!(windows) {
            assert_eq!(doc.runtime.mountpoint, PathBuf::from(r"C:\mnt\appfs"));
        } else {
            assert_eq!(doc.runtime.mountpoint, PathBuf::from("/tmp/appfs"));
        }
    }
}
