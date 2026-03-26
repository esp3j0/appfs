use super::{
    build_appfs_bridge_config, AppfsAdapter, AppfsBridgeCliArgs, ResolvedAppfsRuntimeCliArgs,
    APP_STRUCTURE_SYNC_STATE_FILENAME,
};
use anyhow::Result;
use serde_json::Value as JsonValue;
use std::fs;
use std::path::Path;

pub(super) struct AppRuntimeEntry {
    pub(super) runtime: ResolvedAppfsRuntimeCliArgs,
    pub(super) adapter: AppfsAdapter,
}

pub(super) fn build_runtime_entry(
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
