use super::core::build_structure_connector;
use super::{
    ActionCursorDoc, AppfsBridgeConfig, CursorState, APP_STRUCTURE_SYNC_STATE_FILENAME,
    DEFAULT_RETENTION_HINT_SEC, SNAPSHOT_EXPAND_JOURNAL_FILENAME,
};
use agentfs_sdk::{
    AppConnectorV3, AppStructureNodeKindV3, AppStructureNodeV3, AppStructureSnapshotV3,
    AppStructureSyncReasonV3, AppStructureSyncResultV3, ConnectorContextV3,
    GetAppStructureRequestV3, RefreshAppStructureRequestV3,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct AppStructureSyncStateDoc {
    #[serde(default)]
    revision: Option<String>,
    #[serde(default)]
    active_scope: Option<String>,
    #[serde(default)]
    owned_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StructureSyncOutcome {
    pub(super) changed: bool,
    pub(super) revision: Option<String>,
    pub(super) active_scope: Option<String>,
}

pub(super) fn ensure_app_structure_initialized(
    root: &Path,
    app_id: &str,
    session_id: &str,
    bridge_config: &AppfsBridgeConfig,
) -> Result<()> {
    let app_dir = root.join(app_id);
    let manifest_path = app_dir.join("_meta").join("manifest.res.json");
    let requires_bootstrap = !app_dir.exists() || !manifest_path.exists();
    if !requires_bootstrap {
        bootstrap_runtime_scaffolding(root, app_id)?;
        return Ok(());
    }

    let Some(mut connector) = build_structure_connector(app_id, bridge_config)? else {
        if app_dir.exists() && manifest_path.exists() {
            bootstrap_runtime_scaffolding(root, app_id)?;
            return Ok(());
        }
        anyhow::bail!(
            "App structure is not initialized for app_id={} and no AppConnectorV3 structure path is available yet for the configured transport",
            app_id
        );
    };

    let mut service = AppTreeSyncService::new(
        root.to_path_buf(),
        app_id.to_string(),
        session_id.to_string(),
    );
    service.sync_initial(&mut *connector)?;
    Ok(())
}

pub(super) fn refresh_app_structure(
    root: &Path,
    app_id: &str,
    session_id: &str,
    connector: &mut dyn AppConnectorV3,
    reason: AppStructureSyncReasonV3,
    target_scope: Option<String>,
    trigger_action_path: Option<String>,
) -> Result<StructureSyncOutcome> {
    let mut service = AppTreeSyncService::new(
        root.to_path_buf(),
        app_id.to_string(),
        session_id.to_string(),
    );
    service.refresh(connector, reason, target_scope, trigger_action_path)
}

struct AppTreeSyncService {
    root: PathBuf,
    app_id: String,
    session_id: String,
}

impl AppTreeSyncService {
    fn new(root: PathBuf, app_id: String, session_id: String) -> Self {
        Self {
            root,
            app_id,
            session_id,
        }
    }

    fn sync_initial(&mut self, connector: &mut dyn AppConnectorV3) -> Result<()> {
        let state = self.load_state()?;
        let ctx = self.context("structure-init");
        eprintln!(
            "[structure.sync] op=get_app_structure app={} known_revision={}",
            self.app_id,
            state.revision.as_deref().unwrap_or("<none>")
        );
        let response = connector.get_app_structure(
            GetAppStructureRequestV3 {
                app_id: self.app_id.clone(),
                known_revision: state.revision.clone(),
            },
            &ctx,
        )?;

        match response.result {
            AppStructureSyncResultV3::Unchanged { .. } => {
                bootstrap_runtime_scaffolding(&self.root, &self.app_id)?;
                eprintln!(
                    "[structure.sync] result app={} changed=false revision={} active_scope={}",
                    self.app_id,
                    state.revision.as_deref().unwrap_or("<none>"),
                    state.active_scope.as_deref().unwrap_or("<none>")
                );
            }
            AppStructureSyncResultV3::Snapshot { snapshot } => {
                let revision = snapshot.revision.clone();
                let active_scope = snapshot.active_scope.clone();
                self.apply_snapshot(snapshot)?;
                eprintln!(
                    "[structure.sync] result app={} changed=true revision={} active_scope={}",
                    self.app_id,
                    revision,
                    active_scope.as_deref().unwrap_or("<none>")
                );
            }
        }
        Ok(())
    }

    #[allow(dead_code)]
    fn refresh(
        &mut self,
        connector: &mut dyn AppConnectorV3,
        reason: AppStructureSyncReasonV3,
        target_scope: Option<String>,
        trigger_action_path: Option<String>,
    ) -> Result<StructureSyncOutcome> {
        let state = self.load_state()?;
        let ctx = self.context("structure-refresh");
        eprintln!(
            "[structure.sync] op=refresh_app_structure app={} reason={} target_scope={} trigger_action_path={} known_revision={}",
            self.app_id,
            structure_reason_label(reason),
            target_scope.as_deref().unwrap_or("<none>"),
            trigger_action_path.as_deref().unwrap_or("<none>"),
            state.revision.as_deref().unwrap_or("<none>")
        );
        let response = connector.refresh_app_structure(
            RefreshAppStructureRequestV3 {
                app_id: self.app_id.clone(),
                known_revision: state.revision.clone(),
                reason,
                target_scope,
                trigger_action_path,
            },
            &ctx,
        )?;

        match response.result {
            AppStructureSyncResultV3::Unchanged {
                revision,
                active_scope,
                ..
            } => {
                eprintln!(
                    "[structure.sync] result app={} changed=false revision={} active_scope={}",
                    self.app_id,
                    revision,
                    active_scope.as_deref().unwrap_or("<none>")
                );
                Ok(StructureSyncOutcome {
                    changed: false,
                    revision: Some(revision),
                    active_scope,
                })
            }
            AppStructureSyncResultV3::Snapshot { snapshot } => {
                let revision = snapshot.revision.clone();
                let active_scope = snapshot.active_scope.clone();
                self.apply_snapshot(snapshot)?;
                eprintln!(
                    "[structure.sync] result app={} changed=true revision={} active_scope={}",
                    self.app_id,
                    revision,
                    active_scope.as_deref().unwrap_or("<none>")
                );
                Ok(StructureSyncOutcome {
                    changed: true,
                    revision: Some(revision),
                    active_scope,
                })
            }
        }
    }

    fn apply_snapshot(&mut self, snapshot: AppStructureSnapshotV3) -> Result<()> {
        if snapshot.app_id != self.app_id {
            anyhow::bail!(
                "structure snapshot app_id mismatch: snapshot={} runtime={}",
                snapshot.app_id,
                self.app_id
            );
        }

        let state = self.load_state()?;
        let desired_owned_paths = self.desired_owned_paths(&snapshot);
        self.prune_removed_owned_paths(&state.owned_paths, &desired_owned_paths)?;
        self.materialize_snapshot(&snapshot)?;
        self.save_state(&AppStructureSyncStateDoc {
            revision: Some(snapshot.revision),
            active_scope: snapshot.active_scope,
            owned_paths: desired_owned_paths.into_iter().collect(),
        })?;
        bootstrap_runtime_scaffolding(&self.root, &self.app_id)?;
        Ok(())
    }

    fn materialize_snapshot(&self, snapshot: &AppStructureSnapshotV3) -> Result<()> {
        let app_dir = self.app_dir();
        fs::create_dir_all(&app_dir)
            .with_context(|| format!("Failed to create app directory {}", app_dir.display()))?;

        let mut manifest_nodes = BTreeMap::new();
        let mut dir_paths = BTreeSet::new();
        dir_paths.insert(String::new());
        dir_paths.insert("_meta".to_string());

        for node in &snapshot.nodes {
            self.validate_node(node)?;
            if let Some(parent) = Path::new(&node.path).parent() {
                let rel = parent.to_string_lossy().replace('\\', "/");
                if !rel.is_empty() {
                    dir_paths.insert(rel);
                }
            }
            if matches!(node.kind, AppStructureNodeKindV3::Directory) {
                dir_paths.insert(node.path.clone());
            }
            if let Some(entry) = &node.manifest_entry {
                let template = entry
                    .get("template")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("manifest_entry missing template for {}", node.path)
                    })?;
                let mut normalized = entry.clone();
                if let Some(obj) = normalized.as_object_mut() {
                    obj.remove("template");
                }
                manifest_nodes.insert(template.to_string(), normalized);
            }
        }

        for dir in dir_paths {
            let full = if dir.is_empty() {
                app_dir.clone()
            } else {
                app_dir.join(dir)
            };
            fs::create_dir_all(&full)
                .with_context(|| format!("Failed to create directory {}", full.display()))?;
        }

        for node in &snapshot.nodes {
            self.materialize_node(node)?;
        }

        let manifest_json = json!({
            "app_id": self.app_id,
            "nodes": manifest_nodes,
        });
        let manifest_path = app_dir.join("_meta").join("manifest.res.json");
        write_json_file(&manifest_path, &manifest_json)?;
        Ok(())
    }

    fn materialize_node(&self, node: &AppStructureNodeV3) -> Result<()> {
        let full = self.app_dir().join(&node.path);
        match node.kind {
            AppStructureNodeKindV3::Directory => {
                fs::create_dir_all(&full)
                    .with_context(|| format!("Failed to create directory {}", full.display()))?;
            }
            AppStructureNodeKindV3::ActionFile => {
                ensure_parent_dir(&full)?;
                if !full.exists() {
                    fs::write(&full, b"").with_context(|| {
                        format!("Failed to create action sink {}", full.display())
                    })?;
                }
            }
            AppStructureNodeKindV3::LiveResource | AppStructureNodeKindV3::StaticJsonResource => {
                ensure_parent_dir(&full)?;
                let content = node.seed_content.clone().unwrap_or_else(|| json!({}));
                write_json_file(&full, &content)?;
            }
            AppStructureNodeKindV3::SnapshotResource => {
                ensure_parent_dir(&full)?;
                if !full.exists() {
                    fs::write(&full, b"").with_context(|| {
                        format!("Failed to create snapshot placeholder {}", full.display())
                    })?;
                }
            }
        }
        Ok(())
    }

    fn prune_removed_owned_paths(
        &self,
        previous_owned_paths: &[String],
        desired_owned_paths: &BTreeSet<String>,
    ) -> Result<()> {
        let mut to_remove: Vec<String> = previous_owned_paths
            .iter()
            .filter(|path| !desired_owned_paths.contains(path.as_str()))
            .cloned()
            .collect();
        to_remove.sort_by_key(|path| std::cmp::Reverse(path.len()));

        for rel in to_remove {
            if is_runtime_protected_path(&rel) {
                continue;
            }
            let full = self.app_dir().join(&rel);
            if !full.exists() {
                continue;
            }
            if full.is_dir() {
                fs::remove_dir(&full)
                    .with_context(|| format!("Failed to remove directory {}", full.display()))?;
            } else {
                fs::remove_file(&full)
                    .with_context(|| format!("Failed to remove file {}", full.display()))?;
            }
        }
        Ok(())
    }

    fn desired_owned_paths(&self, snapshot: &AppStructureSnapshotV3) -> BTreeSet<String> {
        let mut owned = BTreeSet::new();
        owned.insert("_meta".to_string());
        owned.insert("_meta/manifest.res.json".to_string());
        for node in &snapshot.nodes {
            owned.insert(node.path.clone());
            let mut current = Path::new(&node.path).parent();
            while let Some(parent) = current {
                let rel = parent.to_string_lossy().replace('\\', "/");
                if rel.is_empty() {
                    break;
                }
                if is_runtime_protected_path(&rel) {
                    break;
                }
                owned.insert(rel.clone());
                current = parent.parent();
            }
        }
        owned
    }

    fn validate_node(&self, node: &AppStructureNodeV3) -> Result<()> {
        if node.path.trim().is_empty() {
            anyhow::bail!("structure node path cannot be empty");
        }
        let rel = Path::new(&node.path);
        if rel.is_absolute() {
            anyhow::bail!("structure node path must be relative: {}", node.path);
        }
        for component in rel.components() {
            use std::path::Component;
            match component {
                Component::Normal(_) => {}
                _ => anyhow::bail!("structure node path is invalid: {}", node.path),
            }
        }
        if is_runtime_protected_path(&node.path) {
            anyhow::bail!(
                "connector-owned node may not target runtime-protected path: {}",
                node.path
            );
        }
        Ok(())
    }

    fn context(&self, request_id: &str) -> ConnectorContextV3 {
        ConnectorContextV3 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: None,
            trace_id: None,
        }
    }

    fn state_path(&self) -> PathBuf {
        self.app_dir()
            .join("_meta")
            .join(APP_STRUCTURE_SYNC_STATE_FILENAME)
    }

    fn load_state(&self) -> Result<AppStructureSyncStateDoc> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(AppStructureSyncStateDoc::default());
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    fn save_state(&self, state: &AppStructureSyncStateDoc) -> Result<()> {
        let path = self.state_path();
        ensure_parent_dir(&path)?;
        write_json_file(&path, &serde_json::to_value(state)?)
    }

    fn app_dir(&self) -> PathBuf {
        self.root.join(&self.app_id)
    }
}

fn bootstrap_runtime_scaffolding(root: &Path, app_id: &str) -> Result<()> {
    let app_dir = root.join(app_id);
    let stream_dir = app_dir.join("_stream");
    let replay_dir = stream_dir.join("from-seq");
    fs::create_dir_all(&replay_dir).with_context(|| {
        format!(
            "Failed to create runtime replay dir {}",
            replay_dir.display()
        )
    })?;

    let events_path = stream_dir.join("events.evt.jsonl");
    if !events_path.exists() {
        fs::write(&events_path, b"")
            .with_context(|| format!("Failed to initialize {}", events_path.display()))?;
    }

    let cursor_path = stream_dir.join("cursor.res.json");
    if !cursor_path.exists() {
        write_json_file(
            &cursor_path,
            &json!(CursorState {
                min_seq: 0,
                max_seq: 0,
                retention_hint_sec: DEFAULT_RETENTION_HINT_SEC,
            }),
        )?;
    }

    let jobs_path = stream_dir.join("inflight.jobs.res.json");
    if !jobs_path.exists() {
        write_json_file(&jobs_path, &json!([]))?;
    }

    let action_cursors_path = stream_dir.join(super::ACTION_CURSORS_FILENAME);
    if !action_cursors_path.exists() {
        write_json_file(
            &action_cursors_path,
            &serde_json::to_value(ActionCursorDoc::default())?,
        )?;
    }

    let snapshot_journal_path = stream_dir.join(SNAPSHOT_EXPAND_JOURNAL_FILENAME);
    if !snapshot_journal_path.exists() {
        write_json_file(&snapshot_journal_path, &json!({"resources": {}}))?;
    }

    Ok(())
}

fn write_json_file(path: &Path, value: &JsonValue) -> Result<()> {
    ensure_parent_dir(path)?;
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp, bytes)
        .with_context(|| format!("Failed to write temp file {}", tmp.display()))?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp, path)
        .with_context(|| format!("Failed to move {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
    Ok(())
}

fn is_runtime_protected_path(rel: &str) -> bool {
    rel == "_stream"
        || rel.starts_with("_stream/")
        || rel == format!("_meta/{APP_STRUCTURE_SYNC_STATE_FILENAME}")
}

fn structure_reason_label(reason: AppStructureSyncReasonV3) -> &'static str {
    match reason {
        AppStructureSyncReasonV3::Initialize => "initialize",
        AppStructureSyncReasonV3::EnterScope => "enter_scope",
        AppStructureSyncReasonV3::Refresh => "refresh",
        AppStructureSyncReasonV3::Recover => "recover",
    }
}

#[cfg(test)]
mod tests {
    use super::{bootstrap_runtime_scaffolding, AppTreeSyncService};
    use agentfs_sdk::DemoAppConnectorV2;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn structure_sync_initializes_tree_and_runtime_files() {
        let temp = TempDir::new().expect("tempdir");
        let mut service = AppTreeSyncService::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
        );
        let mut connector = DemoAppConnectorV2::new("aiim".to_string());
        service
            .sync_initial(&mut connector)
            .expect("initial structure sync should succeed");

        assert!(temp.path().join("aiim/_meta/manifest.res.json").exists());
        assert!(temp.path().join("aiim/_stream/events.evt.jsonl").exists());
        assert!(temp
            .path()
            .join("aiim/contacts/zhangsan/send_message.act")
            .exists());
        assert!(temp
            .path()
            .join("aiim/chats/chat-001/messages.res.jsonl")
            .exists());
    }

    #[test]
    fn structure_sync_refresh_prunes_connector_owned_and_keeps_runtime_owned() {
        let temp = TempDir::new().expect("tempdir");
        let mut service = AppTreeSyncService::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
        );
        let mut connector = DemoAppConnectorV2::new("aiim".to_string());
        service
            .sync_initial(&mut connector)
            .expect("initial structure sync should succeed");

        fs::write(temp.path().join("aiim/_stream/custom-runtime.log"), b"keep")
            .expect("write runtime-owned file");

        service
            .refresh(
                &mut connector,
                agentfs_sdk::AppStructureSyncReasonV3::EnterScope,
                Some("chat-long".to_string()),
                Some("/_app/enter_scope.act".to_string()),
            )
            .expect("scope refresh should succeed");

        assert!(!temp.path().join("aiim/chats/chat-001").exists());
        assert!(temp.path().join("aiim/chats/chat-long").exists());
        assert!(!temp
            .path()
            .join("aiim/chats/chat-001/messages.res.jsonl")
            .exists());
        assert!(temp
            .path()
            .join("aiim/chats/chat-long/messages.res.jsonl")
            .exists());
        assert!(temp.path().join("aiim/_stream/custom-runtime.log").exists());
    }

    #[test]
    fn failed_structure_refresh_leaves_previous_tree_intact() {
        let temp = TempDir::new().expect("tempdir");
        let mut service = AppTreeSyncService::new(
            temp.path().to_path_buf(),
            "aiim".to_string(),
            "sess-test".to_string(),
        );
        let mut connector = DemoAppConnectorV2::new("aiim".to_string());
        service
            .sync_initial(&mut connector)
            .expect("initial structure sync should succeed");

        fs::write(temp.path().join("aiim/_stream/custom-runtime.log"), b"keep")
            .expect("write runtime-owned file");

        let err = service
            .refresh(
                &mut connector,
                agentfs_sdk::AppStructureSyncReasonV3::EnterScope,
                Some("missing-scope".to_string()),
                Some("/_app/enter_scope.act".to_string()),
            )
            .expect_err("unknown scope should fail");
        assert!(err
            .to_string()
            .contains("unknown structure scope: missing-scope"));
        assert!(temp.path().join("aiim/chats/chat-001").exists());
        assert!(temp
            .path()
            .join("aiim/chats/chat-001/messages.res.jsonl")
            .exists());
        assert!(!temp.path().join("aiim/chats/chat-long").exists());
        assert!(temp.path().join("aiim/_stream/custom-runtime.log").exists());
    }

    #[test]
    fn bootstrap_runtime_scaffolding_is_idempotent() {
        let temp = TempDir::new().expect("tempdir");
        bootstrap_runtime_scaffolding(temp.path(), "aiim").expect("first bootstrap");
        bootstrap_runtime_scaffolding(temp.path(), "aiim").expect("second bootstrap");
        assert!(temp.path().join("aiim/_stream/from-seq").exists());
        assert!(temp
            .path()
            .join("aiim/_stream/action-cursors.res.json")
            .exists());
    }
}
