use agentfs_sdk::{ConnectorContextV2, FetchLivePageRequestV2};
use anyhow::Result;
use chrono::Utc;
use serde_json::Value as JsonValue;
use std::fs;

use super::errors::{
    is_transient_connector_failure, ERR_INVALID_ARGUMENT, ERR_PAGER_HANDLE_CLOSED,
    ERR_PAGER_HANDLE_EXPIRED, ERR_PAGER_HANDLE_NOT_FOUND, ERR_PERMISSION_DENIED,
};
use super::shared::{
    collect_files_with_suffix, is_handle_format_valid, normalize_runtime_handle_id,
    parse_rfc3339_timestamp,
};
use super::{AppfsAdapter, PagingHandle, ProcessOutcome};

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
            self.emit_failed(
                action_path,
                request_id,
                ERR_PAGER_HANDLE_EXPIRED,
                "handle expired",
                client_token,
            )?;
            return Ok(ProcessOutcome::Consumed);
        }

        let (upstream_cursor, resource_path) = {
            let handle = self
                .handles
                .get(&handle_key)
                .expect("paging handle should exist after precheck");
            (handle.upstream_cursor.clone(), handle.resource_path.clone())
        };
        let request_ctx = ConnectorContextV2 {
            app_id: self.app_id.clone(),
            session_id: self.session_id.clone(),
            request_id: request_id.to_string(),
            client_token: client_token.clone(),
            trace_id: None,
        };
        match self.connector.fetch_live_page(
            FetchLivePageRequestV2 {
                resource_path,
                handle_id: Some(handle_key.clone()),
                cursor: upstream_cursor,
                page_size: 50,
            },
            &request_ctx,
        ) {
            Ok(response) => {
                if let Some(handle) = self.handles.get_mut(&handle_key) {
                    handle.page_no = response.page.page_no;
                    handle.upstream_cursor = response.page.next_cursor.clone();
                    handle.expires_at_ts = response
                        .page
                        .expires_at
                        .as_deref()
                        .and_then(parse_rfc3339_timestamp)
                        .filter(|expiry| *expiry > Utc::now().timestamp());
                }
                let content = serde_json::json!({
                    "items": response.items,
                    "page": {
                        "handle_id": response.page.handle_id,
                        "page_no": response.page.page_no,
                        "has_more": response.page.has_more,
                        "mode": "live",
                        "expires_at": response.page.expires_at,
                    }
                });
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
            Err(err) => {
                let (code, message, retryable) = if err.code.eq("CURSOR_INVALID") {
                    (
                        ERR_PAGER_HANDLE_NOT_FOUND.to_string(),
                        "handle cursor invalid".to_string(),
                        false,
                    )
                } else if err.code.eq("CURSOR_EXPIRED") {
                    (
                        ERR_PAGER_HANDLE_EXPIRED.to_string(),
                        "handle cursor expired".to_string(),
                        false,
                    )
                } else {
                    (err.code, err.message, err.retryable)
                };
                if is_transient_connector_failure(&code, retryable) {
                    eprintln!(
                        "AppFS adapter transient connector paging failure for {action_path}: code={code} message={message}; will retry without advancing cursor"
                    );
                    return Ok(ProcessOutcome::RetryPending);
                }
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
            if let Some(handle) = self.handles.get_mut(&handle_key) {
                // Keep explicit close semantics deterministic for legacy callers:
                // after close is requested, subsequent fetches should observe CLOSED.
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

        if let Some(handle) = self.handles.get_mut(&handle_key) {
            handle.closed = true;
        }
        self.emit_event(
            action_path,
            request_id,
            "action.completed",
            Some(serde_json::json!({
                "closed": true,
                "handle_id": handle_key,
            })),
            None,
            client_token,
        )?;
        Ok(ProcessOutcome::Consumed)
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
                        upstream_cursor: json
                            .get("page")
                            .and_then(|p| p.get("next_cursor"))
                            .and_then(|v| v.as_str())
                            .map(|v| v.to_string()),
                        resource_path: format!(
                            "/{}",
                            path.strip_prefix(&self.app_dir)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .replace('\\', "/")
                        ),
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
