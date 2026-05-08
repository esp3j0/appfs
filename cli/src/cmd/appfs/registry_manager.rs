use super::action_dispatcher::RegisterAppRequest;
use super::registry;
use super::runtime_entry::read_active_scope;
use super::runtime_entry::AppRuntimeRegistryMetadata;
use super::{normalize_appfs_session_id, resolve_runtime_cli_args, ResolvedAppfsRuntimeCliArgs};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) struct RegistryRuntimeSnapshot {
    pub(super) runtime: ResolvedAppfsRuntimeCliArgs,
    pub(super) app_dir: PathBuf,
    pub(super) metadata: AppRuntimeRegistryMetadata,
}

pub(super) fn persist_runtime_registry(
    root: &Path,
    snapshots: &[RegistryRuntimeSnapshot],
    existing: Option<&registry::AppfsAppsRegistryDoc>,
) -> Result<()> {
    let existing = match existing {
        Some(existing) => Some(existing.clone()),
        None => registry::read_app_registry(root)?,
    };
    let existing_registered_at = existing
        .as_ref()
        .map(|doc| {
            doc.apps
                .iter()
                .map(|app| (app.instance_id.clone(), app.registered_at.clone()))
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    let doc = registry::AppfsAppsRegistryDoc {
        version: registry::APPFS_REGISTRY_VERSION,
        apps: snapshots
            .iter()
            .map(|snapshot| registry::AppfsRegisteredAppDoc {
                instance_id: snapshot.metadata.instance_id.clone(),
                app_id: snapshot.runtime.app_id.clone(),
                visibility: snapshot.metadata.visibility,
                parent_app_id: snapshot.metadata.parent_app_id.clone(),
                principal_id: snapshot.metadata.principal_id.clone(),
                profile_id: snapshot.metadata.profile_id.clone(),
                path: snapshot.metadata.path.clone(),
                transport: registry::transport_doc_from_bridge_args(&snapshot.runtime.bridge),
                session_id: snapshot.runtime.session_id.clone(),
                registered_at: existing_registered_at
                    .get(&snapshot.metadata.instance_id)
                    .cloned()
                    .unwrap_or_else(|| now.clone()),
                active_scope: read_active_scope(&snapshot.app_dir),
            })
            .collect(),
    };
    if existing.as_ref() == Some(&doc) {
        return Ok(());
    }
    registry::write_app_registry(root, &doc)
}

pub(super) fn register_request_to_runtime(
    request: RegisterAppRequest,
) -> Result<ResolvedAppfsRuntimeCliArgs> {
    let session_id = normalize_appfs_session_id(request.session_id);
    let doc = registry::AppfsAppsRegistryDoc {
        version: registry::APPFS_REGISTRY_VERSION,
        apps: vec![registry::AppfsRegisteredAppDoc {
            instance_id: request.app_id.clone(),
            app_id: request.app_id.clone(),
            visibility: registry::AppfsRegisteredAppVisibility::Public,
            parent_app_id: None,
            principal_id: None,
            profile_id: None,
            path: request.app_id.clone(),
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
