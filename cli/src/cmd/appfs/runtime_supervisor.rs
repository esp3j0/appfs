use super::action_dispatcher;
use super::registry;
use super::registry_manager::{self, RegistryRuntimeSnapshot};
use super::runtime_entry::{
    build_runtime_entry, build_runtime_entry_with_metadata, read_active_scope, transport_summary,
    AppRuntimeEntry, AppRuntimeRegistryMetadata,
};
use super::runtime_manifest;
use super::supervisor_control;
use super::{AppRuntimeStartupBootstrap, ResolvedAppfsRuntimeCliArgs};
use anyhow::Result;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub(super) struct AppfsRuntimeSupervisor {
    root: PathBuf,
    managed: bool,
    runtime_session_id: String,
    control_plane: supervisor_control::SupervisorControlPlane,
    pub(super) runtimes: BTreeMap<String, AppRuntimeEntry>,
}

impl AppfsRuntimeSupervisor {
    pub(super) fn new(
        root: PathBuf,
        runtime_args: Vec<ResolvedAppfsRuntimeCliArgs>,
        managed: bool,
        startup_bootstrap: Option<HashMap<String, AppRuntimeStartupBootstrap>>,
    ) -> Result<Self> {
        let mut runtimes = BTreeMap::new();
        let mut startup_bootstrap = startup_bootstrap.unwrap_or_default();
        for runtime in runtime_args {
            let app_id = runtime.app_id.clone();
            let entry = build_runtime_entry(&root, runtime, startup_bootstrap.remove(&app_id))?;
            if runtimes
                .insert(entry.runtime.app_id.clone(), entry)
                .is_some()
            {
                anyhow::bail!("duplicate runtime app_id during supervisor bootstrap");
            }
        }
        Ok(Self {
            managed,
            runtime_session_id: runtime_manifest::generate_runtime_session_id(),
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
        self.ensure_default_principal_for_private_policies()?;
        self.materialize_private_apps_for_existing_principals()?;
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
        self.materialize_private_apps_for_existing_principals()?;
        for entry in self.runtimes.values_mut() {
            entry.adapter.poll_once()?;
        }
        self.sync_runtime_registry_to_disk(None)?;
        Ok(())
    }

    pub(super) fn log_started(&self) {
        eprintln!(
            "AppFS runtime session started (mount_root={} runtime_session_id={} managed={})",
            self.root.display(),
            self.runtime_session_id,
            self.managed
        );
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
        self.sync_runtime_registry_to_disk(existing)?;
        runtime_manifest::write_runtime_manifest(&self.root, &self.runtime_session_id, self.managed)
    }

    fn sync_runtime_registry_to_disk(
        &self,
        existing: Option<&registry::AppfsAppsRegistryDoc>,
    ) -> Result<()> {
        let snapshots = self
            .runtimes
            .values()
            .map(|entry| RegistryRuntimeSnapshot {
                runtime: entry.runtime.clone(),
                app_dir: entry.adapter.app_dir.clone(),
                metadata: entry.registry_metadata.clone(),
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
            supervisor_control::SupervisorControlInvocation::CreatePrincipal {
                request_id,
                client_token,
                request,
            } => self.handle_create_principal(&request_id, client_token, request),
            supervisor_control::SupervisorControlInvocation::UpdatePrincipal {
                request_id,
                client_token,
                request,
            } => self.handle_update_principal(&request_id, client_token, request),
            supervisor_control::SupervisorControlInvocation::DeletePrincipal {
                request_id,
                client_token,
                request,
            } => self.handle_delete_principal(&request_id, client_token, request),
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

        match build_runtime_entry(&self.root, runtime.clone(), None) {
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

    fn handle_create_principal(
        &mut self,
        request_id: &str,
        client_token: Option<String>,
        request: action_dispatcher::CreatePrincipalRequest,
    ) -> Result<()> {
        let mut doc = self.load_principal_registry()?;
        if let Some(existing) = doc
            .principals
            .iter()
            .find(|principal| principal.principal_id == request.principal_id)
            .cloned()
        {
            registry::write_principal_record_view(&self.root, &existing)?;
            let materialized = self.materialize_private_apps_for_principal(&existing)?;
            self.control_plane.emit_completed(
                "/_appfs/principals/create_principal.act",
                request_id,
                serde_json::json!({
                    "principal_event": "principal.exists",
                    "principal_id": existing.principal_id,
                    "created": false,
                    "exists": true,
                    "app_instances": materialized,
                }),
                client_token,
            )?;
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let record = registry::PrincipalRecord {
            principal_id: request.principal_id,
            display_name: request.display_name,
            description: request.description,
            kind: request.kind,
            created_at: now.clone(),
            updated_at: now,
        };
        doc.principals.push(record.clone());
        registry::write_principal_registry(&self.root, &doc)?;
        registry::write_principal_record_view(&self.root, &record)?;
        let materialized = self.materialize_private_apps_for_principal(&record)?;
        self.control_plane.emit_completed(
            "/_appfs/principals/create_principal.act",
            request_id,
            serde_json::json!({
                "principal_event": "principal.created",
                "principal_id": record.principal_id,
                "created": true,
                "app_instances": materialized,
            }),
            client_token,
        )?;
        Ok(())
    }

    fn handle_update_principal(
        &mut self,
        request_id: &str,
        client_token: Option<String>,
        request: action_dispatcher::UpdatePrincipalRequest,
    ) -> Result<()> {
        let mut doc = self.load_principal_registry()?;
        let Some(record) = doc
            .principals
            .iter_mut()
            .find(|principal| principal.principal_id == request.principal_id)
        else {
            self.control_plane.emit_failed(
                "/_appfs/principals/update_principal.act",
                request_id,
                "PRINCIPAL_NOT_FOUND",
                &format!("principal {} is not registered", request.principal_id),
                client_token,
            )?;
            return Ok(());
        };

        if let Some(display_name) = request.display_name {
            record.display_name = display_name;
        }
        if let Some(description) = request.description {
            record.description = Some(description);
        }
        if let Some(kind) = request.kind {
            record.kind = kind;
        }
        record.updated_at = chrono::Utc::now().to_rfc3339();
        let updated = record.clone();
        registry::write_principal_registry(&self.root, &doc)?;
        registry::write_principal_record_view(&self.root, &updated)?;
        self.control_plane.emit_completed(
            "/_appfs/principals/update_principal.act",
            request_id,
            serde_json::json!({
                "principal_event": "principal.updated",
                "principal_id": updated.principal_id,
                "updated": true,
            }),
            client_token,
        )?;
        Ok(())
    }

    fn handle_delete_principal(
        &mut self,
        request_id: &str,
        client_token: Option<String>,
        request: action_dispatcher::DeletePrincipalRequest,
    ) -> Result<()> {
        let mut doc = self.load_principal_registry()?;
        let before = doc.principals.len();
        doc.principals
            .retain(|principal| principal.principal_id != request.principal_id);
        if doc.principals.len() == before {
            self.control_plane.emit_failed(
                "/_appfs/principals/delete_principal.act",
                request_id,
                "PRINCIPAL_NOT_FOUND",
                &format!("principal {} is not registered", request.principal_id),
                client_token,
            )?;
            return Ok(());
        }
        let credential_cleanup_requests = self
            .runtimes
            .values()
            .filter(|entry| {
                entry.registry_metadata.principal_id.as_deref()
                    == Some(request.principal_id.as_str())
            })
            .filter_map(|entry| {
                let profile_id = entry.registry_metadata.profile_id.as_ref()?;
                Some(serde_json::json!({
                    "instance_id": entry.registry_metadata.instance_id.as_str(),
                    "app_id": entry.runtime.app_id.as_str(),
                    "principal_id": request.principal_id.as_str(),
                    "profile_id": profile_id,
                    "status": "requested",
                }))
            })
            .collect::<Vec<_>>();
        registry::write_principal_registry(&self.root, &doc)?;
        registry::delete_principal_record_view(&self.root, &request.principal_id)?;
        self.control_plane.emit_completed(
            "/_appfs/principals/delete_principal.act",
            request_id,
            serde_json::json!({
                "principal_event": "principal.deleted",
                "principal_id": request.principal_id,
                "deleted": true,
                "credentials_cleanup": "requested",
                "credential_cleanup_requests": credential_cleanup_requests,
            }),
            client_token,
        )?;
        Ok(())
    }

    fn load_principal_registry(&self) -> Result<registry::PrincipalRegistryDoc> {
        Ok(registry::read_principal_registry(&self.root)?.unwrap_or(
            registry::PrincipalRegistryDoc {
                version: registry::APPFS_REGISTRY_VERSION,
                default_principal_id: registry::APPFS_DEFAULT_PRINCIPAL_ID.to_string(),
                principals: Vec::new(),
            },
        ))
    }

    fn ensure_default_principal_for_private_policies(&mut self) -> Result<()> {
        let Some(policy_doc) = registry::read_app_policy_registry(&self.root)? else {
            return Ok(());
        };
        if !policy_doc
            .apps
            .iter()
            .any(|policy| policy.visibility == registry::AppfsAppPolicyVisibility::Private)
        {
            return Ok(());
        }

        let mut doc = self.load_principal_registry()?;
        if doc
            .principals
            .iter()
            .any(|principal| principal.principal_id == registry::APPFS_DEFAULT_PRINCIPAL_ID)
        {
            return Ok(());
        }

        let now = chrono::Utc::now().to_rfc3339();
        let record = registry::PrincipalRecord {
            principal_id: registry::APPFS_DEFAULT_PRINCIPAL_ID.to_string(),
            display_name: "Default agent".to_string(),
            description: Some("The default AppFS agent principal.".to_string()),
            kind: "agent".to_string(),
            created_at: now.clone(),
            updated_at: now,
        };
        doc.principals.push(record.clone());
        registry::write_principal_registry(&self.root, &doc)?;
        registry::write_principal_record_view(&self.root, &record)
    }

    fn materialize_private_apps_for_existing_principals(&mut self) -> Result<()> {
        let doc = self.load_principal_registry()?;
        for principal in doc.principals {
            self.materialize_private_apps_for_principal(&principal)?;
        }
        Ok(())
    }

    fn materialize_private_apps_for_principal(
        &mut self,
        principal: &registry::PrincipalRecord,
    ) -> Result<Vec<serde_json::Value>> {
        let Some(policy_doc) = registry::read_app_policy_registry(&self.root)? else {
            return Ok(Vec::new());
        };
        let mut materialized = Vec::new();
        for policy in policy_doc
            .apps
            .iter()
            .filter(|policy| policy.visibility == registry::AppfsAppPolicyVisibility::Private)
        {
            let instance_id = format!("{}--{}", policy.app_id, principal.principal_id);
            if self.runtimes.contains_key(&instance_id) {
                continue;
            }
            let path_template = policy.path_template.as_deref().ok_or_else(|| {
                anyhow::anyhow!("private app policy {} missing path_template", policy.app_id)
            })?;
            let path = render_principal_template(path_template, &principal.principal_id);
            let profile_id = policy
                .profile_template
                .as_deref()
                .map(|template| render_principal_template(template, &principal.principal_id))
                .unwrap_or_else(|| format!("{}:{}", policy.app_id, principal.principal_id));
            let runtime = ResolvedAppfsRuntimeCliArgs {
                app_id: policy.app_id.clone(),
                session_id: super::normalize_appfs_session_id(None),
                bridge: registry::bridge_args_from_transport_doc(&policy.transport),
            };
            let metadata = AppRuntimeRegistryMetadata {
                instance_id: instance_id.clone(),
                visibility: registry::AppfsRegisteredAppVisibility::PrivateInstance,
                parent_app_id: Some(policy.app_id.clone()),
                principal_id: Some(principal.principal_id.clone()),
                profile_id: Some(profile_id.clone()),
                path: path.clone(),
            };
            let mut entry = build_runtime_entry_with_metadata(&self.root, runtime, metadata, None)?;
            entry.adapter.prepare_action_sinks()?;
            self.runtimes.insert(instance_id.clone(), entry);
            materialized.push(serde_json::json!({
                "instance_id": instance_id,
                "app_id": policy.app_id,
                "principal_id": principal.principal_id,
                "profile_id": profile_id,
                "path": path,
            }));
        }
        if !materialized.is_empty() {
            self.sync_registry_to_disk(None)?;
        }
        Ok(materialized)
    }
}

fn render_principal_template(template: &str, principal_id: &str) -> String {
    template.replace("{principal_id}", principal_id)
}
