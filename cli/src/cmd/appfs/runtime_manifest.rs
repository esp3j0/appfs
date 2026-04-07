use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub(crate) const APPFS_RUNTIME_MANIFEST_REL_PATH: &str = ".well-known/appfs/runtime.json";
pub(crate) const APPFS_RUNTIME_SCHEMA_VERSION: u32 = 1;
pub(crate) const APPFS_RUNTIME_KIND: &str = "appfs";
pub(crate) const APPFS_MULTI_AGENT_MODE_SHARED: &str = "shared_mount_distinct_attach";
const CONTROL_REGISTER_ACTION_PATH: &str = "/_appfs/register_app.act";
const CONTROL_UNREGISTER_ACTION_PATH: &str = "/_appfs/unregister_app.act";
const CONTROL_LIST_ACTION_PATH: &str = "/_appfs/list_apps.act";
const CONTROL_REGISTRY_PATH: &str = "/_appfs/apps.registry.json";
const CONTROL_EVENTS_PATH: &str = "/_appfs/_stream/events.evt.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsRuntimeManifestControlPlaneDoc {
    pub(crate) register_action: String,
    pub(crate) unregister_action: String,
    pub(crate) list_action: String,
    pub(crate) registry: String,
    pub(crate) events: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsRuntimeManifestCapabilitiesDoc {
    pub(crate) app_registration: bool,
    pub(crate) event_stream: bool,
    pub(crate) multi_app: bool,
    pub(crate) scope_switch: bool,
    pub(crate) multi_agent_attach: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AppfsRuntimeManifestDoc {
    pub(crate) schema_version: u32,
    pub(crate) runtime_kind: String,
    pub(crate) mount_root: PathBuf,
    pub(crate) runtime_session_id: String,
    pub(crate) managed: bool,
    pub(crate) multi_agent_mode: String,
    pub(crate) control_plane: AppfsRuntimeManifestControlPlaneDoc,
    pub(crate) capabilities: AppfsRuntimeManifestCapabilitiesDoc,
    pub(crate) generated_at: String,
}

pub(crate) fn generate_runtime_session_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("rt-{}", &uuid[..8])
}

pub(crate) fn runtime_manifest_path(root: &Path) -> PathBuf {
    root.join(APPFS_RUNTIME_MANIFEST_REL_PATH.replace('/', std::path::MAIN_SEPARATOR_STR))
}

pub(crate) fn build_runtime_manifest(
    root: &Path,
    runtime_session_id: &str,
    managed: bool,
) -> AppfsRuntimeManifestDoc {
    AppfsRuntimeManifestDoc {
        schema_version: APPFS_RUNTIME_SCHEMA_VERSION,
        runtime_kind: APPFS_RUNTIME_KIND.to_string(),
        mount_root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
        runtime_session_id: runtime_session_id.to_string(),
        managed,
        multi_agent_mode: APPFS_MULTI_AGENT_MODE_SHARED.to_string(),
        control_plane: AppfsRuntimeManifestControlPlaneDoc {
            register_action: CONTROL_REGISTER_ACTION_PATH.to_string(),
            unregister_action: CONTROL_UNREGISTER_ACTION_PATH.to_string(),
            list_action: CONTROL_LIST_ACTION_PATH.to_string(),
            registry: CONTROL_REGISTRY_PATH.to_string(),
            events: CONTROL_EVENTS_PATH.to_string(),
        },
        capabilities: AppfsRuntimeManifestCapabilitiesDoc {
            app_registration: true,
            event_stream: true,
            multi_app: true,
            scope_switch: true,
            multi_agent_attach: true,
        },
        generated_at: Utc::now().to_rfc3339(),
    }
}

pub(crate) fn write_runtime_manifest(
    root: &Path,
    runtime_session_id: &str,
    managed: bool,
) -> Result<()> {
    let path = runtime_manifest_path(root);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("invalid AppFS runtime manifest path {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create AppFS runtime manifest directory {}",
            parent.display()
        )
    })?;
    let doc = build_runtime_manifest(root, runtime_session_id, managed);
    let bytes =
        serde_json::to_vec_pretty(&doc).context("failed to serialize AppFS runtime manifest")?;
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, bytes).with_context(|| {
        format!(
            "failed to write temporary AppFS runtime manifest {}",
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "failed to publish AppFS runtime manifest {}",
            path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        build_runtime_manifest, generate_runtime_session_id, runtime_manifest_path,
        write_runtime_manifest, APPFS_MULTI_AGENT_MODE_SHARED, APPFS_RUNTIME_KIND,
        APPFS_RUNTIME_MANIFEST_REL_PATH, APPFS_RUNTIME_SCHEMA_VERSION,
    };
    use tempfile::tempdir;

    #[test]
    fn writes_runtime_manifest_at_well_known_path() {
        let temp = tempdir().expect("tempdir");
        let session_id = "rt-shared-01";

        write_runtime_manifest(temp.path(), session_id, true).expect("write runtime manifest");

        let manifest_path = runtime_manifest_path(temp.path());
        assert!(manifest_path.exists());
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).expect("read runtime manifest"))
                .expect("parse runtime manifest");
        assert_eq!(doc["schema_version"], APPFS_RUNTIME_SCHEMA_VERSION);
        assert_eq!(doc["runtime_kind"], APPFS_RUNTIME_KIND);
        assert_eq!(doc["runtime_session_id"], session_id);
        assert_eq!(doc["multi_agent_mode"], APPFS_MULTI_AGENT_MODE_SHARED);
        assert_eq!(
            manifest_path
                .strip_prefix(temp.path())
                .expect("manifest relative path")
                .to_string_lossy()
                .replace('\\', "/"),
            APPFS_RUNTIME_MANIFEST_REL_PATH
        );
    }

    #[test]
    fn runtime_session_id_is_prefixed_for_runtime_scope() {
        let session_id = generate_runtime_session_id();
        assert!(session_id.starts_with("rt-"));
        assert!(session_id.len() > 3);
    }

    #[test]
    fn build_runtime_manifest_marks_multi_agent_attach_capability() {
        let temp = tempdir().expect("tempdir");
        let doc = build_runtime_manifest(temp.path(), "rt-shared-02", false);
        assert!(doc.capabilities.multi_agent_attach);
        assert!(doc.capabilities.event_stream);
        assert!(!doc.mount_root.as_os_str().is_empty());
    }

    #[test]
    fn write_runtime_manifest_rewrites_existing_manifest() {
        let temp = tempdir().expect("tempdir");

        write_runtime_manifest(temp.path(), "rt-shared-03", true).expect("write first manifest");
        write_runtime_manifest(temp.path(), "rt-shared-04", false)
            .expect("rewrite runtime manifest");

        let manifest_path = runtime_manifest_path(temp.path());
        let doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).expect("read runtime manifest"))
                .expect("parse runtime manifest");
        assert_eq!(doc["runtime_session_id"], "rt-shared-04");
        assert_eq!(doc["managed"], false);
    }
}
