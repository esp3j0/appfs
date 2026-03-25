use anyhow::{Context, Result};
use chrono::Utc;
use serde_json::{json, Value as JsonValue};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use super::action_dispatcher::{
    normalize_actionline_v2_payload, parse_list_apps_request, parse_register_app_request,
    parse_unregister_app_request, RegisterAppRequest, UnregisterAppRequest,
};
use super::errors::{ERR_INVALID_ARGUMENT, ERR_INVALID_PAYLOAD};
use super::shared::{decode_jsonl_line, extract_client_token};
use super::{ActionCursorDoc, ActionCursorState, CursorState, DEFAULT_RETENTION_HINT_SEC};

const CONTROL_APP_ID: &str = "_appfs";
const CONTROL_SESSION_ID: &str = "runtime-control";
const CONTROL_REGISTER_ACTION: &str = "register_app.act";
const CONTROL_UNREGISTER_ACTION: &str = "unregister_app.act";
const CONTROL_LIST_ACTION: &str = "list_apps.act";

#[derive(Debug, Clone)]
pub(super) enum SupervisorControlInvocation {
    Register {
        request_id: String,
        client_token: Option<String>,
        request: RegisterAppRequest,
    },
    Unregister {
        request_id: String,
        client_token: Option<String>,
        request: UnregisterAppRequest,
    },
    List {
        request_id: String,
        client_token: Option<String>,
    },
}

pub(super) struct SupervisorControlPlane {
    root: PathBuf,
    events_path: PathBuf,
    cursor_path: PathBuf,
    replay_dir: PathBuf,
    action_cursors_path: PathBuf,
    cursor: CursorState,
    action_cursors: HashMap<String, ActionCursorState>,
    next_seq: i64,
    actionline_v2_strict: bool,
}

impl SupervisorControlPlane {
    pub(super) fn new(root: PathBuf, actionline_v2_strict: bool) -> Result<Self> {
        let control_dir = root.join(CONTROL_APP_ID);
        let stream_dir = control_dir.join("_stream");
        let cursor_path = stream_dir.join("cursor.res.json");
        let replay_dir = stream_dir.join("from-seq");
        let action_cursors_path = stream_dir.join(super::ACTION_CURSORS_FILENAME);
        let cursor = load_cursor_or_default(&cursor_path)?;
        let next_seq = cursor.max_seq + 1;
        Ok(Self {
            root,
            events_path: stream_dir.join("events.evt.jsonl"),
            cursor_path,
            replay_dir,
            action_cursors_path: action_cursors_path.clone(),
            cursor,
            action_cursors: load_action_cursors_or_default(&action_cursors_path)?,
            next_seq,
            actionline_v2_strict,
        })
    }

    pub(super) fn prepare_action_sinks(&mut self) -> Result<()> {
        let control_dir = self.root.join(CONTROL_APP_ID);
        let stream_dir = control_dir.join("_stream");
        fs::create_dir_all(&self.replay_dir).with_context(|| {
            format!(
                "Failed to create AppFS control replay dir {}",
                self.replay_dir.display()
            )
        })?;
        if !self.events_path.exists() {
            fs::write(&self.events_path, b"").with_context(|| {
                format!(
                    "Failed to initialize AppFS control stream {}",
                    self.events_path.display()
                )
            })?;
        }
        if !self.cursor_path.exists() {
            write_json_file(
                &self.cursor_path,
                &json!(CursorState {
                    min_seq: 0,
                    max_seq: 0,
                    retention_hint_sec: DEFAULT_RETENTION_HINT_SEC,
                }),
            )?;
        }
        if !self.action_cursors_path.exists() {
            write_json_file(
                &self.action_cursors_path,
                &serde_json::to_value(ActionCursorDoc::default())?,
            )?;
        }
        for action_name in [
            CONTROL_REGISTER_ACTION,
            CONTROL_UNREGISTER_ACTION,
            CONTROL_LIST_ACTION,
        ] {
            let action_path = control_dir.join(action_name);
            if !action_path.exists() {
                fs::write(&action_path, b"").with_context(|| {
                    format!(
                        "Failed to initialize AppFS control action {}",
                        action_path.display()
                    )
                })?;
            }
        }
        fs::create_dir_all(&stream_dir).with_context(|| {
            format!(
                "Failed to create AppFS control stream dir {}",
                stream_dir.display()
            )
        })?;
        Ok(())
    }

    pub(super) fn drain_invocations(&mut self) -> Result<Vec<SupervisorControlInvocation>> {
        let mut out = Vec::new();
        for action_name in [
            CONTROL_LIST_ACTION,
            CONTROL_REGISTER_ACTION,
            CONTROL_UNREGISTER_ACTION,
        ] {
            out.extend(self.drain_action_file(action_name)?);
        }
        Ok(out)
    }

    fn drain_action_file(&mut self, action_name: &str) -> Result<Vec<SupervisorControlInvocation>> {
        let action_path = self.root.join(CONTROL_APP_ID).join(action_name);
        if !action_path.exists() {
            return Ok(Vec::new());
        }

        let bytes = fs::read(&action_path)
            .with_context(|| format!("Failed to read control action {}", action_path.display()))?;
        let cursor_key = action_name.to_string();
        let mut cursor = self
            .action_cursors
            .get(&cursor_key)
            .cloned()
            .unwrap_or_default();
        let file_len = bytes.len() as u64;
        if cursor.offset > file_len {
            cursor.offset = file_len;
        }
        if cursor.offset == file_len {
            return Ok(Vec::new());
        }

        let start = cursor.offset as usize;
        let Some(last_newline_rel) = bytes[start..].iter().rposition(|byte| *byte == b'\n') else {
            return Ok(Vec::new());
        };
        let end = start + last_newline_rel + 1;
        let mut invocations = Vec::new();
        let mut line_start = start;

        while line_start < end {
            let line_end = bytes[line_start..end]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|idx| line_start + idx + 1)
                .expect("newline must exist");
            let line_bytes = &bytes[line_start..line_end];
            let request_id = new_request_id();

            match decode_jsonl_line(line_bytes, line_start == 0) {
                Ok(Some(line)) => match self.normalize_payload(&line) {
                    Ok((payload_json, client_token)) => {
                        let failure_token = client_token.clone();
                        match parse_invocation(
                            action_name,
                            &request_id,
                            client_token,
                            &payload_json,
                        ) {
                            Ok(invocation) => invocations.push(invocation),
                            Err(code) => {
                                self.emit_failed(
                                    control_action_path(action_name),
                                    &request_id,
                                    code,
                                    "invalid AppFS lifecycle control payload",
                                    failure_token.or_else(|| extract_client_token(&payload_json)),
                                )?;
                            }
                        }
                    }
                    Err((code, message, client_token)) => {
                        self.emit_failed(
                            control_action_path(action_name),
                            &request_id,
                            code,
                            message,
                            client_token,
                        )?;
                    }
                },
                Ok(None) => {}
                Err(message) => {
                    self.emit_failed(
                        control_action_path(action_name),
                        &request_id,
                        ERR_INVALID_PAYLOAD,
                        &message,
                        None,
                    )?;
                }
            }

            line_start = line_end;
        }

        cursor.offset = end as u64;
        cursor.boundary_probe = None;
        cursor.pending_multiline_eof_len = None;
        self.action_cursors.insert(cursor_key, cursor);
        self.save_action_cursors()?;

        Ok(invocations)
    }

    fn normalize_payload(
        &self,
        line: &str,
    ) -> std::result::Result<(String, Option<String>), (&'static str, &'static str, Option<String>)>
    {
        match normalize_actionline_v2_payload(line, self.actionline_v2_strict) {
            Ok(Some(parsed)) => Ok((parsed.payload_json, Some(parsed.client_token))),
            Ok(None) => Ok((line.to_string(), extract_client_token(line))),
            Err(err) => Err((err.code, err.reason, None)),
        }
    }

    pub(super) fn emit_completed(
        &mut self,
        action_path: &str,
        request_id: &str,
        content: JsonValue,
        client_token: Option<String>,
    ) -> Result<()> {
        self.emit_event(
            action_path,
            request_id,
            "action.completed",
            Some(content),
            None,
            client_token,
        )
    }

    pub(super) fn emit_failed(
        &mut self,
        action_path: &str,
        request_id: &str,
        error_code: &str,
        message: &str,
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
                "retryable": false,
            })),
            client_token,
        )
    }

    fn emit_event(
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
            "event_id": format!("evt-{seq}"),
            "ts": Utc::now().to_rfc3339(),
            "app": CONTROL_APP_ID,
            "session_id": CONTROL_SESSION_ID,
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
        if let Some(client_token) = client_token {
            event["client_token"] = json!(client_token);
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
                format!(
                    "Failed to open AppFS control stream {}",
                    self.events_path.display()
                )
            })?;
        writeln!(events, "{line}")?;
        events.flush()?;

        let replay_path = self.replay_dir.join(format!("{seq}.evt.jsonl"));
        fs::write(&replay_path, format!("{line}\n")).with_context(|| {
            format!(
                "Failed to write AppFS control replay file {}",
                replay_path.display()
            )
        })?;

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

    fn save_cursor(&self) -> Result<()> {
        write_json_file(&self.cursor_path, &serde_json::to_value(&self.cursor)?)
    }

    fn save_action_cursors(&self) -> Result<()> {
        write_json_file(
            &self.action_cursors_path,
            &serde_json::to_value(ActionCursorDoc {
                actions: self.action_cursors.clone(),
            })?,
        )
    }
}

fn parse_invocation(
    action_name: &str,
    request_id: &str,
    client_token: Option<String>,
    payload_json: &str,
) -> std::result::Result<SupervisorControlInvocation, &'static str> {
    match action_name {
        CONTROL_REGISTER_ACTION => Ok(SupervisorControlInvocation::Register {
            request_id: request_id.to_string(),
            client_token,
            request: parse_register_app_request(payload_json)?,
        }),
        CONTROL_UNREGISTER_ACTION => Ok(SupervisorControlInvocation::Unregister {
            request_id: request_id.to_string(),
            client_token,
            request: parse_unregister_app_request(payload_json)?,
        }),
        CONTROL_LIST_ACTION => {
            parse_list_apps_request(payload_json)?;
            Ok(SupervisorControlInvocation::List {
                request_id: request_id.to_string(),
                client_token,
            })
        }
        _ => Err(ERR_INVALID_ARGUMENT),
    }
}

fn control_action_path(action_name: &str) -> &'static str {
    match action_name {
        CONTROL_REGISTER_ACTION => "/_appfs/register_app.act",
        CONTROL_UNREGISTER_ACTION => "/_appfs/unregister_app.act",
        CONTROL_LIST_ACTION => "/_appfs/list_apps.act",
        _ => "/_appfs/unknown.act",
    }
}

fn new_request_id() -> String {
    let uuid = Uuid::new_v4().simple().to_string();
    format!("req-{}", &uuid[..8])
}

fn load_cursor_or_default(path: &Path) -> Result<CursorState> {
    if !path.exists() {
        return Ok(CursorState {
            min_seq: 0,
            max_seq: 0,
            retention_hint_sec: DEFAULT_RETENTION_HINT_SEC,
        });
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut cursor: CursorState = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    if cursor.retention_hint_sec <= 0 {
        cursor.retention_hint_sec = DEFAULT_RETENTION_HINT_SEC;
    }
    Ok(cursor)
}

fn load_action_cursors_or_default(path: &Path) -> Result<HashMap<String, ActionCursorState>> {
    if !path.exists() {
        return Ok(HashMap::new());
    }
    let content =
        fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let doc: ActionCursorDoc = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(doc.actions)
}

fn write_json_file(path: &Path, value: &JsonValue) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("Failed to create parent directory {}", parent.display()))?;
    let tmp_path = path.with_extension("tmp");
    let bytes = serde_json::to_vec_pretty(value)?;
    fs::write(&tmp_path, bytes)
        .with_context(|| format!("Failed to write temp file {}", tmp_path.display()))?;
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp_path, path).with_context(|| {
        format!(
            "Failed to move {} to {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(())
}
