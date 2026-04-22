use super::schema::{
    AppfsComposeAppTransport, AppfsComposeConnector, AppfsComposeConnectorHealthcheck,
    AppfsComposeConnectorMode, AppfsComposeDoc, AppfsComposeTransportKind,
};
use crate::cmd::appfs::core::build_app_connector;
use crate::cmd::appfs::{build_appfs_bridge_config, AppfsBridgeCliArgs};
use agentfs_sdk::{AuthStatus, ConnectorContext};
use anyhow::{Context, Result};
use std::collections::BTreeMap;
use std::process::{Child, Command as ProcessCommand};
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const DEFAULT_HEALTHCHECK_INTERVAL_MS: u64 = 1_000;
const DEFAULT_HEALTHCHECK_TIMEOUT_MS: u64 = 3_000;
const DEFAULT_HEALTHCHECK_MAX_ATTEMPTS: u32 = 20;

#[derive(Debug)]
pub(crate) struct ComposeConnectorSupervisor {
    owned_children: Vec<ComposeOwnedChild>,
}

#[derive(Debug)]
struct ComposeOwnedChild {
    connector_name: String,
    child: Child,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedComposeApp {
    pub(crate) app_id: String,
    pub(crate) connector_name: String,
    pub(crate) transport_kind: AppfsComposeTransportKind,
    pub(crate) endpoint: String,
    pub(crate) session_id: Option<String>,
    pub(crate) transport: AppfsComposeAppTransport,
}

#[derive(Debug, Clone, Copy)]
struct ResolvedHealthcheck {
    interval_ms: u64,
    timeout_ms: u64,
    max_attempts: u32,
}

impl ComposeConnectorSupervisor {
    pub(crate) fn resolve_apps(
        compose: &AppfsComposeDoc,
    ) -> Result<(Self, BTreeMap<String, ResolvedComposeApp>)> {
        let mut supervisor = Self {
            owned_children: Vec::new(),
        };
        let mut resolved_apps = BTreeMap::new();

        for (app_id, app) in &compose.apps {
            let connector = compose.connectors.get(&app.connector).ok_or_else(|| {
                anyhow::anyhow!(
                    "compose app {} references unknown connector {}",
                    app_id,
                    app.connector
                )
            })?;
            let resolved = match supervisor.resolve_app_connector(app_id, app, connector) {
                Ok(resolved) => resolved,
                Err(err) => {
                    supervisor.shutdown();
                    return Err(err);
                }
            };
            resolved_apps.insert(app_id.clone(), resolved);
        }

        Ok((supervisor, resolved_apps))
    }

    pub(crate) fn shutdown(&mut self) {
        for owned in self.owned_children.iter_mut().rev() {
            terminate_child_process(&mut owned.child);
        }
        self.owned_children.clear();
    }

    #[cfg(test)]
    fn owned_pids(&self) -> Vec<u32> {
        self.owned_children
            .iter()
            .map(|owned| owned.child.id())
            .collect()
    }

    fn resolve_app_connector(
        &mut self,
        app_id: &str,
        app: &super::schema::AppfsComposeApp,
        connector: &AppfsComposeConnector,
    ) -> Result<ResolvedComposeApp> {
        let healthcheck = resolve_healthcheck(connector.healthcheck.as_ref());
        match connector.mode {
            AppfsComposeConnectorMode::External => {
                wait_for_connector_ready(app_id, connector, healthcheck)?;
            }
            AppfsComposeConnectorMode::Command => {
                let mut child = spawn_connector_child(&app.connector, connector)
                    .with_context(|| format!("failed to launch connector for app {app_id}"))?;
                if let Err(err) = wait_for_connector_ready(app_id, connector, healthcheck) {
                    terminate_child_process(&mut child);
                    return Err(err);
                }
                self.owned_children.push(ComposeOwnedChild {
                    connector_name: app.connector.clone(),
                    child,
                });
            }
            AppfsComposeConnectorMode::ExternalOrCommand => {
                if probe_connector_once(app_id, connector, healthcheck.timeout_ms).is_err() {
                    let mut child =
                        spawn_connector_child(&app.connector, connector).with_context(|| {
                            format!("failed to launch fallback connector for app {app_id}")
                        })?;
                    if let Err(err) = wait_for_connector_ready(app_id, connector, healthcheck) {
                        terminate_child_process(&mut child);
                        return Err(err);
                    }
                    self.owned_children.push(ComposeOwnedChild {
                        connector_name: app.connector.clone(),
                        child,
                    });
                }
            }
        }

        Ok(ResolvedComposeApp {
            app_id: app_id.to_string(),
            connector_name: app.connector.clone(),
            transport_kind: connector.transport,
            endpoint: connector.endpoint.clone(),
            session_id: app.session_id.clone(),
            transport: app.transport.clone(),
        })
    }
}

impl Drop for ComposeConnectorSupervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn resolve_healthcheck(
    healthcheck: Option<&AppfsComposeConnectorHealthcheck>,
) -> ResolvedHealthcheck {
    ResolvedHealthcheck {
        interval_ms: healthcheck
            .map(|healthcheck| healthcheck.interval_ms)
            .unwrap_or(DEFAULT_HEALTHCHECK_INTERVAL_MS),
        timeout_ms: healthcheck
            .map(|healthcheck| healthcheck.timeout_ms)
            .unwrap_or(DEFAULT_HEALTHCHECK_TIMEOUT_MS),
        max_attempts: healthcheck
            .map(|healthcheck| healthcheck.max_attempts)
            .unwrap_or(DEFAULT_HEALTHCHECK_MAX_ATTEMPTS),
    }
}

fn wait_for_connector_ready(
    app_id: &str,
    connector: &AppfsComposeConnector,
    healthcheck: ResolvedHealthcheck,
) -> Result<()> {
    let started = Instant::now();
    let mut last_error = None;
    for attempt in 1..=healthcheck.max_attempts.max(1) {
        match probe_connector_once(app_id, connector, healthcheck.timeout_ms) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_error = Some(err);
                if attempt < healthcheck.max_attempts.max(1) {
                    thread::sleep(Duration::from_millis(healthcheck.interval_ms.max(1)));
                }
            }
        }
    }
    let last_error = last_error.unwrap_or_else(|| {
        anyhow::anyhow!(
            "connector {} for app {} did not become ready",
            connector.endpoint,
            app_id
        )
    });
    Err(anyhow::anyhow!(
        "connector {} for app {} did not become ready within {} ms: {last_error:#}",
        connector.endpoint,
        app_id,
        started.elapsed().as_millis()
    ))
}

fn probe_connector_once(
    app_id: &str,
    connector: &AppfsComposeConnector,
    timeout_ms: u64,
) -> Result<()> {
    let bridge_config = build_health_bridge_config(connector, timeout_ms);
    let mut connector_client = build_app_connector(app_id, &bridge_config).with_context(|| {
        format!(
            "failed to initialize connector for app {} at {}",
            app_id, connector.endpoint
        )
    })?;
    let ctx = ConnectorContext {
        app_id: app_id.to_string(),
        session_id: format!("compose-health-{}", app_id),
        request_id: format!("compose-health-{}", Uuid::new_v4().simple()),
        client_token: None,
        trace_id: None,
    };
    let health = connector_client
        .health(&ctx)
        .map_err(|err| anyhow::anyhow!("connector health failed: {}: {}", err.code, err.message))?;
    if !health.healthy {
        anyhow::bail!(
            "connector reported unhealthy auth_status={:?} message={}",
            health.auth_status,
            health.message.unwrap_or_else(|| "<none>".to_string())
        );
    }
    if matches!(
        health.auth_status,
        AuthStatus::Expired | AuthStatus::Invalid
    ) {
        anyhow::bail!("connector auth is not ready: {:?}", health.auth_status);
    }
    Ok(())
}

fn build_health_bridge_config(
    connector: &AppfsComposeConnector,
    timeout_ms: u64,
) -> crate::cmd::appfs::AppfsBridgeConfig {
    build_appfs_bridge_config(AppfsBridgeCliArgs {
        adapter_http_endpoint: match connector.transport {
            AppfsComposeTransportKind::Http => Some(connector.endpoint.clone()),
            AppfsComposeTransportKind::Grpc => None,
        },
        adapter_http_timeout_ms: timeout_ms.max(1),
        adapter_grpc_endpoint: match connector.transport {
            AppfsComposeTransportKind::Http => None,
            AppfsComposeTransportKind::Grpc => Some(connector.endpoint.clone()),
        },
        adapter_grpc_timeout_ms: timeout_ms.max(1),
        adapter_bridge_max_retries: 0,
        adapter_bridge_initial_backoff_ms: 1,
        adapter_bridge_max_backoff_ms: 1,
        adapter_bridge_circuit_breaker_failures: 0,
        adapter_bridge_circuit_breaker_cooldown_ms: 1,
    })
}

fn spawn_connector_child(connector_name: &str, connector: &AppfsComposeConnector) -> Result<Child> {
    let command = connector.command.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "compose connector {} requires command but none was configured",
            connector_name
        )
    })?;
    let mut process = ProcessCommand::new(&command.program);
    process.current_dir(&command.cwd);
    process.args(&command.args);
    for (key, value) in &command.env {
        process.env(key, value);
    }
    process.spawn().with_context(|| {
        format!(
            "failed to spawn compose connector {} via {}",
            connector_name,
            command.program.display()
        )
    })
}

fn terminate_child_process(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => {}
        Ok(None) => {
            let _ = child.kill();
            if !wait_for_child_exit(child, Duration::from_secs(5)) {
                #[cfg(target_os = "windows")]
                {
                    let _ = ProcessCommand::new("taskkill")
                        .args(["/PID", &child.id().to_string(), "/T", "/F"])
                        .status();
                    let _ = wait_for_child_exit(child, Duration::from_secs(5));
                }
            }
        }
        Err(_) => {}
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
        thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(test)]
mod tests {
    use super::{ComposeConnectorSupervisor, ResolvedComposeApp};
    use crate::cmd::appfs::compose::schema::parse_compose_doc;
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};
    use tempfile::TempDir;

    #[test]
    fn resolves_external_http_connector() {
        let server = MockHttpConnectorServer::spawn("aiim");
        let temp = TempDir::new().expect("tempdir");
        let doc = parse_compose_doc(
            &format!(
                r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
connectors:
  aiim-http:
    mode: external
    transport: http
    endpoint: {}
    healthcheck:
      interval_ms: 20
      timeout_ms: 100
      max_attempts: 5
apps:
  aiim:
    connector: aiim-http
"#,
                server.endpoint()
            ),
            temp.path().join("appfs-compose.yaml"),
        )
        .expect("compose should parse");

        let (_supervisor, resolved) =
            ComposeConnectorSupervisor::resolve_apps(&doc).expect("compose should resolve");

        let resolved_aiim = resolved.get("aiim").expect("resolved app");
        assert_eq!(
            resolved_aiim,
            &ResolvedComposeApp {
                app_id: "aiim".to_string(),
                connector_name: "aiim-http".to_string(),
                transport_kind: crate::cmd::appfs::compose::schema::AppfsComposeTransportKind::Http,
                endpoint: server.endpoint(),
                session_id: None,
                transport: Default::default(),
            }
        );
    }

    #[test]
    fn launches_command_connector_and_shuts_it_down() {
        let temp = TempDir::new().expect("tempdir");
        let signal_path = temp.path().join("connector-started.txt");
        let port = reserve_test_port();
        let endpoint = format!("http://127.0.0.1:{port}");
        let server_started = Arc::new(AtomicBool::new(false));
        let server_signal = signal_path.clone();
        let server_started_flag = server_started.clone();
        let server_thread = thread::spawn(move || {
            wait_for_path(&server_signal, Duration::from_secs(5)).expect("wait for child signal");
            let _server = MockHttpConnectorServer::bind_at("aiim", port);
            server_started_flag.store(true, Ordering::SeqCst);
            thread::sleep(Duration::from_secs(2));
        });

        let doc = parse_compose_doc(
            &format!(
                r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
connectors:
  aiim-http:
    mode: command
    transport: http
    endpoint: {endpoint}
    healthcheck:
      interval_ms: 20
      timeout_ms: 100
      max_attempts: 50
    command:
      program: {program}
      args: {args}
apps:
  aiim:
    connector: aiim-http
"#,
                program = shell_program_literal(),
                args = shell_args_yaml(&signal_path),
            ),
            temp.path().join("appfs-compose.yaml"),
        )
        .expect("compose should parse");

        let (mut supervisor, resolved) =
            ComposeConnectorSupervisor::resolve_apps(&doc).expect("compose should resolve");
        assert!(signal_path.exists());
        assert!(server_started.load(Ordering::SeqCst));
        assert_eq!(resolved.get("aiim").expect("resolved").endpoint, endpoint);
        let owned_pids = supervisor.owned_pids();
        assert_eq!(owned_pids.len(), 1);

        supervisor.shutdown();
        wait_for_process_exit(owned_pids[0], Duration::from_secs(5))
            .expect("compose-owned child should exit");
        server_thread.join().expect("server thread");
    }

    #[test]
    fn external_or_command_falls_back_to_command() {
        let temp = TempDir::new().expect("tempdir");
        let signal_path = temp.path().join("connector-started.txt");
        let port = reserve_test_port();
        let endpoint = format!("http://127.0.0.1:{port}");
        let server_signal = signal_path.clone();
        let server_thread = thread::spawn(move || {
            wait_for_path(&server_signal, Duration::from_secs(5)).expect("wait for child signal");
            let _server = MockHttpConnectorServer::bind_at("aiim", port);
            thread::sleep(Duration::from_secs(2));
        });

        let doc = parse_compose_doc(
            &format!(
                r#"
version: 1
runtime:
  db: .agentfs/demo.db
  mountpoint: ./mnt/appfs
  backend: fuse
connectors:
  aiim-http:
    mode: external_or_command
    transport: http
    endpoint: {endpoint}
    healthcheck:
      interval_ms: 20
      timeout_ms: 100
      max_attempts: 50
    command:
      program: {program}
      args: {args}
apps:
  aiim:
    connector: aiim-http
"#,
                program = shell_program_literal(),
                args = shell_args_yaml(&signal_path),
            ),
            temp.path().join("appfs-compose.yaml"),
        )
        .expect("compose should parse");

        let (mut supervisor, _resolved) =
            ComposeConnectorSupervisor::resolve_apps(&doc).expect("compose should resolve");
        assert!(signal_path.exists());
        assert_eq!(supervisor.owned_pids().len(), 1);
        supervisor.shutdown();
        server_thread.join().expect("server thread");
    }

    fn wait_for_path(path: &Path, timeout: Duration) -> anyhow::Result<()> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if path.exists() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(20));
        }
        anyhow::bail!("timed out waiting for {}", path.display())
    }

    fn reserve_test_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .expect("bind port")
            .local_addr()
            .expect("local addr")
            .port()
    }

    fn wait_for_process_exit(pid: u32, timeout: Duration) -> anyhow::Result<()> {
        let started = Instant::now();
        while started.elapsed() < timeout {
            if !process_exists(pid)? {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("process {} did not exit in time", pid)
    }

    fn process_exists(pid: u32) -> anyhow::Result<bool> {
        #[cfg(target_os = "windows")]
        {
            let status = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "if (Get-Process -Id {} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}",
                        pid
                    ),
                ])
                .status()?;
            Ok(status.success())
        }
        #[cfg(not(target_os = "windows"))]
        {
            let status = std::process::Command::new("sh")
                .args(["-c", &format!("kill -0 {} >/dev/null 2>&1", pid)])
                .status()?;
            Ok(status.success())
        }
    }

    fn shell_program_literal() -> &'static str {
        #[cfg(target_os = "windows")]
        {
            "powershell"
        }
        #[cfg(not(target_os = "windows"))]
        {
            "sh"
        }
    }

    fn shell_args_yaml(signal_path: &Path) -> String {
        #[cfg(target_os = "windows")]
        {
            serde_json::to_string(&vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                format!(
                    "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 30",
                    signal_path.display()
                ),
            ])
            .expect("serialize shell args")
        }
        #[cfg(not(target_os = "windows"))]
        {
            serde_json::to_string(&vec![
                "-c".to_string(),
                format!("printf started > '{}' ; sleep 30", signal_path.display()),
            ])
            .expect("serialize shell args")
        }
    }

    struct MockHttpConnectorServer {
        endpoint: String,
        stop: Arc<AtomicBool>,
        thread: Option<thread::JoinHandle<()>>,
    }

    impl MockHttpConnectorServer {
        fn spawn(app_id: &str) -> Self {
            let port = reserve_test_port();
            Self::bind_at(app_id, port)
        }

        fn bind_at(app_id: &str, port: u16) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind connector server");
            listener
                .set_nonblocking(true)
                .expect("set nonblocking listener");
            let endpoint = format!("http://127.0.0.1:{port}");
            let app_id = app_id.to_string();
            let stop = Arc::new(AtomicBool::new(false));
            let stop_flag = stop.clone();
            let thread = thread::spawn(move || {
                while !stop_flag.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            handle_http_connection(&mut stream, &app_id);
                        }
                        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => break,
                    }
                }
            });
            Self {
                endpoint,
                stop,
                thread: Some(thread),
            }
        }

        fn endpoint(&self) -> String {
            self.endpoint.clone()
        }
    }

    impl Drop for MockHttpConnectorServer {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = TcpStream::connect(self.endpoint.trim_start_matches("http://"));
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn handle_http_connection(stream: &mut TcpStream, app_id: &str) {
        let Some((route, _payload)) = parse_http_request(stream) else {
            return;
        };
        let (status, body) = match route.as_str() {
            "/connector/info" => (
                200,
                json!({
                    "connector_id": format!("mock-{app_id}"),
                    "version": "test",
                    "app_id": app_id,
                    "transport": "http_bridge",
                    "supports_snapshot": true,
                    "supports_live": true,
                    "supports_action": true,
                    "optional_features": []
                }),
            ),
            "/connector/health" => (
                200,
                json!({
                    "healthy": true,
                    "auth_status": "valid",
                    "message": "ok",
                    "checked_at": "2026-04-22T00:00:00Z"
                }),
            ),
            _ => (
                404,
                json!({
                    "code": "NOT_SUPPORTED",
                    "message": format!("unknown route: {route}"),
                    "retryable": false
                }),
            ),
        };
        write_http_json(stream, status, &body);
    }

    fn parse_http_request(stream: &mut TcpStream) -> Option<(String, serde_json::Value)> {
        let mut buf = [0u8; 4096];
        let read = stream.read(&mut buf).ok()?;
        if read == 0 {
            return None;
        }
        let request = String::from_utf8_lossy(&buf[..read]);
        let header_end = request.find("\r\n\r\n")?;
        let headers = &request[..header_end];
        let mut lines = headers.lines();
        let request_line = lines.next()?;
        let route = request_line.split_whitespace().nth(1)?.to_string();
        let content_length = lines
            .find_map(|line| {
                line.strip_prefix("Content-Length:")
                    .map(str::trim)
                    .and_then(|value| value.parse::<usize>().ok())
            })
            .unwrap_or(0);
        let body_bytes = &buf[header_end + 4..read];
        let payload = if content_length == 0 {
            json!({})
        } else {
            serde_json::from_slice(body_bytes).ok()?
        };
        Some((route, payload))
    }

    fn write_http_json(stream: &mut TcpStream, status: u16, body: &serde_json::Value) {
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
}
