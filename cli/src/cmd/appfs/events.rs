use super::*;
use std::fs::OpenOptions;
use std::io::Write;

impl AppfsAdapter {
    pub(super) fn enqueue_streaming_job(
        &mut self,
        action_path: &str,
        request_id: &str,
        client_token: Option<String>,
        plan: AdapterStreamingPlanV1,
    ) -> Result<()> {
        self.streaming_jobs.push(StreamingJob {
            request_id: request_id.to_string(),
            path: action_path.to_string(),
            client_token,
            accepted: plan.accepted_content,
            progress: plan.progress_content,
            terminal: plan.terminal_content,
            stage: 0,
        });
        self.save_streaming_jobs()
    }

    pub(super) fn drain_streaming_jobs(&mut self) -> Result<()> {
        if self.streaming_jobs.is_empty() {
            return Ok(());
        }

        let jobs = std::mem::take(&mut self.streaming_jobs);
        let mut next_jobs = Vec::with_capacity(jobs.len());
        for mut job in jobs {
            match job.stage {
                0 => {
                    self.emit_event(
                        &job.path,
                        &job.request_id,
                        "action.accepted",
                        Some(job.accepted.clone().unwrap_or_else(|| json!("accepted"))),
                        None,
                        job.client_token.clone(),
                    )?;
                    job.stage = 1;
                    next_jobs.push(job);
                }
                1 => {
                    self.emit_event(
                        &job.path,
                        &job.request_id,
                        "action.progress",
                        Some(
                            job.progress
                                .clone()
                                .unwrap_or_else(|| json!({ "percent": 50 })),
                        ),
                        None,
                        job.client_token.clone(),
                    )?;
                    job.stage = 2;
                    next_jobs.push(job);
                }
                _ => {
                    self.emit_event(
                        &job.path,
                        &job.request_id,
                        "action.completed",
                        Some(job.terminal),
                        None,
                        job.client_token,
                    )?;
                }
            }
        }
        self.streaming_jobs = next_jobs;
        self.save_streaming_jobs()
    }

    pub(super) fn emit_failed(
        &mut self,
        action_path: &str,
        request_id: &str,
        error_code: &str,
        message: &str,
        client_token: Option<String>,
    ) -> Result<()> {
        let retryable = error_code == ERR_PAGER_HANDLE_EXPIRED;
        self.emit_failed_with_retryable(
            action_path,
            request_id,
            error_code,
            message,
            retryable,
            client_token,
        )
    }

    pub(super) fn emit_failed_with_retryable(
        &mut self,
        action_path: &str,
        request_id: &str,
        error_code: &str,
        message: &str,
        retryable: bool,
        client_token: Option<String>,
    ) -> Result<()> {
        self.emit_event(
            action_path,
            request_id,
            "action.failed",
            None,
            Some(json!({
                "code": error_code,
                "message": message,
                "retryable": retryable,
            })),
            client_token,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn emit_snapshot_too_large(
        &mut self,
        action_path: &str,
        request_id: &str,
        resource_path: &str,
        size_bytes: usize,
        max_size: usize,
        phase: &str,
        client_token: Option<String>,
    ) -> Result<()> {
        self.emit_event(
            action_path,
            request_id,
            "action.failed",
            None,
            Some(json!({
                "code": ERR_SNAPSHOT_TOO_LARGE,
                "message": format!(
                    "snapshot resource exceeds max_materialized_bytes: bytes={size_bytes} max={max_size}"
                ),
                "retryable": false,
                "resource_path": resource_path,
                "phase": phase,
                "size": size_bytes,
                "max_size": max_size,
            })),
            client_token,
        )
    }

    pub(super) fn emit_event(
        &mut self,
        action_path: &str,
        request_id: &str,
        event_type: &str,
        content: Option<JsonValue>,
        error: Option<JsonValue>,
        client_token: Option<String>,
    ) -> Result<()> {
        let seq = self.next_seq;
        self.next_seq += 1;

        let mut event = json!({
            "seq": seq,
            "event_id": format!("evt-{}", seq),
            "ts": Utc::now().to_rfc3339(),
            "app": self.app_id,
            "session_id": self.session_id,
            "request_id": request_id,
            "path": action_path,
            "type": event_type,
        });

        if let Some(content) = content {
            event["content"] = content;
        }
        if let Some(error) = error {
            event["error"] = error;
        }
        if let Some(token) = client_token {
            event["client_token"] = json!(token);
        }

        let line = serde_json::to_string(&event)?;
        self.publish_event(seq, &line)
    }

    fn publish_event(&mut self, seq: i64, line: &str) -> Result<()> {
        let mut events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.events_path)
            .with_context(|| {
                format!("Failed to open stream file {}", self.events_path.display())
            })?;
        writeln!(events, "{line}")?;
        events.flush()?;

        let replay_file = self.replay_dir.join(format!("{seq}.evt.jsonl"));
        fs::write(&replay_file, format!("{line}\n"))
            .with_context(|| format!("Failed to write replay file {}", replay_file.display()))?;

        self.cursor.max_seq = seq;
        if self.cursor.min_seq <= 0 {
            self.cursor.min_seq = seq;
        }
        if self.cursor.retention_hint_sec <= 0 {
            self.cursor.retention_hint_sec = DEFAULT_RETENTION_HINT_SEC;
        }
        self.save_cursor()?;
        Ok(())
    }
}
