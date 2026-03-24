use agentfs_sdk::{
    connector_error_codes_v2, ConnectorContextV2, ConnectorErrorV2, FetchSnapshotChunkRequestV2,
    SnapshotResumeV2,
};
use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value as JsonValue};
use std::fs;
use std::io::{BufRead, ErrorKind};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::errors::{ERR_CACHE_MISS_EXPAND_FAILED, ERR_INVALID_ARGUMENT};
use super::shared::{
    decode_jsonl_line, deterministic_shorten_segment, is_safe_resource_rel_path,
    normalize_resource_rel_path, snapshot_coalesce_window_ms, snapshot_expand_delay_ms,
    snapshot_force_expand_on_refresh, snapshot_publish_delay_ms,
};
use super::{
    AppfsAdapter, ProcessOutcome, SnapshotCacheState, SnapshotOnTimeoutPolicy, SnapshotSpec,
};

impl AppfsAdapter {
    pub(super) fn initialize_snapshot_states(&mut self) {
        for spec in &self.snapshot_specs {
            let rel = spec.template.clone();
            let abs = self.app_dir.join(&rel);
            let state = match fs::metadata(abs) {
                Ok(_) => SnapshotCacheState::Hot,
                Err(_) => SnapshotCacheState::Cold,
            };
            self.snapshot_states.insert(rel, state);
        }
    }

    pub(super) fn startup_prewarm_snapshots(&mut self) {
        let mut prewarm_specs: Vec<SnapshotSpec> = self
            .snapshot_specs
            .iter()
            .filter(|spec| spec.prewarm)
            .cloned()
            .collect();
        prewarm_specs.sort_by(|lhs, rhs| lhs.template.cmp(&rhs.template));

        for spec in prewarm_specs {
            let resource_path = format!("/{}", spec.template);
            let timeout = Duration::from_millis(spec.prewarm_timeout_ms.max(1));
            let request_id = format!("prewarm-{}", uuid::Uuid::new_v4());
            let ctx = ConnectorContextV2 {
                app_id: self.app_id.clone(),
                session_id: self.session_id.clone(),
                request_id,
                client_token: None,
                trace_id: None,
            };
            match self
                .business_connector
                .prewarm_snapshot_meta(&resource_path, timeout, &ctx)
            {
                Ok(meta) => {
                    self.transition_snapshot_state(&spec.template, SnapshotCacheState::Hot);
                    eprintln!(
                        "[prewarm] resource={resource_path} state=hot size_bytes={} revision={}",
                        meta.size_bytes
                            .map(|size| size.to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        meta.revision.unwrap_or_else(|| "unknown".to_string())
                    );
                }
                Err(err) if is_prewarm_timeout(&err) => {
                    self.transition_snapshot_state(&spec.template, SnapshotCacheState::Cold);
                    eprintln!(
                        "[prewarm] timeout resource={resource_path} state=cold timeout_ms={} err={}",
                        spec.prewarm_timeout_ms,
                        prewarm_error_summary(&err)
                    );
                }
                Err(err) => {
                    self.transition_snapshot_state(&spec.template, SnapshotCacheState::Cold);
                    eprintln!(
                        "[prewarm] failed resource={resource_path} state=cold err={}",
                        prewarm_error_summary(&err)
                    );
                }
            }
        }
    }

    fn snapshot_state_for(&self, resource_rel: &str) -> SnapshotCacheState {
        self.snapshot_states
            .get(resource_rel)
            .copied()
            .unwrap_or(SnapshotCacheState::Cold)
    }

    pub(super) fn transition_snapshot_state(
        &mut self,
        resource_rel: &str,
        next: SnapshotCacheState,
    ) {
        let prev = self.snapshot_state_for(resource_rel);
        if prev != next {
            eprintln!(
                "[cache] state resource=/{} from={} to={}",
                resource_rel,
                prev.as_str(),
                next.as_str()
            );
        }
        self.snapshot_states.insert(resource_rel.to_string(), next);
    }

    fn mark_snapshot_recent_expand(&mut self, resource_rel: &str) {
        self.snapshot_recent_expands
            .insert(resource_rel.to_string(), Instant::now());
    }

    pub(super) fn clear_snapshot_recent_expand(&mut self, resource_rel: &str) {
        self.snapshot_recent_expands.remove(resource_rel);
    }

    fn is_coalesced_snapshot_hit(&mut self, resource_rel: &str) -> bool {
        let window = Duration::from_millis(snapshot_coalesce_window_ms().max(1));
        match self.snapshot_recent_expands.get(resource_rel).copied() {
            Some(expanded_at) if expanded_at.elapsed() <= window => true,
            Some(_) => {
                self.snapshot_recent_expands.remove(resource_rel);
                false
            }
            None => false,
        }
    }

    fn snapshot_expand_temp_rel_path(&self, resource_rel: &str) -> String {
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
        format!("/_stream/snapshot-expand-tmp/{}.pending.jsonl", sanitized)
    }

    fn snapshot_expand_temp_abs_path(&self, resource_rel: &str) -> PathBuf {
        self.app_dir.join(
            self.snapshot_expand_temp_rel_path(resource_rel)
                .trim_start_matches('/'),
        )
    }

    fn validate_stale_snapshot_jsonl(
        &self,
        stale_abs: &Path,
    ) -> std::result::Result<usize, String> {
        let file = fs::File::open(stale_abs).map_err(|err| format!("open_failed: {err}"))?;
        let mut reader = std::io::BufReader::new(file);
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

    pub(super) fn handle_snapshot_refresh(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_path: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let Some(resource_rel) = normalize_resource_rel_path(resource_path) else {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "resource_path must be a non-empty .res.jsonl path",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        };
        if !is_safe_resource_rel_path(&resource_rel) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "unsafe snapshot resource_path",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let Some(snapshot_spec) = self.find_snapshot_spec(&resource_rel).cloned() else {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "snapshot resource is not declared in manifest",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        };

        let resource_abs = self.app_dir.join(&resource_rel);
        let force_expand = snapshot_force_expand_on_refresh();
        let (size_bytes, coalesced) = match fs::metadata(&resource_abs) {
            Ok(meta) => {
                let size_bytes = meta.len() as usize;
                if force_expand {
                    eprintln!(
                        "[cache] refresh forcing expand resource=/{} existing_bytes={size_bytes}",
                        resource_rel
                    );
                    self.clear_snapshot_recent_expand(&resource_rel);
                    return self.expand_snapshot_cache_read_through(
                        action_path,
                        request_id,
                        &resource_rel,
                        &snapshot_spec,
                        "forced_refresh",
                        client_token,
                    );
                }
                let coalesced = self.is_coalesced_snapshot_hit(&resource_rel);
                self.transition_snapshot_state(&resource_rel, SnapshotCacheState::Hot);
                eprintln!("[cache] hit resource=/{} bytes={size_bytes}", resource_rel);
                if coalesced {
                    eprintln!(
                        "[cache] coalesced concurrent miss resource=/{}",
                        resource_rel
                    );
                }
                (size_bytes, coalesced)
            }
            Err(err) => {
                let reason = match err.kind() {
                    ErrorKind::NotFound => format!("resource_missing: {err}"),
                    _ => format!("resource_unreadable: {err}"),
                };
                self.clear_snapshot_recent_expand(&resource_rel);
                return self.expand_snapshot_cache_read_through(
                    action_path,
                    request_id,
                    &resource_rel,
                    &snapshot_spec,
                    &reason,
                    client_token,
                );
            }
        };

        if size_bytes > snapshot_spec.max_materialized_bytes {
            let resource_path = format!("/{}", resource_rel);
            eprintln!(
                "[cache] snapshot_too_large resource={} size={} max={}",
                resource_path, size_bytes, snapshot_spec.max_materialized_bytes
            );
            self.emit_snapshot_too_large(
                action_path,
                request_id,
                &resource_path,
                size_bytes,
                snapshot_spec.max_materialized_bytes,
                "existing_cache",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        self.emit_event(
            action_path,
            request_id,
            "action.completed",
            Some(json!({
                "refreshed": true,
                "resource_path": format!("/{}", resource_rel),
                "bytes": size_bytes,
                "max_materialized_bytes": snapshot_spec.max_materialized_bytes,
                "cached": true,
                "coalesced": coalesced,
                "state": self.snapshot_state_for(&resource_rel).as_str(),
                "generated_at": Utc::now().to_rfc3339(),
            })),
            None,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
    }

    fn expand_snapshot_cache_read_through(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_rel: &str,
        snapshot_spec: &SnapshotSpec,
        reason: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let resource_path = format!("/{}", resource_rel);
        self.clear_snapshot_recent_expand(resource_rel);
        self.transition_snapshot_state(resource_rel, SnapshotCacheState::Cold);
        self.transition_snapshot_state(resource_rel, SnapshotCacheState::Warming);
        let temp_artifact = self.snapshot_expand_temp_rel_path(resource_rel);
        let temp_artifact_abs = self.snapshot_expand_temp_abs_path(resource_rel);
        self.update_snapshot_expand_journal(
            resource_rel,
            "warming",
            request_id,
            Some(temp_artifact.clone()),
        )?;

        eprintln!(
            "[cache] miss, expanding resource={} phase=read_through reason={} timeout_ms={} on_timeout={}",
            resource_path,
            reason,
            snapshot_spec.read_through_timeout_ms,
            snapshot_spec.on_timeout.as_str()
        );

        let started_at = Instant::now();
        let simulated_delay_ms = snapshot_expand_delay_ms();
        if simulated_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(simulated_delay_ms));
        }

        if simulated_delay_ms > snapshot_spec.read_through_timeout_ms {
            let mut stale_unavailable_reason: Option<&'static str> = None;
            let mut stale_unavailable_detail: Option<String> = None;
            if matches!(
                snapshot_spec.on_timeout,
                SnapshotOnTimeoutPolicy::ReturnStale
            ) {
                let stale_abs = self.app_dir.join(resource_rel);
                match fs::metadata(&stale_abs) {
                    Ok(meta) => {
                        let stale_bytes = meta.len() as usize;
                        if stale_bytes <= snapshot_spec.max_materialized_bytes {
                            match self.validate_stale_snapshot_jsonl(&stale_abs) {
                                Ok(valid_lines) => {
                                    self.transition_snapshot_state(
                                        resource_rel,
                                        SnapshotCacheState::Hot,
                                    );
                                    self.clear_snapshot_recent_expand(resource_rel);
                                    self.clear_snapshot_expand_journal_entry(resource_rel)?;
                                    eprintln!(
                                        "[cache] timeout_return_stale resource={} bytes={} timeout_ms={} elapsed_ms={} health=valid valid_lines={}",
                                        resource_path,
                                        stale_bytes,
                                        snapshot_spec.read_through_timeout_ms,
                                        simulated_delay_ms,
                                        valid_lines
                                    );
                                    self.emit_event(
                                        &resource_path,
                                        request_id,
                                        "cache.expand",
                                        Some(json!({
                                            "path": resource_path.clone(),
                                            "phase": "timeout",
                                            "state": self.snapshot_state_for(resource_rel).as_str(),
                                            "failure_reason": "timeout",
                                            "timeout_ms": snapshot_spec.read_through_timeout_ms,
                                            "elapsed_ms": simulated_delay_ms,
                                            "on_timeout": snapshot_spec.on_timeout.as_str(),
                                            "fallback": "return_stale",
                                            "stale_health": "valid",
                                            "stale_valid_lines": valid_lines,
                                        })),
                                        None,
                                        client_token.clone(),
                                    )?;
                                    self.emit_event(
                                        &resource_path,
                                        request_id,
                                        "cache.stale",
                                        Some(json!({
                                            "path": resource_path.clone(),
                                            "reason": "timeout",
                                            "bytes": stale_bytes,
                                            "max_size": snapshot_spec.max_materialized_bytes,
                                            "state": self.snapshot_state_for(resource_rel).as_str(),
                                            "stale_health": "valid",
                                            "stale_valid_lines": valid_lines,
                                        })),
                                        None,
                                        client_token.clone(),
                                    )?;
                                    self.emit_event(
                                        action_path,
                                        request_id,
                                        "action.completed",
                                        Some(json!({
                                            "refreshed": false,
                                            "resource_path": resource_path,
                                            "bytes": stale_bytes,
                                            "max_materialized_bytes": snapshot_spec.max_materialized_bytes,
                                            "cached": true,
                                            "coalesced": false,
                                            "stale": true,
                                            "degrade_reason": "timeout_return_stale",
                                            "stale_health": "valid",
                                            "stale_valid_lines": valid_lines,
                                            "state": self.snapshot_state_for(resource_rel).as_str(),
                                            "generated_at": Utc::now().to_rfc3339(),
                                        })),
                                        None,
                                        client_token,
                                    )?;
                                    return Ok(ProcessOutcome::Consumed);
                                }
                                Err(health_reason) => {
                                    stale_unavailable_reason = Some("stale_cache_unhealthy");
                                    stale_unavailable_detail = Some(health_reason.clone());
                                    eprintln!(
                                        "[cache] timeout_return_stale unavailable resource={} reason=stale_cache_unhealthy detail={}",
                                        resource_path, health_reason
                                    );
                                }
                            }
                        } else {
                            stale_unavailable_reason = Some("stale_cache_too_large");
                            stale_unavailable_detail = Some(format!(
                                "size={} max={}",
                                stale_bytes, snapshot_spec.max_materialized_bytes
                            ));
                            eprintln!(
                                "[cache] timeout_return_stale unavailable resource={} reason=stale_cache_too_large size={} max={}",
                                resource_path, stale_bytes, snapshot_spec.max_materialized_bytes
                            );
                        }
                    }
                    Err(_) => {
                        stale_unavailable_reason = Some("no_stale_cache");
                        eprintln!(
                            "[cache] timeout_return_stale unavailable resource={} reason=no_stale_cache",
                            resource_path
                        );
                    }
                }
            }
            self.transition_snapshot_state(resource_rel, SnapshotCacheState::Error);
            self.clear_snapshot_recent_expand(resource_rel);
            self.emit_event(
                &resource_path,
                request_id,
                "cache.expand",
                Some(json!({
                    "path": resource_path,
                    "phase": "failed",
                    "state": self.snapshot_state_for(resource_rel).as_str(),
                    "failure_reason": "timeout",
                    "timeout_ms": snapshot_spec.read_through_timeout_ms,
                    "on_timeout": snapshot_spec.on_timeout.as_str(),
                    "stale_reason": stale_unavailable_reason,
                    "stale_detail": stale_unavailable_detail,
                })),
                None,
                client_token.clone(),
            )?;
            let timeout_reason = if let Some(reason) = stale_unavailable_reason {
                let detail = stale_unavailable_detail.as_deref().unwrap_or("none");
                format!(
                    "expand_timeout elapsed_ms={} timeout_ms={} on_timeout={} stale_reason={} stale_detail={}",
                    simulated_delay_ms,
                    snapshot_spec.read_through_timeout_ms,
                    snapshot_spec.on_timeout.as_str(),
                    reason,
                    detail
                )
            } else {
                format!(
                    "expand_timeout elapsed_ms={} timeout_ms={} on_timeout={}",
                    simulated_delay_ms,
                    snapshot_spec.read_through_timeout_ms,
                    snapshot_spec.on_timeout.as_str()
                )
            };
            self.clear_snapshot_expand_journal_entry(resource_rel)?;
            return self.handle_snapshot_cache_expand_hook(
                action_path,
                request_id,
                resource_rel,
                "timeout",
                &timeout_reason,
                client_token,
            );
        }

        let expanded_jsonl = match self.fetch_snapshot_jsonl_from_upstream(resource_rel, request_id) {
            Ok(content) => content,
            Err(upstream_reason) => {
                self.transition_snapshot_state(resource_rel, SnapshotCacheState::Error);
                self.clear_snapshot_recent_expand(resource_rel);
                self.emit_event(
                    &resource_path,
                    request_id,
                    "cache.expand",
                    Some(json!({
                        "path": resource_path,
                        "phase": "failed",
                        "state": self.snapshot_state_for(resource_rel).as_str(),
                        "failure_reason": upstream_reason,
                    })),
                    None,
                    client_token.clone(),
                )?;
                self.clear_snapshot_expand_journal_entry(resource_rel)?;
                return self.handle_snapshot_cache_expand_hook(
                    action_path,
                    request_id,
                    resource_rel,
                    "expand_hook",
                    "upstream_connector_unavailable",
                    client_token,
                );
            }
        };

        let size_bytes = expanded_jsonl.len();
        if size_bytes > snapshot_spec.max_materialized_bytes {
            self.transition_snapshot_state(resource_rel, SnapshotCacheState::Error);
            self.clear_snapshot_recent_expand(resource_rel);
            eprintln!(
                "[cache] snapshot_too_large resource={} size={} max={}",
                resource_path, size_bytes, snapshot_spec.max_materialized_bytes
            );
            self.emit_event(
                &resource_path,
                request_id,
                "cache.expand",
                Some(json!({
                    "path": resource_path,
                    "phase": "failed",
                    "state": self.snapshot_state_for(resource_rel).as_str(),
                    "failure_reason": "snapshot_too_large",
                    "size": size_bytes,
                    "max_size": snapshot_spec.max_materialized_bytes,
                })),
                None,
                client_token.clone(),
            )?;
            self.emit_snapshot_too_large(
                action_path,
                request_id,
                &resource_path,
                size_bytes,
                snapshot_spec.max_materialized_bytes,
                "expand_publish",
                client_token,
            )?;
            self.clear_snapshot_expand_journal_entry(resource_rel)?;
            return Ok(ProcessOutcome::Consumed);
        }

        self.materialize_snapshot_file(
            resource_rel,
            &expanded_jsonl,
            &temp_artifact_abs,
            request_id,
        )?;
        self.transition_snapshot_state(resource_rel, SnapshotCacheState::Hot);
        self.mark_snapshot_recent_expand(resource_rel);
        self.clear_snapshot_expand_journal_entry(resource_rel)?;
        let elapsed_ms = started_at.elapsed().as_millis() as u64;

        eprintln!(
            "[cache] expanded resource={} bytes={} state=hot elapsed_ms={elapsed_ms}",
            resource_path, size_bytes
        );

        self.emit_event(
            &resource_path,
            request_id,
            "cache.expand",
            Some(json!({
                "path": resource_path,
                "phase": "completed",
                "state": self.snapshot_state_for(resource_rel).as_str(),
                "bytes": size_bytes,
                "elapsed_ms": elapsed_ms,
                "upstream_calls": 1,
            })),
            None,
            client_token.clone(),
        )?;

        self.emit_event(
            action_path,
            request_id,
            "action.completed",
            Some(json!({
                "refreshed": true,
                "resource_path": resource_path,
                "bytes": size_bytes,
                "max_materialized_bytes": snapshot_spec.max_materialized_bytes,
                "cached": false,
                "coalesced": false,
                "state": self.snapshot_state_for(resource_rel).as_str(),
                "generated_at": Utc::now().to_rfc3339(),
            })),
            None,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
    }

    fn handle_snapshot_cache_expand_hook(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_rel: &str,
        phase: &str,
        reason: &str,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        let resource_path = format!("/{}", resource_rel);
        eprintln!(
            "[cache] miss, expanding resource={} phase={} reason={}",
            resource_path, phase, reason
        );
        eprintln!(
            "[cache] expand failed resource={} phase={} reason={}",
            resource_path, phase, reason
        );
        self.emit_failed_with_retryable(
            action_path,
            request_id,
            ERR_CACHE_MISS_EXPAND_FAILED,
            &format!(
                "snapshot read-through expansion failed: resource={} phase={} reason={}",
                resource_path, phase, reason
            ),
            true,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
    }

    fn fetch_snapshot_jsonl_from_upstream(
        &mut self,
        resource_rel: &str,
        request_id: &str,
    ) -> std::result::Result<String, String> {
        eprintln!("[cache.expand] fetch_snapshot_chunk resource=/{}", resource_rel);
        let mut out = String::new();
        let mut resume = SnapshotResumeV2::Start;
        let mut total_bytes = 0usize;
        let budget_bytes = 1_048_576_u64;

        loop {
            let ctx = ConnectorContextV2 {
                app_id: self.app_id.clone(),
                session_id: self.session_id.clone(),
                request_id: request_id.to_string(),
                client_token: None,
                trace_id: None,
            };
            let response = self
                .business_connector
                .fetch_snapshot_chunk(
                    FetchSnapshotChunkRequestV2 {
                        resource_path: format!("/{}", resource_rel),
                        resume,
                        budget_bytes,
                    },
                    &ctx,
                )
                .map_err(snapshot_expand_error_summary)?;

            for record in response.records {
                let line = serde_json::to_string(&record.line)
                    .map_err(|err| format!("invalid_snapshot_record line_serialize_failed: {err}"))?;
                out.push_str(&line);
                out.push('\n');
                total_bytes += line.len() + 1;
            }

            if !response.has_more {
                break;
            }

            let Some(next_cursor) = response.next_cursor else {
                return Err("connector_response_invalid missing_next_cursor_when_has_more".to_string());
            };
            resume = SnapshotResumeV2::Cursor(next_cursor);
        }

        if total_bytes == 0 {
            return Err("connector_response_invalid empty_snapshot_chunk_stream".to_string());
        }

        Ok(out)
    }

    fn materialize_snapshot_file(
        &mut self,
        resource_rel: &str,
        content: &str,
        temp_path: &Path,
        request_id: &str,
    ) -> Result<()> {
        let abs = self.app_dir.join(resource_rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create snapshot parent directory {}",
                    parent.display()
                )
            })?;
        }

        if let Some(parent) = temp_path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "Failed to create snapshot temp parent directory {}",
                    parent.display()
                )
            })?;
        }

        fs::write(temp_path, content).with_context(|| {
            format!(
                "Failed to write snapshot expansion temp file {}",
                temp_path.display()
            )
        })?;
        let temp_rel = format!(
            "/{}",
            temp_path
                .strip_prefix(&self.app_dir)
                .unwrap_or(temp_path)
                .to_string_lossy()
                .replace('\\', "/")
        );
        self.update_snapshot_expand_journal(
            resource_rel,
            "publishing",
            request_id,
            Some(temp_rel),
        )?;

        let publish_delay_ms = snapshot_publish_delay_ms();
        if publish_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(publish_delay_ms));
        }

        if abs.exists() {
            fs::remove_file(&abs).with_context(|| {
                format!("Failed to remove stale snapshot file {}", abs.display())
            })?;
        }
        fs::rename(temp_path, &abs).with_context(|| {
            format!(
                "Failed to publish snapshot expansion from {} to {}",
                temp_path.display(),
                abs.display()
            )
        })?;
        Ok(())
    }
}

fn is_prewarm_timeout(err: &ConnectorErrorV2) -> bool {
    err.code.eq_ignore_ascii_case(connector_error_codes_v2::TIMEOUT)
        || err
            .message
            .split_whitespace()
            .any(|segment| segment.eq_ignore_ascii_case("timeout"))
}

fn prewarm_error_summary(err: &ConnectorErrorV2) -> String {
    format!(
        "code={} retryable={} message={} details={}",
        err.code,
        err.retryable,
        err.message,
        err.details.as_deref().unwrap_or("none")
    )
}

fn snapshot_expand_error_summary(err: ConnectorErrorV2) -> String {
    format!(
        "connector_error code={} retryable={} message={} details={}",
        err.code,
        err.retryable,
        err.message,
        err.details.as_deref().unwrap_or("none")
    )
}
