use super::*;

impl AppfsAdapter {
    pub(super) fn handle_fetch_next(
        &mut self,
        action_path: &str,
        request_id: &str,
        handle_id: &str,
        requester_session_id: Option<&str>,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        if !is_handle_format_valid(handle_id) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "invalid handle_id format",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let handle_key = self.resolve_handle_key(handle_id);
        let (owner_session, expires_at_ts, closed) = match self.handles.get(&handle_key) {
            Some(h) => (h.owner_session.clone(), h.expires_at_ts, h.closed),
            None => {
                self.emit_failed(
                    action_path,
                    request_id,
                    ERR_PAGER_HANDLE_NOT_FOUND,
                    "handle not found",
                    client_token,
                )?;
                return Ok(ProcessOutcome::Consumed);
            }
        };

        let effective_session = requester_session_id.unwrap_or(self.session_id.as_str());
        if effective_session != owner_session {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PERMISSION_DENIED,
                "cross-session handle access denied",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if closed {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_CLOSED,
                "handle already closed",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if expires_at_ts.is_some_and(|expiry| Utc::now().timestamp() >= expiry) {
            if let Some(handle) = self.handles.get_mut(&handle_key) {
                // Tombstone expired handles on explicit close requests so any
                // subsequent fetch observes deterministic CLOSED semantics.
                handle.closed = true;
            }
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_EXPIRED,
                "handle expired",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let current_page_no = self
            .handles
            .get(&handle_key)
            .expect("paging handle should exist after precheck")
            .page_no;
        let page_no = current_page_no + 1;
        let has_more = page_no < 3;
        let request_ctx = RequestContextV1 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: client_token.clone(),
        };
        match self.business_adapter.submit_control_action(
            action_path,
            AdapterControlActionV1::PagingFetchNext {
                handle_id: handle_key.clone(),
                page_no,
                has_more,
            },
            &request_ctx,
        ) {
            Ok(AdapterControlOutcomeV1::Completed { content }) => {
                if let Some(handle) = self.handles.get_mut(&handle_key) {
                    handle.page_no = page_no;
                }
                self.emit_event(
                    action_path,
                    request_id,
                    "action.completed",
                    Some(content),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            }) => {
                self.emit_failed_with_retryable(
                    action_path,
                    request_id,
                    &code,
                    &message,
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Internal { message }) => {
                eprintln!(
                    "AppFS adapter transient bridge failure for {action_path}: {message}; will retry without advancing cursor"
                );
                Ok(ProcessOutcome::RetryPending)
            }
        }
    }

    pub(super) fn handle_close_handle(
        &mut self,
        action_path: &str,
        request_id: &str,
        handle_id: &str,
        requester_session_id: Option<&str>,
        client_token: Option<String>,
    ) -> Result<ProcessOutcome> {
        if !is_handle_format_valid(handle_id) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_INVALID_ARGUMENT,
                "invalid handle_id format",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let handle_key = self.resolve_handle_key(handle_id);
        let (owner_session, expires_at_ts, closed) = match self.handles.get(&handle_key) {
            Some(h) => (h.owner_session.clone(), h.expires_at_ts, h.closed),
            None => {
                self.emit_failed(
                    action_path,
                    request_id,
                    ERR_PAGER_HANDLE_NOT_FOUND,
                    "handle not found",
                    client_token,
                )?;
                return Ok(ProcessOutcome::Consumed);
            }
        };

        let effective_session = requester_session_id.unwrap_or(self.session_id.as_str());
        if effective_session != owner_session {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PERMISSION_DENIED,
                "cross-session handle access denied",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if closed {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_CLOSED,
                "handle already closed",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        if expires_at_ts.is_some_and(|expiry| Utc::now().timestamp() >= expiry) {
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_EXPIRED,
                "handle expired",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let request_ctx = RequestContextV1 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: client_token.clone(),
        };
        match self.business_adapter.submit_control_action(
            action_path,
            AdapterControlActionV1::PagingClose {
                handle_id: handle_key.clone(),
            },
            &request_ctx,
        ) {
            Ok(AdapterControlOutcomeV1::Completed { content }) => {
                if let Some(handle) = self.handles.get_mut(&handle_key) {
                    handle.closed = true;
                }
                self.emit_event(
                    action_path,
                    request_id,
                    "action.completed",
                    Some(content),
                    None,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            }) => {
                self.emit_failed_with_retryable(
                    action_path,
                    request_id,
                    &code,
                    &message,
                    retryable,
                    client_token,
                )?;
                Ok(ProcessOutcome::Consumed)
            }
            Err(AdapterErrorV1::Internal { message }) => {
                eprintln!(
                    "AppFS adapter transient bridge failure for {action_path}: {message}; will retry without advancing cursor"
                );
                Ok(ProcessOutcome::RetryPending)
            }
        }
    }

    pub(super) fn load_known_handles(&mut self) -> Result<()> {
        let mut resources = Vec::new();
        collect_files_with_suffix(&self.app_dir, ".res.json", &mut resources)?;
        for path in resources {
            if path.starts_with(self.app_dir.join("_stream")) {
                continue;
            }

            let content = match fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let json: JsonValue = match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if let Some(handle_id) = json
                .get("page")
                .and_then(|p| p.get("handle_id"))
                .and_then(|h| h.as_str())
            {
                let normalized_handle = normalize_runtime_handle_id(handle_id);
                if normalized_handle != handle_id {
                    self.handle_aliases
                        .insert(handle_id.to_string(), normalized_handle.clone());
                }
                self.handles.insert(
                    normalized_handle,
                    PagingHandle {
                        page_no: 0,
                        closed: false,
                        owner_session: json
                            .get("page")
                            .and_then(|p| p.get("session_id"))
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .unwrap_or(self.session_id.as_str())
                            .to_string(),
                        expires_at_ts: json
                            .get("page")
                            .and_then(|p| p.get("expires_at"))
                            .and_then(|v| v.as_str())
                            .and_then(parse_rfc3339_timestamp),
                    },
                );
            }
        }
        Ok(())
    }

    pub(super) fn resolve_handle_key(&self, requested: &str) -> String {
        if let Some(alias) = self.handle_aliases.get(requested) {
            return alias.clone();
        }
        let normalized = normalize_runtime_handle_id(requested);
        if self.handles.contains_key(&normalized) {
            return normalized;
        }
        requested.to_string()
    }
}
