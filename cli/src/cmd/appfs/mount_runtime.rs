use agentfs_sdk::{
    BoxedFile, ConnectorContextV2, DirEntry, FetchSnapshotChunkRequestV2, FileSystem, FsError,
    SnapshotResumeV2, Stats, TimeChange, DEFAULT_DIR_MODE, DEFAULT_FILE_MODE,
};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::Value as JsonValue;
use std::collections::HashMap;
use std::io::{BufRead, Cursor};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use super::core::{build_app_connector, parse_manifest_contract_json};
use super::journal::{SnapshotExpandJournalDoc, SnapshotExpandJournalEntry};
use super::registry;
use super::shared::{
    action_template_matches, decode_jsonl_line, deterministic_shorten_segment,
    snapshot_expand_delay_ms, snapshot_force_expand_on_refresh, snapshot_publish_delay_ms,
};
use super::{
    AppfsRuntimeCliArgs, SnapshotOnTimeoutPolicy, SnapshotSpec, SNAPSHOT_EXPAND_JOURNAL_FILENAME,
};

const ROOT_INO: i64 = 1;
const OPEN_READ_ONLY: i32 = 0;
const OPEN_READ_WRITE: i32 = 2;
const OPEN_ACCESS_MODE_MASK: i32 = 0x0003;
const OPEN_WRITE_ONLY: i32 = 1;
const OPEN_NO_READ_HINT: i32 = 0x2000_0000;

#[cfg(target_os = "windows")]
const RAW_IO_ERROR: i32 = 1117;
#[cfg(not(target_os = "windows"))]
const RAW_IO_ERROR: i32 = libc::EIO;

#[cfg(target_os = "windows")]
const RAW_TIMEOUT_ERROR: i32 = 1460;
#[cfg(not(target_os = "windows"))]
const RAW_TIMEOUT_ERROR: i32 = libc::ETIMEDOUT;

type DynFs = Arc<Mutex<dyn FileSystem + Send>>;

#[derive(Clone)]
pub(crate) struct MountSnapshotReadThroughConfig {
    pub runtimes: Vec<AppfsRuntimeCliArgs>,
    pub managed: bool,
}

pub(crate) fn wrap_mount_runtime_filesystem(
    inner: DynFs,
    config: MountSnapshotReadThroughConfig,
) -> DynFs {
    Arc::new(Mutex::new(MountSnapshotReadThroughFs::new(inner, config)))
}

struct MountSnapshotReadThroughFs {
    inner: DynFs,
    config: MountSnapshotReadThroughConfig,
    runtime_configs: Mutex<HashMap<String, AppfsRuntimeCliArgs>>,
    registry_fingerprint: Mutex<Option<Vec<u8>>>,
    path_cache: Mutex<HashMap<i64, String>>,
    runtimes: Mutex<HashMap<String, MountSnapshotRuntime>>,
    expand_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

struct MountSnapshotRuntime {
    app_id: String,
    session_id: String,
    snapshot_specs: Vec<SnapshotSpec>,
    snapshot_expand_journal: HashMap<String, SnapshotExpandJournalEntry>,
    connector: Box<dyn agentfs_sdk::AppConnector>,
}

impl MountSnapshotRuntime {
    fn find_snapshot_spec(&self, resource_rel: &str) -> Option<&SnapshotSpec> {
        self.snapshot_specs
            .iter()
            .filter(|spec| action_template_matches(&spec.template, resource_rel))
            .max_by_key(|spec| super::shared::template_specificity(&spec.template))
    }

    fn journal_path(&self) -> String {
        format!(
            "{}/_stream/{}",
            self.app_id, SNAPSHOT_EXPAND_JOURNAL_FILENAME
        )
    }

    fn temp_rel_path(&self, resource_rel: &str) -> String {
        let mut sanitized = String::with_capacity(resource_rel.len() + 16);
        for ch in resource_rel.chars() {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                sanitized.push(ch);
            } else {
                sanitized.push('_');
            }
        }
        if sanitized.is_empty() {
            sanitized.push_str("snapshot");
        }
        if sanitized.len() > 160 {
            sanitized = deterministic_shorten_segment(&sanitized, 160);
        }
        format!(
            "{}/_stream/snapshot-expand-tmp/{}.pending.jsonl",
            self.app_id, sanitized
        )
    }

    fn request_context(&self, request_id: &str) -> ConnectorContextV2 {
        ConnectorContextV2 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: None,
            trace_id: None,
        }
    }
}

fn should_expand_on_open(
    stats: Option<&Stats>,
    has_journal: bool,
    force_expand_existing: bool,
) -> bool {
    if force_expand_existing || has_journal {
        return true;
    }
    match stats {
        None => true,
        Some(stats) => stats.size == 0,
    }
}

fn open_requests_read(flags: i32) -> bool {
    if (flags & OPEN_NO_READ_HINT) != 0 {
        return false;
    }
    let access_mode = flags & OPEN_ACCESS_MODE_MASK;
    access_mode != OPEN_WRITE_ONLY
}

fn should_skip_existing_expand(
    stats: Option<&Stats>,
    has_journal: bool,
    force_expand_existing: bool,
) -> bool {
    if has_journal || force_expand_existing {
        return false;
    }
    match stats {
        Some(stats) => stats.size > 0,
        None => false,
    }
}

impl MountSnapshotReadThroughFs {
    fn new(inner: DynFs, config: MountSnapshotReadThroughConfig) -> Self {
        let mut path_cache = HashMap::new();
        path_cache.insert(ROOT_INO, String::new());
        let runtime_configs = config
            .runtimes
            .iter()
            .cloned()
            .map(|runtime| (runtime.app_id.clone(), runtime))
            .collect();
        Self {
            inner,
            config,
            runtime_configs: Mutex::new(runtime_configs),
            registry_fingerprint: Mutex::new(None),
            path_cache: Mutex::new(path_cache),
            runtimes: Mutex::new(HashMap::new()),
            expand_locks: Mutex::new(HashMap::new()),
        }
    }

    async fn cached_path_for_ino(&self, ino: i64) -> Option<String> {
        self.path_cache.lock().await.get(&ino).cloned()
    }

    async fn cache_path(&self, ino: i64, rel_path: String) {
        self.path_cache.lock().await.insert(ino, rel_path);
    }

    async fn expand_lock_for(&self, app_id: &str, resource_rel: &str) -> Arc<Mutex<()>> {
        let mut locks = self.expand_locks.lock().await;
        let key = format!("{app_id}:{resource_rel}");
        locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn runtime_loaded(&self, app_id: &str) -> Result<bool> {
        self.refresh_registry_snapshot().await?;
        let Some(runtime_config) = self.runtime_configs.lock().await.get(app_id).cloned() else {
            return Ok(false);
        };

        let mut guard = self.runtimes.lock().await;
        if guard.contains_key(app_id) {
            return Ok(true);
        }

        let manifest_rel = format!("{app_id}/_meta/manifest.res.json");
        let manifest_bytes = match self.read_file_if_exists(&manifest_rel).await? {
            Some(bytes) => bytes,
            None => return Ok(false),
        };
        let manifest_json = String::from_utf8(manifest_bytes)
            .with_context(|| format!("manifest is not valid UTF-8: /{manifest_rel}"))?;
        let manifest_contract =
            parse_manifest_contract_json(&manifest_json, &format!("/{}", manifest_rel))?;
        let session_id = super::normalize_appfs_session_id(runtime_config.session_id.clone());
        let bridge_config = super::build_appfs_bridge_config(runtime_config.bridge.clone());
        let connector = build_app_connector(app_id, &bridge_config)?;
        let journal_path = format!("{app_id}/_stream/{}", SNAPSHOT_EXPAND_JOURNAL_FILENAME);
        let snapshot_expand_journal = match self.read_file_if_exists(&journal_path).await? {
            Some(bytes) => {
                let doc: SnapshotExpandJournalDoc = serde_json::from_slice(&bytes)
                    .with_context(|| format!("Failed to parse /{journal_path}"))?;
                doc.resources
            }
            None => HashMap::new(),
        };

        let mut snapshot_expand_journal = snapshot_expand_journal;
        self.recover_incomplete_expands(app_id, &journal_path, &mut snapshot_expand_journal)
            .await?;
        let runtime = MountSnapshotRuntime {
            app_id: app_id.to_string(),
            session_id,
            snapshot_specs: manifest_contract.snapshot_specs,
            snapshot_expand_journal,
            connector,
        };
        guard.insert(app_id.to_string(), runtime);
        Ok(true)
    }

    async fn refresh_registry_snapshot(&self) -> Result<()> {
        if !self.config.managed {
            return Ok(());
        }
        let Some(bytes) = self
            .read_file_if_exists(registry::APPFS_REGISTRY_REL_PATH)
            .await?
        else {
            return Ok(());
        };
        {
            let fingerprint = self.registry_fingerprint.lock().await;
            if fingerprint.as_deref() == Some(bytes.as_slice()) {
                return Ok(());
            }
        }
        let doc = registry::parse_app_registry_bytes(&bytes)
            .context("failed to load managed AppFS registry for mount read-through")?;
        let runtime_args = registry::runtime_args_from_registry(&doc)?;
        let runtime_configs = runtime_args
            .into_iter()
            .map(|runtime| (runtime.app_id.clone(), runtime))
            .collect::<HashMap<_, _>>();
        *self.runtime_configs.lock().await = runtime_configs;
        *self.registry_fingerprint.lock().await = Some(bytes);
        self.runtimes.lock().await.clear();
        Ok(())
    }

    async fn recover_incomplete_expands(
        &self,
        app_id: &str,
        journal_path: &str,
        journal_resources: &mut HashMap<String, SnapshotExpandJournalEntry>,
    ) -> Result<()> {
        if journal_resources.is_empty() {
            return Ok(());
        }

        let entries: Vec<(String, SnapshotExpandJournalEntry)> = journal_resources
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (resource_rel, entry) in entries {
            if let Some(temp_artifact) = entry.temp_artifact.as_deref() {
                if self.is_valid_temp_artifact(app_id, temp_artifact) {
                    let temp_rel = temp_artifact.trim_start_matches('/');
                    let _ = self.remove_file_if_exists(temp_rel).await;
                }
            }
            eprintln!(
                "[recovery] mount snapshot expand incomplete resource=/{} status={} -> cold",
                resource_rel, entry.status
            );
        }
        journal_resources.clear();
        self.save_snapshot_expand_journal(journal_path, journal_resources)
            .await
    }

    fn is_valid_temp_artifact(&self, app_id: &str, temp_artifact: &str) -> bool {
        let trimmed = temp_artifact.trim().trim_start_matches('/');
        if trimmed.is_empty() {
            return false;
        }
        let rel_path = Path::new(trimmed);
        if rel_path.is_absolute() {
            return false;
        }
        let mut normalized = PathBuf::new();
        for component in rel_path.components() {
            match component {
                Component::Normal(seg) => normalized.push(seg),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => return false,
            }
        }
        normalized.starts_with(
            Path::new(app_id)
                .join("_stream")
                .join("snapshot-expand-tmp"),
        )
    }

    async fn maybe_snapshot_resource_from_fs_path(
        &self,
        fs_rel_path: &str,
    ) -> Result<Option<(String, String)>> {
        let Some((app_id, resource_rel)) = split_app_relative_path(fs_rel_path) else {
            return Ok(None);
        };
        if !self.runtime_loaded(app_id).await? {
            return Ok(None);
        }
        let guard = self.runtimes.lock().await;
        let Some(runtime) = guard.get(app_id) else {
            return Ok(None);
        };
        Ok(runtime
            .find_snapshot_spec(resource_rel)
            .map(|_| (app_id.to_string(), resource_rel.to_string())))
    }

    async fn journal_contains(&self, app_id: &str, resource_rel: &str) -> Result<bool> {
        if !self.runtime_loaded(app_id).await? {
            return Ok(false);
        }
        let guard = self.runtimes.lock().await;
        Ok(guard
            .get(app_id)
            .is_some_and(|runtime| runtime.snapshot_expand_journal.contains_key(resource_rel)))
    }

    async fn ensure_snapshot_materialized(
        &self,
        app_id: &str,
        resource_rel: &str,
        trigger: &str,
    ) -> Result<()> {
        if !self.runtime_loaded(app_id).await? {
            anyhow::bail!("AppFS manifest is not available yet for mount-side read-through");
        }

        let expand_lock = self.expand_lock_for(app_id, resource_rel).await;
        let _expand_guard = expand_lock.lock().await;
        let request_id = format!("read-{}", uuid::Uuid::new_v4().simple());
        let resource_path = format!("/{}", resource_rel);
        let (snapshot_spec, temp_rel) = {
            let mut guard = self.runtimes.lock().await;
            let runtime = guard
                .get_mut(app_id)
                .expect("runtime_loaded() should initialize runtime");
            let snapshot_spec = runtime
                .find_snapshot_spec(resource_rel)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("snapshot resource is not declared in manifest"))?;
            let temp_rel = runtime.temp_rel_path(resource_rel);
            (snapshot_spec, temp_rel)
        };
        let resource_fs_rel = format!("{}/{}", app_id, resource_rel);
        let has_journal = self.journal_contains(app_id, resource_rel).await?;
        let force_expand_existing = trigger == "open" && snapshot_force_expand_on_refresh();
        let current_stats = self.lookup_path(&resource_fs_rel).await?;
        if should_skip_existing_expand(current_stats.as_ref(), has_journal, force_expand_existing) {
            if trigger == "lookup_miss" {
                eprintln!(
                    "[cache] coalesced concurrent miss resource={} trigger={}",
                    resource_path, trigger
                );
            }
            return Ok(());
        }

        {
            let mut guard = self.runtimes.lock().await;
            let runtime = guard
                .get_mut(app_id)
                .expect("runtime_loaded() should initialize runtime");
            runtime.snapshot_expand_journal.insert(
                resource_rel.to_string(),
                SnapshotExpandJournalEntry {
                    resource_path: resource_path.clone(),
                    status: "warming".to_string(),
                    request_id: request_id.clone(),
                    started_at: chrono::Utc::now().to_rfc3339(),
                    updated_at: chrono::Utc::now().to_rfc3339(),
                    temp_artifact: Some(format!("/{}", temp_rel.clone())),
                },
            );
        }
        self.flush_runtime_snapshot_journal(app_id).await?;

        eprintln!(
            "[cache] mount read-through resource={} trigger={} timeout_ms={} on_timeout={}",
            resource_path,
            trigger,
            snapshot_spec.read_through_timeout_ms,
            snapshot_spec.on_timeout.as_str()
        );

        let simulated_delay_ms = snapshot_expand_delay_ms();
        if simulated_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(simulated_delay_ms));
        }

        if simulated_delay_ms > snapshot_spec.read_through_timeout_ms {
            let stale_health = if matches!(
                snapshot_spec.on_timeout,
                SnapshotOnTimeoutPolicy::ReturnStale
            ) {
                if let Some(stale_bytes) = self.read_file_if_exists(&resource_fs_rel).await? {
                    let stale_size = stale_bytes.len();
                    if stale_size > snapshot_spec.max_materialized_bytes {
                        Some("stale_cache_too_large".to_string())
                    } else if self.validate_stale_snapshot_jsonl(&stale_bytes).is_ok() {
                        None
                    } else {
                        Some("stale_cache_unhealthy".to_string())
                    }
                } else {
                    Some("stale_cache_missing".to_string())
                }
            } else {
                Some("stale_cache_disabled".to_string())
            };
            let stale_ok = stale_health.is_none();
            self.remove_snapshot_journal_entry(app_id, resource_rel)
                .await?;
            if stale_ok {
                eprintln!(
                    "[cache] timeout_return_stale resource={} trigger={}",
                    resource_path, trigger
                );
                return Ok(());
            }
            eprintln!(
                "[cache] expand failed resource={} phase=timeout on_timeout={} stale_reason={} trigger={}",
                resource_path,
                snapshot_spec.on_timeout.as_str(),
                stale_health
                    .as_deref()
                    .unwrap_or("stale_cache_missing"),
                trigger
            );
            return Err(io_error(
                RAW_TIMEOUT_ERROR,
                format!("snapshot read-through timed out: {}", resource_path),
            )
            .into());
        }

        let expanded_jsonl = {
            let mut guard = self.runtimes.lock().await;
            let runtime = guard
                .get_mut(app_id)
                .expect("runtime_loaded() should initialize runtime");
            self.fetch_snapshot_jsonl_from_upstream(runtime, resource_rel, &request_id)?
        };
        if expanded_jsonl.len() > snapshot_spec.max_materialized_bytes {
            self.remove_snapshot_journal_entry(app_id, resource_rel)
                .await?;
            eprintln!(
                "[cache] snapshot_too_large resource={} size={} max_size={} trigger={}",
                resource_path,
                expanded_jsonl.len(),
                snapshot_spec.max_materialized_bytes,
                trigger
            );
            return Err(io_error(
                RAW_IO_ERROR,
                format!(
                    "snapshot too large for {}: {} > {}",
                    resource_path,
                    expanded_jsonl.len(),
                    snapshot_spec.max_materialized_bytes
                ),
            )
            .into());
        }

        self.materialize_snapshot_file(
            app_id,
            resource_rel,
            &temp_rel,
            &expanded_jsonl,
            &request_id,
        )
        .await?;
        self.remove_snapshot_journal_entry(app_id, resource_rel)
            .await?;
        eprintln!(
            "[cache] expanded resource={} bytes={} trigger={}",
            resource_path,
            expanded_jsonl.len(),
            trigger
        );
        Ok(())
    }

    fn fetch_snapshot_jsonl_from_upstream(
        &self,
        runtime: &mut MountSnapshotRuntime,
        resource_rel: &str,
        request_id: &str,
    ) -> Result<Vec<u8>> {
        eprintln!(
            "[cache.expand] fetch_snapshot_chunk resource=/{} trigger=read",
            resource_rel
        );
        let mut out = Vec::new();
        let mut resume = SnapshotResumeV2::Start;
        let budget_bytes = 1_048_576_u64;
        loop {
            let response = runtime
                .connector
                .fetch_snapshot_chunk(
                    FetchSnapshotChunkRequestV2 {
                        resource_path: format!("/{}", resource_rel),
                        resume,
                        budget_bytes,
                    },
                    &runtime.request_context(request_id),
                )
                .map_err(|err| {
                    io_error(
                        RAW_IO_ERROR,
                        format!(
                            "connector fetch_snapshot_chunk failed code={} message={}",
                            err.code, err.message
                        ),
                    )
                })?;
            for record in response.records {
                let line = serde_json::to_vec(&record.line).map_err(io_error_eio)?;
                out.extend_from_slice(&line);
                out.push(b'\n');
            }
            if !response.has_more {
                break;
            }
            let next_cursor = response.next_cursor.ok_or_else(|| {
                io_error(
                    RAW_IO_ERROR,
                    "connector response missing next_cursor while has_more=true".to_string(),
                )
            })?;
            resume = SnapshotResumeV2::Cursor(next_cursor);
        }
        if out.is_empty() {
            return Err(io_error(
                RAW_IO_ERROR,
                format!(
                    "connector returned empty snapshot stream for /{}",
                    resource_rel
                ),
            )
            .into());
        }
        Ok(out)
    }

    async fn materialize_snapshot_file(
        &self,
        app_id: &str,
        resource_rel: &str,
        temp_rel: &str,
        content: &[u8],
        request_id: &str,
    ) -> Result<()> {
        let target_rel = format!("{}/{}", app_id, resource_rel);
        self.write_file_bytes(temp_rel, content).await?;

        self.mark_snapshot_journal_publishing(app_id, resource_rel, request_id, temp_rel)
            .await?;

        let publish_delay_ms = snapshot_publish_delay_ms();
        if publish_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(publish_delay_ms));
        }

        self.remove_file_if_exists(&target_rel).await?;
        self.rename_path(temp_rel, &target_rel).await
    }

    async fn save_snapshot_expand_journal(
        &self,
        journal_path: &str,
        resources: &HashMap<String, SnapshotExpandJournalEntry>,
    ) -> Result<()> {
        let doc = SnapshotExpandJournalDoc {
            resources: resources.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&doc).map_err(io_error_eio)?;
        self.write_file_bytes(journal_path, &bytes).await
    }

    async fn flush_runtime_snapshot_journal(&self, app_id: &str) -> Result<()> {
        let (journal_path, resources) = {
            let guard = self.runtimes.lock().await;
            let runtime = guard
                .get(app_id)
                .expect("runtime_loaded() should initialize runtime");
            (
                runtime.journal_path(),
                runtime.snapshot_expand_journal.clone(),
            )
        };
        self.save_snapshot_expand_journal(&journal_path, &resources)
            .await
    }

    async fn remove_snapshot_journal_entry(&self, app_id: &str, resource_rel: &str) -> Result<()> {
        let (journal_path, resources) = {
            let mut guard = self.runtimes.lock().await;
            let runtime = guard
                .get_mut(app_id)
                .expect("runtime_loaded() should initialize runtime");
            runtime.snapshot_expand_journal.remove(resource_rel);
            (
                runtime.journal_path(),
                runtime.snapshot_expand_journal.clone(),
            )
        };
        self.save_snapshot_expand_journal(&journal_path, &resources)
            .await
    }

    async fn mark_snapshot_journal_publishing(
        &self,
        app_id: &str,
        resource_rel: &str,
        request_id: &str,
        temp_rel: &str,
    ) -> Result<()> {
        let (journal_path, resources) = {
            let mut guard = self.runtimes.lock().await;
            let runtime = guard
                .get_mut(app_id)
                .expect("runtime_loaded() should initialize runtime");
            if let Some(entry) = runtime.snapshot_expand_journal.get_mut(resource_rel) {
                entry.status = "publishing".to_string();
                entry.request_id = request_id.to_string();
                entry.updated_at = chrono::Utc::now().to_rfc3339();
                entry.temp_artifact = Some(format!("/{}", temp_rel));
            }
            (
                runtime.journal_path(),
                runtime.snapshot_expand_journal.clone(),
            )
        };
        self.save_snapshot_expand_journal(&journal_path, &resources)
            .await
    }

    fn validate_stale_snapshot_jsonl(&self, bytes: &[u8]) -> std::result::Result<usize, String> {
        let mut reader = Cursor::new(bytes);
        let mut line_buf = Vec::new();
        let mut valid_lines = 0usize;
        let mut line_no = 0usize;
        loop {
            line_buf.clear();
            let read = reader
                .read_until(b'\n', &mut line_buf)
                .map_err(|err| format!("read_failed line={} err={err}", line_no + 1))?;
            if read == 0 {
                break;
            }
            line_no += 1;
            let Some(line) = decode_jsonl_line(&line_buf, line_no == 1)
                .map_err(|err| format!("decode_failed line={line_no} err={err}"))?
            else {
                continue;
            };
            let value: JsonValue = serde_json::from_str(&line)
                .map_err(|err| format!("parse_failed line={line_no} err={err}"))?;
            if !value.is_object() {
                return Err(format!("non_object_json line={line_no}"));
            }
            valid_lines += 1;
        }
        if valid_lines == 0 {
            return Err("empty_or_blank_snapshot".to_string());
        }
        Ok(valid_lines)
    }

    async fn read_file_if_exists(&self, rel_path: &str) -> Result<Option<Vec<u8>>> {
        let Some(stats) = self.lookup_path(rel_path).await? else {
            return Ok(None);
        };
        let file = {
            let fs = self.inner.lock().await;
            fs.open(stats.ino, OPEN_READ_ONLY).await?
        };
        let bytes = file.pread(0, stats.size.max(0) as u64).await?;
        Ok(Some(bytes))
    }

    async fn write_file_bytes(&self, rel_path: &str, bytes: &[u8]) -> Result<()> {
        let (parent_ino, name) = self.ensure_parent_dir(rel_path).await?;
        let file = if let Some(stats) = self.lookup_path(rel_path).await? {
            let fs = self.inner.lock().await;
            fs.open(stats.ino, OPEN_READ_WRITE).await?
        } else {
            let (stats, file) = {
                let fs = self.inner.lock().await;
                fs.create_file(parent_ino, &name, DEFAULT_FILE_MODE, 0, 0)
                    .await?
            };
            self.cache_path(stats.ino, rel_path.to_string()).await;
            file
        };
        file.truncate(0).await?;
        if !bytes.is_empty() {
            file.pwrite(0, bytes).await?;
        }
        file.fsync().await?;
        Ok(())
    }

    async fn remove_file_if_exists(&self, rel_path: &str) -> Result<()> {
        let Some((parent_ino, name)) = self.parent_lookup(rel_path).await? else {
            return Ok(());
        };
        let exists = {
            let fs = self.inner.lock().await;
            fs.lookup(parent_ino, &name).await?
        };
        if exists.is_some() {
            let fs = self.inner.lock().await;
            fs.unlink(parent_ino, &name).await?;
        }
        Ok(())
    }

    async fn rename_path(&self, old_rel: &str, new_rel: &str) -> Result<()> {
        let (old_parent_ino, old_name) = self
            .parent_lookup(old_rel)
            .await?
            .ok_or_else(|| anyhow::anyhow!("rename source parent missing: /{old_rel}"))?;
        let (new_parent_ino, new_name) = self.ensure_parent_dir(new_rel).await?;
        let fs = self.inner.lock().await;
        fs.rename(old_parent_ino, &old_name, new_parent_ino, &new_name)
            .await?;
        Ok(())
    }

    async fn lookup_path(&self, rel_path: &str) -> Result<Option<Stats>> {
        let mut current_ino = ROOT_INO;
        if rel_path.is_empty() {
            let fs = self.inner.lock().await;
            return fs.getattr(ROOT_INO).await.map_err(Into::into);
        }
        for component in rel_path.split('/').filter(|segment| !segment.is_empty()) {
            let next = {
                let fs = self.inner.lock().await;
                fs.lookup(current_ino, component).await?
            };
            let Some(stats) = next else {
                return Ok(None);
            };
            current_ino = stats.ino;
        }
        let fs = self.inner.lock().await;
        fs.getattr(current_ino).await.map_err(Into::into)
    }

    async fn ensure_parent_dir(&self, rel_path: &str) -> Result<(i64, String)> {
        let path = Path::new(rel_path);
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid path without file name: /{rel_path}"))?
            .to_string();
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let mut current_ino = ROOT_INO;
        let mut current_path = String::new();
        for component in parent.components() {
            let Component::Normal(seg) = component else {
                continue;
            };
            let seg = seg
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-utf8 path component in /{rel_path}"))?;
            let next = {
                let fs = self.inner.lock().await;
                fs.lookup(current_ino, seg).await?
            };
            let stats = if let Some(stats) = next {
                stats
            } else {
                let fs = self.inner.lock().await;
                fs.mkdir(current_ino, seg, DEFAULT_DIR_MODE, 0, 0).await?
            };
            current_path = join_rel_path(&current_path, seg);
            self.cache_path(stats.ino, current_path.clone()).await;
            current_ino = stats.ino;
        }
        Ok((current_ino, name))
    }

    async fn parent_lookup(&self, rel_path: &str) -> Result<Option<(i64, String)>> {
        let path = Path::new(rel_path);
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Ok(None);
        };
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let parent_rel = parent.to_string_lossy().replace('\\', "/");
        let Some(parent_stats) = self.lookup_path(&parent_rel).await? else {
            return Ok(None);
        };
        Ok(Some((parent_stats.ino, name.to_string())))
    }
}

#[async_trait]
impl FileSystem for MountSnapshotReadThroughFs {
    async fn lookup(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<Option<Stats>, agentfs_sdk::error::Error> {
        let parent_path = self.cached_path_for_ino(parent_ino).await;
        let initial = {
            let fs = self.inner.lock().await;
            fs.lookup(parent_ino, name).await?
        };
        if let Some(stats) = initial {
            if let Some(parent_path) = parent_path {
                self.cache_path(stats.ino, join_rel_path(&parent_path, name))
                    .await;
            }
            return Ok(Some(stats));
        }

        if let Some(parent_path) = parent_path {
            let candidate = join_rel_path(&parent_path, name);
            if let Some((resource_app_id, resource_rel)) = self
                .maybe_snapshot_resource_from_fs_path(&candidate)
                .await
                .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?
            {
                self.ensure_snapshot_materialized(&resource_app_id, &resource_rel, "lookup_miss")
                    .await
                    .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?;
                let retry = {
                    let fs = self.inner.lock().await;
                    fs.lookup(parent_ino, name).await?
                };
                if let Some(stats) = retry {
                    self.cache_path(stats.ino, candidate).await;
                    return Ok(Some(stats));
                }
            }
        }
        Ok(None)
    }

    async fn getattr(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Stats>, agentfs_sdk::error::Error> {
        self.inner.lock().await.getattr(ino).await
    }

    async fn readlink(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<String>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readlink(ino).await
    }

    async fn readdir(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Vec<String>>, agentfs_sdk::error::Error> {
        self.inner.lock().await.readdir(ino).await
    }

    async fn readdir_plus(
        &self,
        ino: i64,
    ) -> std::result::Result<Option<Vec<DirEntry>>, agentfs_sdk::error::Error> {
        let entries = self.inner.lock().await.readdir_plus(ino).await?;
        if let (Some(parent_path), Some(entries)) =
            (self.cached_path_for_ino(ino).await, entries.as_ref())
        {
            for entry in entries {
                self.cache_path(entry.stats.ino, join_rel_path(&parent_path, &entry.name))
                    .await;
            }
        }
        Ok(entries)
    }

    async fn chmod(
        &self,
        ino: i64,
        mode: u32,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chmod(ino, mode).await
    }

    async fn chown(
        &self,
        ino: i64,
        uid: Option<u32>,
        gid: Option<u32>,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.chown(ino, uid, gid).await
    }

    async fn utimens(
        &self,
        ino: i64,
        atime: TimeChange,
        mtime: TimeChange,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.utimens(ino, atime, mtime).await
    }

    async fn open(
        &self,
        ino: i64,
        flags: i32,
    ) -> std::result::Result<BoxedFile, agentfs_sdk::error::Error> {
        let mut target_ino = ino;
        if open_requests_read(flags) {
            if let Some(rel_path) = self.cached_path_for_ino(ino).await {
                if let Some((resource_app_id, resource_rel)) = self
                    .maybe_snapshot_resource_from_fs_path(&rel_path)
                    .await
                    .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?
                {
                    let should_force_expand = snapshot_force_expand_on_refresh();
                    let has_journal = self
                        .journal_contains(&resource_app_id, &resource_rel)
                        .await
                        .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?;
                    let current_stats = self
                        .lookup_path(&format!("{}/{}", resource_app_id, resource_rel))
                        .await
                        .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?;
                    eprintln!(
                        "[cache.open] app={} resource=/{} flags=0x{:x} size={} has_journal={} force_expand={} read_intent=true",
                        resource_app_id,
                        resource_rel,
                        flags,
                        current_stats.as_ref().map(|stats| stats.size).unwrap_or(-1),
                        has_journal,
                        should_force_expand
                    );
                    if should_expand_on_open(
                        current_stats.as_ref(),
                        has_journal,
                        should_force_expand,
                    ) {
                        self.ensure_snapshot_materialized(&resource_app_id, &resource_rel, "open")
                            .await
                            .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?;
                        if let Some(stats) = self
                            .lookup_path(&format!("{}/{}", resource_app_id, resource_rel))
                            .await
                            .map_err(|err| map_anyhow_to_sdk_error(err, RAW_IO_ERROR))?
                        {
                            target_ino = stats.ino;
                            self.cache_path(target_ino, rel_path).await;
                        } else {
                            return Err(FsError::NotFound.into());
                        }
                    }
                }
            }
        }
        self.inner.lock().await.open(target_ino, flags).await
    }

    async fn mkdir(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<Stats, agentfs_sdk::error::Error> {
        let stats = self
            .inner
            .lock()
            .await
            .mkdir(parent_ino, name, mode, uid, gid)
            .await?;
        if let Some(parent_path) = self.cached_path_for_ino(parent_ino).await {
            self.cache_path(stats.ino, join_rel_path(&parent_path, name))
                .await;
        }
        Ok(stats)
    }

    async fn create_file(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<(Stats, BoxedFile), agentfs_sdk::error::Error> {
        let (stats, file) = self
            .inner
            .lock()
            .await
            .create_file(parent_ino, name, mode, uid, gid)
            .await?;
        if let Some(parent_path) = self.cached_path_for_ino(parent_ino).await {
            self.cache_path(stats.ino, join_rel_path(&parent_path, name))
                .await;
        }
        Ok((stats, file))
    }

    async fn mknod(
        &self,
        parent_ino: i64,
        name: &str,
        mode: u32,
        rdev: u64,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<Stats, agentfs_sdk::error::Error> {
        let stats = self
            .inner
            .lock()
            .await
            .mknod(parent_ino, name, mode, rdev, uid, gid)
            .await?;
        if let Some(parent_path) = self.cached_path_for_ino(parent_ino).await {
            self.cache_path(stats.ino, join_rel_path(&parent_path, name))
                .await;
        }
        Ok(stats)
    }

    async fn symlink(
        &self,
        parent_ino: i64,
        name: &str,
        target: &str,
        uid: u32,
        gid: u32,
    ) -> std::result::Result<Stats, agentfs_sdk::error::Error> {
        let stats = self
            .inner
            .lock()
            .await
            .symlink(parent_ino, name, target, uid, gid)
            .await?;
        if let Some(parent_path) = self.cached_path_for_ino(parent_ino).await {
            self.cache_path(stats.ino, join_rel_path(&parent_path, name))
                .await;
        }
        Ok(stats)
    }

    async fn unlink(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.unlink(parent_ino, name).await
    }

    async fn rmdir(
        &self,
        parent_ino: i64,
        name: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner.lock().await.rmdir(parent_ino, name).await
    }

    async fn link(
        &self,
        ino: i64,
        newparent_ino: i64,
        newname: &str,
    ) -> std::result::Result<Stats, agentfs_sdk::error::Error> {
        let stats = self
            .inner
            .lock()
            .await
            .link(ino, newparent_ino, newname)
            .await?;
        if let Some(parent_path) = self.cached_path_for_ino(newparent_ino).await {
            self.cache_path(stats.ino, join_rel_path(&parent_path, newname))
                .await;
        }
        Ok(stats)
    }

    async fn rename(
        &self,
        oldparent_ino: i64,
        oldname: &str,
        newparent_ino: i64,
        newname: &str,
    ) -> std::result::Result<(), agentfs_sdk::error::Error> {
        self.inner
            .lock()
            .await
            .rename(oldparent_ino, oldname, newparent_ino, newname)
            .await
    }

    async fn statfs(
        &self,
    ) -> std::result::Result<agentfs_sdk::FilesystemStats, agentfs_sdk::error::Error> {
        self.inner.lock().await.statfs().await
    }
}

fn join_rel_path(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

fn split_app_relative_path(rel_path: &str) -> Option<(&str, &str)> {
    let trimmed = rel_path.trim_matches('/');
    let (app_id, rest) = trimmed.split_once('/')?;
    if app_id.is_empty() || rest.is_empty() {
        return None;
    }
    Some((app_id, rest))
}

fn io_error(errno: i32, message: String) -> std::io::Error {
    let _ = message;
    std::io::Error::from_raw_os_error(errno)
}

fn io_error_eio<E: std::fmt::Display>(err: E) -> std::io::Error {
    io_error(RAW_IO_ERROR, err.to_string())
}

fn map_anyhow_to_sdk_error(err: anyhow::Error, default_errno: i32) -> agentfs_sdk::error::Error {
    match err.downcast::<std::io::Error>() {
        Ok(io_err) => io_err.into(),
        Err(other) => io_error(default_errno, other.to_string()).into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        open_requests_read, should_expand_on_open, should_skip_existing_expand, OPEN_NO_READ_HINT,
        OPEN_READ_ONLY, OPEN_READ_WRITE, OPEN_WRITE_ONLY,
    };
    use agentfs_sdk::{Stats, DEFAULT_FILE_MODE};

    fn file_stats(size: u64) -> Stats {
        Stats {
            ino: 1,
            mode: DEFAULT_FILE_MODE,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: size as i64,
            atime: 0,
            mtime: 0,
            ctime: 0,
            atime_nsec: 0,
            mtime_nsec: 0,
            ctime_nsec: 0,
            rdev: 0,
        }
    }

    #[test]
    fn open_expands_missing_placeholder_or_journaled_snapshot() {
        assert!(should_expand_on_open(None, false, false));
        assert!(should_expand_on_open(Some(&file_stats(0)), false, false));
        assert!(should_expand_on_open(Some(&file_stats(128)), true, false));
        assert!(should_expand_on_open(Some(&file_stats(128)), false, true));
        assert!(!should_expand_on_open(Some(&file_stats(128)), false, false));
    }

    #[test]
    fn existing_expand_skips_only_non_empty_materialized_files() {
        assert!(!should_skip_existing_expand(None, false, false));
        assert!(!should_skip_existing_expand(
            Some(&file_stats(0)),
            false,
            false
        ));
        assert!(!should_skip_existing_expand(
            Some(&file_stats(128)),
            true,
            false
        ));
        assert!(!should_skip_existing_expand(
            Some(&file_stats(128)),
            false,
            true
        ));
        assert!(should_skip_existing_expand(
            Some(&file_stats(128)),
            false,
            false
        ));
    }

    #[test]
    fn write_only_open_does_not_count_as_read_intent() {
        assert!(open_requests_read(OPEN_READ_ONLY));
        assert!(open_requests_read(OPEN_READ_WRITE));
        assert!(!open_requests_read(OPEN_WRITE_ONLY));
        assert!(!open_requests_read(OPEN_READ_ONLY | OPEN_NO_READ_HINT));
    }
}
