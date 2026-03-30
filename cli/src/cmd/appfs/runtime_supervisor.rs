use super::action_dispatcher;
use super::registry;
use super::registry_manager::{self, RegistryRuntimeSnapshot};
use super::runtime_entry::{
    build_runtime_entry, read_active_scope, transport_summary, AppRuntimeEntry,
};
use super::supervisor_control;
use super::ResolvedAppfsRuntimeCliArgs;
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub(super) struct AppfsRuntimeSupervisor {
    root: PathBuf,
    control_plane: supervisor_control::SupervisorControlPlane,
    pub(super) runtimes: BTreeMap<String, AppRuntimeEntry>,
}

impl AppfsRuntimeSupervisor {
    pub(super) fn new(
        root: PathBuf,
        runtime_args: Vec<ResolvedAppfsRuntimeCliArgs>,
    ) -> Result<Self> {
        let mut runtimes = BTreeMap::new();
        for runtime in runtime_args {
            let entry = build_runtime_entry(&root, runtime)?;
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
                std::env::var("APPFS_ACTIONLINE_STRICT")
                    .map(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "True"))
                    .unwrap_or(false),
            )?,
            root,
            runtimes,
        })
    }

    pub(super) fn prepare_action_sinks(&mut self) -> Result<()> {
        self.control_plane.prepare_action_sinks()?;
        for entry in self.runtimes.values_mut() {
            entry.adapter.prepare_action_sinks()?;
        }
        Ok(())
    }

    pub(super) fn poll_once(&mut self) -> Result<()> {
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

    pub(super) fn log_started(&self) {
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

    pub(super) fn sync_registry_to_disk(
        &self,
        existing: Option<&registry::AppfsAppsRegistryDoc>,
    ) -> Result<()> {
        let snapshots = self
            .runtimes
            .values()
            .map(|entry| RegistryRuntimeSnapshot {
                runtime: entry.runtime.clone(),
                app_dir: entry.adapter.app_dir.clone(),
            })
            .collect::<Vec<_>>();
        registry_manager::persist_runtime_registry(&self.root, &snapshots, existing)
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

        let runtime = match registry_manager::register_request_to_runtime(request) {
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

        match build_runtime_entry(&self.root, runtime.clone()) {
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
}
