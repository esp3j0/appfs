use super::action_dispatcher::RegisterAppRequest;
use super::registry;
use super::runtime_entry::read_active_scope;
use super::{normalize_appfs_session_id, resolve_runtime_cli_args, ResolvedAppfsRuntimeCliArgs};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub(super) struct RegistryRuntimeSnapshot {
    pub(super) runtime: ResolvedAppfsRuntimeCliArgs,
    pub(super) app_dir: PathBuf,
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
    let active_scopes = snapshots
        .iter()
        .map(|snapshot| {
            (
                snapshot.runtime.app_id.clone(),
                read_active_scope(&snapshot.app_dir),
            )
        })
        .collect::<HashMap<_, _>>();
    let runtime_args = snapshots
        .iter()
        .map(|snapshot| snapshot.runtime.clone())
        .collect::<Vec<_>>();
    let doc = registry::build_app_registry_doc(&runtime_args, &active_scopes, existing.as_ref());
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
