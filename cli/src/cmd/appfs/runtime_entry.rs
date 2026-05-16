use super::{
    build_appfs_bridge_config, AppRuntimeStartupBootstrap, AppfsAdapter, AppfsBridgeCliArgs,
    ResolvedAppfsRuntimeCliArgs, APP_STRUCTURE_SYNC_STATE_FILENAME,
};
use anyhow::Result;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::Path;

pub(super) struct AppRuntimeEntry {
    pub(super) runtime: ResolvedAppfsRuntimeCliArgs,
    pub(super) adapter: AppfsAdapter,
    pub(super) registry_metadata: AppRuntimeRegistryMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AppRuntimeRegistryMetadata {
    pub(super) instance_id: String,
    pub(super) visibility: crate::cmd::appfs::registry::AppfsRegisteredAppVisibility,
    pub(super) parent_app_id: Option<String>,
    pub(super) principal_id: Option<String>,
    pub(super) profile_id: Option<String>,
    pub(super) path: String,
    pub(super) inbound_poll_ms: u64,
}

pub(super) fn build_runtime_entry(
    root: &Path,
    runtime: ResolvedAppfsRuntimeCliArgs,
    startup_bootstrap: Option<AppRuntimeStartupBootstrap>,
) -> Result<AppRuntimeEntry> {
    build_runtime_entry_with_metadata(
        root,
        runtime.clone(),
        AppRuntimeRegistryMetadata::public(runtime.app_id.clone()),
        startup_bootstrap,
    )
}

pub(super) fn build_runtime_entry_with_metadata(
    root: &Path,
    runtime: ResolvedAppfsRuntimeCliArgs,
    metadata: AppRuntimeRegistryMetadata,
    startup_bootstrap: Option<AppRuntimeStartupBootstrap>,
) -> Result<AppRuntimeEntry> {
    let adapter = AppfsAdapter::new_with_mount_path(
        root.to_path_buf(),
        runtime.app_id.clone(),
        metadata.path.clone(),
        metadata.principal_id.clone(),
        metadata.profile_id.clone(),
        runtime.session_id.clone(),
        build_appfs_bridge_config(runtime.bridge.clone()),
        startup_bootstrap,
    )?;
    Ok(AppRuntimeEntry {
        runtime,
        adapter,
        registry_metadata: metadata,
    })
}

impl AppRuntimeRegistryMetadata {
    pub(super) fn public(app_id: String) -> Self {
        Self {
            instance_id: app_id.clone(),
            visibility: crate::cmd::appfs::registry::AppfsRegisteredAppVisibility::Public,
            parent_app_id: None,
            principal_id: None,
            profile_id: None,
            path: app_id,
            inbound_poll_ms: 0,
        }
    }

    pub(super) fn from_registered_app(
        app: &crate::cmd::appfs::registry::AppfsRegisteredAppDoc,
    ) -> Self {
        Self {
            instance_id: app.instance_id.clone(),
            visibility: app.visibility,
            parent_app_id: app.parent_app_id.clone(),
            principal_id: app.principal_id.clone(),
            profile_id: app.profile_id.clone(),
            path: app.path.clone(),
            inbound_poll_ms: app.inbound_poll_ms.unwrap_or(0),
        }
    }
}

pub(super) fn read_active_scope(app_dir: &Path) -> Option<String> {
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

pub(super) fn transport_summary(bridge: &AppfsBridgeCliArgs) -> JsonValue {
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
