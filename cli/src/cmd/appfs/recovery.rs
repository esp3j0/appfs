use anyhow::Result;
use serde_json::json;
use std::fs;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

use super::journal::SnapshotExpandJournalEntry;
use super::{AppfsAdapter, SnapshotCacheState};

impl AppfsAdapter {
    pub(super) fn resolve_snapshot_expand_cleanup_target(
        &self,
        temp_artifact: &str,
    ) -> std::result::Result<PathBuf, String> {
        let trimmed = temp_artifact.trim();
        if trimmed.is_empty() {
            return Err("empty_temp_artifact".to_string());
        }

        let rel_raw = trimmed.trim_start_matches(['/', '\\']);
        let rel_path = Path::new(rel_raw);
        if rel_path.is_absolute() {
            return Err("absolute_temp_artifact_path".to_string());
        }

        let mut normalized = PathBuf::new();
        for component in rel_path.components() {
            match component {
                Component::Normal(seg) => normalized.push(seg),
                Component::CurDir => {}
                Component::ParentDir => return Err("parent_dir_component_not_allowed".to_string()),
                Component::RootDir | Component::Prefix(_) => {
                    return Err("root_or_prefix_component_not_allowed".to_string())
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            return Err("empty_normalized_temp_artifact_path".to_string());
        }

        let allowed_prefix = Path::new("_stream").join("snapshot-expand-tmp");
        if !normalized.starts_with(&allowed_prefix) {
            return Err(format!(
                "temp_artifact_outside_allowed_prefix path={}",
                normalized.display()
            ));
        }

        let app_root_canonical = fs::canonicalize(&self.app_dir)
            .map_err(|err| format!("canonicalize_app_root_failed: {err}"))?;
        let joined = self.app_dir.join(&normalized);

        if joined.exists() {
            let target_canonical = fs::canonicalize(&joined)
                .map_err(|err| format!("canonicalize_temp_artifact_failed: {err}"))?;
            if !target_canonical.starts_with(&app_root_canonical) {
                return Err(format!(
                    "temp_artifact_outside_app_root target={} app_root={}",
                    target_canonical.display(),
                    app_root_canonical.display()
                ));
            }
            return Ok(joined);
        }

        let Some(parent) = joined.parent() else {
            return Err("temp_artifact_has_no_parent".to_string());
        };
        if parent.exists() {
            let parent_canonical = fs::canonicalize(parent)
                .map_err(|err| format!("canonicalize_temp_artifact_parent_failed: {err}"))?;
            if !parent_canonical.starts_with(&app_root_canonical) {
                return Err(format!(
                    "temp_artifact_parent_outside_app_root parent={} app_root={}",
                    parent_canonical.display(),
                    app_root_canonical.display()
                ));
            }
        }

        Ok(joined)
    }

    pub(super) fn recover_snapshot_expand_journal(&mut self) -> Result<()> {
        if self.snapshot_expand_journal.is_empty() {
            return Ok(());
        }

        let entries: Vec<(String, SnapshotExpandJournalEntry)> = self
            .snapshot_expand_journal
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        for (resource_rel, entry) in entries {
            let mut cleaned_temp = false;
            let mut cleanup_status = "not_requested";
            let mut cleanup_detail: Option<String> = None;
            if let Some(temp_artifact) = entry.temp_artifact.clone() {
                match self.resolve_snapshot_expand_cleanup_target(&temp_artifact) {
                    Ok(temp_abs) => {
                        if temp_abs.exists() {
                            match fs::remove_file(&temp_abs) {
                                Ok(_) => {
                                    cleaned_temp = true;
                                    cleanup_status = "deleted";
                                }
                                Err(err) => {
                                    cleanup_status = "delete_failed";
                                    cleanup_detail = Some(err.to_string());
                                    eprintln!(
                                        "[recovery] snapshot temp cleanup failed resource=/{} artifact={} err={}",
                                        resource_rel,
                                        temp_abs.display(),
                                        err
                                    );
                                }
                            }
                        } else {
                            cleanup_status = "missing";
                        }
                    }
                    Err(reason) => {
                        cleanup_status = "rejected";
                        cleanup_detail = Some(reason.clone());
                        eprintln!(
                            "[recovery] snapshot temp cleanup skipped resource=/{} artifact={} reason={}",
                            resource_rel, temp_artifact, reason
                        );
                    }
                }
            }

            self.transition_snapshot_state(&resource_rel, SnapshotCacheState::Cold);
            self.clear_snapshot_recent_expand(&resource_rel);
            eprintln!(
                "[recovery] snapshot expand incomplete resource=/{} status={} cleaned_temp={} cleanup_status={} -> cold",
                resource_rel, entry.status, cleaned_temp, cleanup_status
            );
            if let Some(detail) = cleanup_detail.as_deref() {
                eprintln!(
                    "[recovery] snapshot expand cleanup detail resource=/{} detail={}",
                    resource_rel, detail
                );
            }

            self.emit_event(
                "/_snapshot/refresh.act",
                &format!("req-rec-{}", Uuid::new_v4().simple()),
                "cache.recovery",
                Some(json!({
                    "path": format!("/{}", resource_rel),
                    "status_before": entry.status,
                    "cleaned_temp": cleaned_temp,
                    "cleanup_status": cleanup_status,
                    "cleanup_detail": cleanup_detail,
                    "temp_artifact": entry.temp_artifact,
                    "phase": "recovered",
                })),
                None,
                None,
            )?;
        }

        self.snapshot_expand_journal.clear();
        self.save_snapshot_expand_journal()
    }
}
