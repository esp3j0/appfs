use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::AppfsAdapter;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(super) struct SnapshotExpandJournalDoc {
    #[serde(default)]
    pub(super) resources: HashMap<String, SnapshotExpandJournalEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SnapshotExpandJournalEntry {
    pub(super) resource_path: String,
    pub(super) status: String,
    pub(super) request_id: String,
    pub(super) started_at: String,
    pub(super) updated_at: String,
    #[serde(default)]
    pub(super) temp_artifact: Option<String>,
}

impl AppfsAdapter {
    pub(super) fn update_snapshot_expand_journal(
        &mut self,
        resource_rel: &str,
        status: &str,
        request_id: &str,
        temp_artifact: Option<String>,
    ) -> Result<()> {
        let now = Utc::now().to_rfc3339();
        let resource_path = format!("/{}", resource_rel);
        let entry = self
            .snapshot_expand_journal
            .entry(resource_rel.to_string())
            .or_insert_with(|| SnapshotExpandJournalEntry {
                resource_path: resource_path.clone(),
                status: status.to_string(),
                request_id: request_id.to_string(),
                started_at: now.clone(),
                updated_at: now.clone(),
                temp_artifact: temp_artifact.clone(),
            });

        entry.resource_path = resource_path;
        entry.status = status.to_string();
        entry.request_id = request_id.to_string();
        entry.updated_at = now;
        if entry.started_at.is_empty() {
            entry.started_at = Utc::now().to_rfc3339();
        }
        if temp_artifact.is_some() {
            entry.temp_artifact = temp_artifact;
        }
        self.save_snapshot_expand_journal()
    }

    pub(super) fn clear_snapshot_expand_journal_entry(&mut self, resource_rel: &str) -> Result<()> {
        if self.snapshot_expand_journal.remove(resource_rel).is_some() {
            self.save_snapshot_expand_journal()?;
        }
        Ok(())
    }

    pub(super) fn load_snapshot_expand_journal(
        path: &Path,
    ) -> Result<HashMap<String, SnapshotExpandJournalEntry>> {
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let doc: SnapshotExpandJournalDoc = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))?;
        Ok(doc.resources)
    }

    pub(super) fn save_snapshot_expand_journal(&self) -> Result<()> {
        let tmp_path = self
            .snapshot_expand_journal_path
            .with_extension("res.json.tmp");
        let doc = SnapshotExpandJournalDoc {
            resources: self.snapshot_expand_journal.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&doc)?;
        fs::write(&tmp_path, bytes).with_context(|| {
            format!(
                "Failed to write snapshot expand journal temp file {}",
                tmp_path.display()
            )
        })?;

        if self.snapshot_expand_journal_path.exists() {
            fs::remove_file(&self.snapshot_expand_journal_path).with_context(|| {
                format!(
                    "Failed to remove old snapshot expand journal file {}",
                    self.snapshot_expand_journal_path.display()
                )
            })?;
        }

        fs::rename(&tmp_path, &self.snapshot_expand_journal_path).with_context(|| {
            format!(
                "Failed to move snapshot expand journal temp file {} to {}",
                tmp_path.display(),
                self.snapshot_expand_journal_path.display()
            )
        })?;
        Ok(())
    }
}
