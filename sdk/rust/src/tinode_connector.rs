use crate::credential_store::{ConnectorCredentialRecord, ConnectorCredentialStatus};
use crate::{
    connector_error_codes, ActionExecutionMode, AppConnector, AppStructureNode,
    AppStructureNodeKind, AppStructureSnapshot, AppStructureSyncResult, AuthStatus,
    ConnectorContext, ConnectorError, ConnectorInboundEvent, ConnectorInfo, ConnectorTransport,
    FetchLivePageRequest, FetchLivePageResponse, FetchSnapshotChunkRequest,
    FetchSnapshotChunkResponse, GetAppStructureRequest, GetAppStructureResponse, HealthStatus,
    RefreshAppStructureRequest, RefreshAppStructureResponse, SnapshotMeta, SnapshotRecord,
    SubmitActionOutcome, SubmitActionRequest, SubmitActionResponse,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use serde_json::{json, Map as JsonMap, Value as JsonValue};
use std::collections::{HashMap, HashSet};
use std::net::TcpStream;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

const TINODE_APP_ID: &str = "tinode";
const TINODE_CREDENTIAL_POLICY_AUTO_CREATE: &str = "auto-create";
const TINODE_STRUCTURE_REVISION: &str = "tinode-skeleton-v0";
const DEFAULT_TINODE_API_KEY: &str = "AQEAAAABAAD_rAp4DJh05a1HAwFT3A6K";
const DEFAULT_TINODE_ACCOUNT_PASSWORD: &str = "TinodeSmoke123!";
const DEFAULT_TINODE_PROTOCOL_VERSION: &str = "0.25";
const DEFAULT_TINODE_TIMEOUT_MS: u64 = 10_000;
const CONNECTOR_SIDE_EVENTS_FIELD: &str = "_appfs_events";
const TINODE_LOGIN_MIN_LEN: usize = 4;
// Tinode's basic authenticator allows 32 runes, but the MySQL-backed auth
// record stores `basic:<login>` in a 32-byte unique key. Keep generated logins
// conservative so account creation does not surface as a server-side 500.
const TINODE_LOGIN_MAX_LEN: usize = 26;
const TINODE_LOGIN_HASH_LEN: usize = 8;

type TinodeSocket = WebSocket<MaybeTlsStream<TcpStream>>;

/// Tinode connector configuration.
///
/// Endpoint and login prefix are infrastructure configuration. The API key and
/// account password are used only by the connector process and must never be
/// rendered into AppFS files, events, skills, or session logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TinodeConnectorConfig {
    pub endpoint: String,
    pub credential_policy: String,
    pub login_prefix: String,
    pub api_key: String,
    pub account_password: String,
    pub protocol_version: String,
    pub request_timeout_ms: u64,
}

impl TinodeConnectorConfig {
    pub fn new(
        endpoint: impl Into<String>,
        credential_policy: impl Into<String>,
        login_prefix: impl Into<String>,
    ) -> std::result::Result<Self, ConnectorError> {
        Self::with_options(
            endpoint,
            credential_policy,
            login_prefix,
            DEFAULT_TINODE_API_KEY,
            DEFAULT_TINODE_ACCOUNT_PASSWORD,
            DEFAULT_TINODE_PROTOCOL_VERSION,
            DEFAULT_TINODE_TIMEOUT_MS,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_options(
        endpoint: impl Into<String>,
        credential_policy: impl Into<String>,
        login_prefix: impl Into<String>,
        api_key: impl Into<String>,
        account_password: impl Into<String>,
        protocol_version: impl Into<String>,
        request_timeout_ms: u64,
    ) -> std::result::Result<Self, ConnectorError> {
        let config = Self {
            endpoint: endpoint.into(),
            credential_policy: credential_policy.into(),
            login_prefix: login_prefix.into(),
            api_key: api_key.into(),
            account_password: account_password.into(),
            protocol_version: protocol_version.into(),
            request_timeout_ms,
        };
        config.validate()
    }

    pub fn from_env() -> std::result::Result<Self, ConnectorError> {
        let endpoint = std::env::var("APPFS_TINODE_ENDPOINT").map_err(|_| {
            connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "APPFS_TINODE_ENDPOINT is required for the in-process Tinode connector",
                false,
            )
        })?;
        let login_prefix = std::env::var("APPFS_TINODE_LOGIN_PREFIX").map_err(|_| {
            connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "APPFS_TINODE_LOGIN_PREFIX is required for the in-process Tinode connector",
                false,
            )
        })?;
        let credential_policy = std::env::var("APPFS_TINODE_CREDENTIAL_POLICY")
            .unwrap_or_else(|_| TINODE_CREDENTIAL_POLICY_AUTO_CREATE.to_string());
        let api_key =
            std::env::var("APPFS_TINODE_API_KEY").unwrap_or_else(|_| DEFAULT_TINODE_API_KEY.into());
        let account_password = std::env::var("APPFS_TINODE_ACCOUNT_PASSWORD")
            .unwrap_or_else(|_| DEFAULT_TINODE_ACCOUNT_PASSWORD.into());
        let protocol_version = std::env::var("APPFS_TINODE_PROTOCOL_VERSION")
            .unwrap_or_else(|_| DEFAULT_TINODE_PROTOCOL_VERSION.into());
        let request_timeout_ms = std::env::var("APPFS_TINODE_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TINODE_TIMEOUT_MS);

        Self::with_options(
            endpoint,
            credential_policy,
            login_prefix,
            api_key,
            account_password,
            protocol_version,
            request_timeout_ms,
        )
    }

    fn validate(mut self) -> std::result::Result<Self, ConnectorError> {
        self.endpoint = self.endpoint.trim().trim_end_matches('/').to_string();
        self.credential_policy = self.credential_policy.trim().to_string();
        self.login_prefix = self.login_prefix.trim().to_string();
        self.api_key = self.api_key.trim().to_string();
        self.account_password = self.account_password.trim().to_string();
        self.protocol_version = self.protocol_version.trim().to_string();

        if self.endpoint.is_empty()
            || !(self.endpoint.starts_with("http://") || self.endpoint.starts_with("https://"))
        {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode connector endpoint must be an http(s) URL",
                false,
            ));
        }
        if self.credential_policy != TINODE_CREDENTIAL_POLICY_AUTO_CREATE {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode connector v0 supports only credential_policy=auto-create",
                false,
            ));
        }
        if !is_safe_login_prefix(&self.login_prefix) {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode login prefix must contain only ASCII letters, digits, '_' or '-'",
                false,
            ));
        }
        if self.api_key.is_empty() {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode API key must be non-empty",
                false,
            ));
        }
        if self.account_password.is_empty() {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode account password must be non-empty",
                false,
            ));
        }
        if self.protocol_version.is_empty() {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode protocol version must be non-empty",
                false,
            ));
        }
        if self.request_timeout_ms == 0 {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode request timeout must be positive",
                false,
            ));
        }

        Ok(self)
    }
}

#[derive(Default)]
struct TinodeSharedState {
    credentials: HashMap<String, ConnectorCredentialRecord>,
    principal_profiles: HashMap<String, String>,
}

static TINODE_SHARED_STATE: OnceLock<Mutex<TinodeSharedState>> = OnceLock::new();

fn tinode_shared_state() -> &'static Mutex<TinodeSharedState> {
    TINODE_SHARED_STATE.get_or_init(|| Mutex::new(TinodeSharedState::default()))
}

/// Tinode AppFS connector.
///
/// PR8 adds the first real business path: auto-create connector-private
/// credentials for the effective profile and send a direct message using a
/// root `contacts/send_message.act` action.
pub struct TinodeConnector {
    config: TinodeConnectorConfig,
    shared_namespace: String,
    credential_create_attempts: u64,
    credentials: HashMap<String, ConnectorCredentialRecord>,
    contacts: HashMap<String, TinodeContact>,
    direct_messages: HashMap<String, Vec<TinodeMessageRecord>>,
    groups: HashMap<String, TinodeGroupRecord>,
    group_messages: HashMap<String, Vec<TinodeGroupMessageRecord>>,
    inbox_recent: Vec<InboxRecord>,
    unread_message_ids: HashSet<String>,
    last_direct_seq_by_contact: HashMap<String, i64>,
    gateway: Box<dyn TinodeGateway>,
}

impl TinodeConnector {
    pub fn new(config: TinodeConnectorConfig) -> Self {
        let gateway = Box::new(WebSocketTinodeGateway::new(config.clone()));
        Self::new_with_gateway(config, gateway)
    }

    fn new_with_gateway(config: TinodeConnectorConfig, gateway: Box<dyn TinodeGateway>) -> Self {
        let shared_namespace = shared_state_namespace(&config);
        Self {
            config,
            shared_namespace,
            credential_create_attempts: 0,
            credentials: HashMap::new(),
            contacts: HashMap::new(),
            direct_messages: HashMap::new(),
            groups: HashMap::new(),
            group_messages: HashMap::new(),
            inbox_recent: Vec::new(),
            unread_message_ids: HashSet::new(),
            last_direct_seq_by_contact: HashMap::new(),
            gateway,
        }
    }

    pub fn from_env() -> std::result::Result<Self, ConnectorError> {
        Ok(Self::new(TinodeConnectorConfig::from_env()?))
    }

    #[must_use]
    pub fn config(&self) -> &TinodeConnectorConfig {
        &self.config
    }

    #[must_use]
    pub fn credential_create_attempts(&self) -> u64 {
        self.credential_create_attempts
    }

    fn snapshot(&self, ctx: &ConnectorContext) -> AppStructureSnapshot {
        AppStructureSnapshot {
            app_id: TINODE_APP_ID.to_string(),
            revision: self.structure_revision(ctx),
            active_scope: None,
            ownership_prefixes: vec![
                "_app".to_string(),
                "contacts".to_string(),
                "groups".to_string(),
                "inbox".to_string(),
                "topics".to_string(),
            ],
            nodes: self.structure_nodes(ctx),
        }
    }

    fn structure_revision(&self, ctx: &ConnectorContext) -> String {
        let profile_id = effective_profile_id(ctx).unwrap_or_else(|_| "missing-profile".into());
        let credential_status = self
            .credentials
            .get(&profile_id)
            .map(|record| credential_status_label(record.credential_status))
            .unwrap_or("missing");
        format!(
            "{TINODE_STRUCTURE_REVISION}-contacts{}-groups{}-inbox{}-{credential_status}",
            self.contacts.len(),
            self.groups.len(),
            self.inbox_recent.len()
        )
    }

    fn structure_nodes(&self, ctx: &ConnectorContext) -> Vec<AppStructureNode> {
        let mut nodes = vec![
            dir("_app"),
            static_json("_app/actions.res.json", self.actions_resource()),
            static_json("_app/control.res.json", self.control_resource()),
            static_json("_app/skill.res.json", self.skill_resource()),
            static_json("_app/self.res.json", self.self_resource(ctx)),
            action("_app/ensure_credentials.act"),
            action("_app/refresh_structure.act"),
            action("_app/refresh_inbox.act"),
            dir("contacts"),
            snapshot_jsonl("contacts/index.res.jsonl"),
            action("contacts/send_message.act"),
            action("contacts/resolve.act"),
            snapshot_jsonl("contacts/search_results.res.jsonl"),
            dir("groups"),
            snapshot_jsonl("groups/index.res.jsonl"),
            action("groups/create_group.act"),
            dir("inbox"),
            snapshot_jsonl("inbox/recent.res.jsonl"),
            snapshot_jsonl("inbox/unread.res.jsonl"),
            action("inbox/mark_read.act"),
            dir("topics"),
            snapshot_jsonl("topics/index.res.jsonl"),
        ];

        let mut contact_keys = self.contacts.keys().cloned().collect::<Vec<_>>();
        contact_keys.sort();
        for key in contact_keys {
            nodes.push(dir(&format!("contacts/{key}")));
            nodes.push(snapshot_jsonl(&format!(
                "contacts/{key}/messages.res.jsonl"
            )));
            nodes.push(action(&format!("contacts/{key}/send_message.act")));
        }

        let mut group_keys = self.groups.keys().cloned().collect::<Vec<_>>();
        group_keys.sort();
        for key in group_keys {
            if let Some(group) = self.groups.get(&key) {
                nodes.push(dir(&format!("groups/{key}")));
                nodes.push(static_json(
                    &format!("groups/{key}/group.res.json"),
                    group.to_resource_line(),
                ));
                nodes.push(snapshot_jsonl(&format!("groups/{key}/messages.res.jsonl")));
                nodes.push(action(&format!("groups/{key}/send_message.act")));
                nodes.push(action(&format!("groups/{key}/invite_members.act")));
            }
        }

        nodes.sort_by(|a, b| a.path.cmp(&b.path));
        nodes
    }

    fn skill_resource(&self) -> JsonValue {
        json!({
            "app_id": TINODE_APP_ID,
            "description": "Tinode private chat app for the current AppFS principal.",
            "when_to_use": [
                "Use when the user wants to send messages through Tinode to a person or another agent.",
                "Use when the user wants to inspect the current principal's Tinode inbox, contacts, or groups.",
                "Load this skill before performing Tinode private-app action-file operations."
            ],
            "overview_markdown": "Tinode is the current AppFS principal's private chat app. Each principal has an independent Tinode profile and credentials. Operate only under the current app root, usually `private/<principal-id>/tinode`.",
            "allowed_tools": ["bash", "read_file", "glob_search"],
            "include_generated_sections": {
                "scope_summary": false,
                "control_actions": true,
                "recommended_actions": true,
                "contact_routing": true
            }
        })
    }

    fn control_resource(&self) -> JsonValue {
        json!({
            "app_id": TINODE_APP_ID,
            "description": "Private Tinode chat control plane for the active AppFS principal.",
            "events_path": "_stream/events.evt.jsonl",
            "actions": [
                {
                    "name": "ensure_credentials",
                    "path": "_app/ensure_credentials.act",
                    "summary": "Create or reuse the current principal's Tinode credentials without exposing secrets.",
                    "example_payload": {
                        "expected_profile_id": "tinode:default",
                        "client_token": "ensure-tinode-default"
                    },
                    "use_when": [
                        "Tinode reports PROFILE_NOT_READY or the user asks to initialize the current agent's Tinode identity."
                    ]
                },
                {
                    "name": "refresh_inbox",
                    "path": "_app/refresh_inbox.act",
                    "summary": "Refresh inbox resources for the current Tinode profile.",
                    "example_payload": {
                        "client_token": "refresh-inbox-001"
                    },
                    "use_when": [
                        "Inbox resources look stale and no AppFS event reminder has arrived."
                    ]
                }
            ]
        })
    }

    fn actions_resource(&self) -> JsonValue {
        json!({
            "app_id": TINODE_APP_ID,
            "recommended_actions": [
                {
                    "name": "send_direct_message",
                    "path": "contacts/send_message.act",
                    "summary": "Send a Tinode direct message. Use `to` with `basic:<login>`, `tinode_user:<usr-id>`, or `principal:<principal-id>`.",
                    "example_payload": {
                        "to": "principal:code-implementer",
                        "text": "请接手实现这个任务。",
                        "client_token": "msg-to-agent-001"
                    },
                    "use_when": [
                        "The user asks to message another agent or a Tinode contact and the exact contact path is unknown.",
                        "The user asks default to delegate work to a forked principal such as code-implementer."
                    ]
                },
                {
                    "name": "create_group",
                    "path": "groups/create_group.act",
                    "summary": "Create a Tinode group and invite members.",
                    "example_payload": {
                        "title": "multi-agent-smoke",
                        "members": ["principal:code-implementer"],
                        "client_token": "grp-multi-agent-001"
                    },
                    "use_when": [
                        "The user wants multiple agents or contacts to collaborate in one Tinode group."
                    ]
                },
                {
                    "name": "mark_inbox_read",
                    "path": "inbox/mark_read.act",
                    "summary": "Mark inbox messages as read for the current principal.",
                    "example_payload": {
                        "client_token": "mark-read-001"
                    },
                    "use_when": [
                        "The user asks to acknowledge or clear read state for Tinode inbox messages."
                    ]
                }
            ],
            "contact_routes": [
                {
                    "mention_tokens": ["another agent", "其他 agent", "code-implementer", "principal:<principal-id>"],
                    "send_message_path": "contacts/send_message.act"
                }
            ]
        })
    }

    fn self_resource(&self, ctx: &ConnectorContext) -> JsonValue {
        let principal_id = ctx.principal_id.as_deref().unwrap_or("default");
        let profile_id = ctx
            .profile_id
            .as_deref()
            .unwrap_or("tinode:unknown-profile");

        if let Some(record) = self.credentials.get(profile_id) {
            let summary = record.safe_summary();
            return json!({
                "app_id": TINODE_APP_ID,
                "principal_id": principal_id,
                "profile_id": summary.profile_id,
                "credential_policy": self.config.credential_policy,
                "credential_status": summary.credential_status,
                "tinode_user_id": summary.upstream_user_id,
                "login": summary.login,
                "display_name": summary.display_name.unwrap_or_else(|| principal_id.to_string()),
                "owner_ref": principal_id,
            });
        }

        json!({
            "app_id": TINODE_APP_ID,
            "principal_id": principal_id,
            "profile_id": profile_id,
            "credential_policy": self.config.credential_policy,
            "credential_status": "missing",
            "tinode_user_id": null,
            "login": null,
            "display_name": principal_id,
            "owner_ref": null,
        })
    }

    fn fetch_snapshot_response(
        &self,
        request: FetchSnapshotChunkRequest,
    ) -> std::result::Result<FetchSnapshotChunkResponse, ConnectorError> {
        let normalized = normalize_path(&request.resource_path);
        if !is_tinode_snapshot_resource(&normalized) {
            return Err(connector_err(
                connector_error_codes::NOT_SUPPORTED,
                format!(
                    "unknown Tinode snapshot resource: {}",
                    request.resource_path
                ),
                false,
            ));
        }

        let records = match normalized.as_str() {
            "contacts/index.res.jsonl" => self.contact_index_records(),
            "groups/index.res.jsonl" => self.group_index_records(),
            "inbox/recent.res.jsonl" => self.inbox_records(false),
            "inbox/unread.res.jsonl" => self.inbox_records(true),
            "contacts/search_results.res.jsonl" | "topics/index.res.jsonl" => Vec::new(),
            path if contact_messages_key(path).is_some() => {
                let key = contact_messages_key(path).expect("checked contact messages path");
                self.contact_message_records(key)
            }
            path if group_messages_key(path).is_some() => {
                let key = group_messages_key(path).expect("checked group messages path");
                self.group_message_records(key)
            }
            _ => Vec::new(),
        };
        let emitted_bytes = records
            .iter()
            .map(|record| {
                serde_json::to_string(&record.line)
                    .unwrap_or_default()
                    .len() as u64
                    + 1
            })
            .sum();

        Ok(FetchSnapshotChunkResponse {
            records,
            emitted_bytes,
            next_cursor: None,
            has_more: false,
            revision: Some(TINODE_STRUCTURE_REVISION.to_string()),
        })
    }

    fn contact_index_records(&self) -> Vec<SnapshotRecord> {
        let mut contacts = self.contacts.values().cloned().collect::<Vec<_>>();
        contacts.sort_by(|a, b| a.key.cmp(&b.key));
        contacts
            .into_iter()
            .map(|contact| SnapshotRecord {
                record_key: contact.key.clone(),
                ordering_key: contact.key.clone(),
                line: contact.to_resource_line(),
            })
            .collect()
    }

    fn contact_message_records(&self, contact_key: &str) -> Vec<SnapshotRecord> {
        self.direct_messages
            .get(contact_key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|message| SnapshotRecord {
                record_key: message.message_id.clone(),
                ordering_key: message.created_at_ms.to_string(),
                line: message.to_resource_line(),
            })
            .collect()
    }

    fn group_index_records(&self) -> Vec<SnapshotRecord> {
        let mut groups = self.groups.values().cloned().collect::<Vec<_>>();
        groups.sort_by(|a, b| a.key.cmp(&b.key));
        groups
            .into_iter()
            .map(|group| SnapshotRecord {
                record_key: group.key.clone(),
                ordering_key: group.created_at_ms.to_string(),
                line: group.to_resource_line(),
            })
            .collect()
    }

    fn group_message_records(&self, group_key: &str) -> Vec<SnapshotRecord> {
        self.group_messages
            .get(group_key)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|message| SnapshotRecord {
                record_key: message.message_id.clone(),
                ordering_key: message.created_at_ms.to_string(),
                line: message.to_resource_line(),
            })
            .collect()
    }

    fn inbox_records(&self, unread_only: bool) -> Vec<SnapshotRecord> {
        self.inbox_recent
            .iter()
            .filter(|record| !unread_only || self.unread_message_ids.contains(&record.message_id))
            .cloned()
            .map(|record| SnapshotRecord {
                record_key: record.message_id.clone(),
                ordering_key: record.created_at_ms.to_string(),
                line: record.to_resource_line(self.unread_message_ids.contains(&record.message_id)),
            })
            .collect()
    }

    fn ensure_credentials(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<(ConnectorCredentialRecord, bool), ConnectorError> {
        let profile_id = effective_profile_id(ctx)?;
        let principal_id = ctx.principal_id.as_deref().unwrap_or("default");
        if let Some(record) = self.ready_credential_record_for_profile(&profile_id, principal_id) {
            return Ok((record, false));
        }

        self.credential_create_attempts += 1;
        let login = login_for_profile(&self.config, principal_id, &profile_id);
        let display_name = display_name_for_principal(principal_id);
        let request = TinodeAccountRequest {
            profile_id: profile_id.clone(),
            login: login.clone(),
            password: self.config.account_password.clone(),
            display_name: display_name.clone(),
            tags: vec![
                login.clone(),
                profile_id.clone(),
                principal_id.to_string(),
                "appfs-agent".to_string(),
            ],
        };
        let account = self.gateway.create_or_reuse_account(request)?;
        let record = ConnectorCredentialRecord {
            profile_id: profile_id.clone(),
            credential_status: ConnectorCredentialStatus::Ready,
            upstream_user_id: Some(account.tinode_user_id.clone()),
            login: Some(account.login.clone()),
            display_name: Some(account.display_name.clone()),
            last_ready_at: Some(now_millis_string()),
            expires_at: None,
            credentials: Some(json!({
                "scheme": "tinode_token",
                "token": account.token,
            })),
        };
        self.credentials.insert(profile_id, record.clone());
        self.store_shared_credential(principal_id, &record);
        Ok((record, true))
    }

    fn ready_credential_record_for_profile(
        &mut self,
        profile_id: &str,
        principal_id: &str,
    ) -> Option<ConnectorCredentialRecord> {
        if let Some(record) = self.credentials.get(profile_id).cloned() {
            if record.credential_status == ConnectorCredentialStatus::Ready {
                self.remember_principal_profile(principal_id, &record);
                return Some(record);
            }
        }
        if let Some(record) = self.shared_credential(profile_id) {
            if record.credential_status == ConnectorCredentialStatus::Ready {
                self.credentials
                    .insert(profile_id.to_string(), record.clone());
                self.remember_principal_profile(principal_id, &record);
                return Some(record);
            }
        }
        None
    }

    fn shared_credential(&self, profile_id: &str) -> Option<ConnectorCredentialRecord> {
        let state = tinode_shared_state().lock().ok()?;
        state
            .credentials
            .get(&shared_credential_key(&self.shared_namespace, profile_id))
            .cloned()
    }

    fn store_shared_credential(&self, principal_id: &str, record: &ConnectorCredentialRecord) {
        if let Ok(mut state) = tinode_shared_state().lock() {
            state.credentials.insert(
                shared_credential_key(&self.shared_namespace, &record.profile_id),
                record.clone(),
            );
            state.principal_profiles.insert(
                shared_principal_key(&self.shared_namespace, principal_id),
                record.profile_id.clone(),
            );
        }
    }

    fn remember_principal_profile(&self, principal_id: &str, record: &ConnectorCredentialRecord) {
        if let Ok(mut state) = tinode_shared_state().lock() {
            state.principal_profiles.insert(
                shared_principal_key(&self.shared_namespace, principal_id),
                record.profile_id.clone(),
            );
        }
    }

    fn remember_shared_principal_contacts(
        &mut self,
        credentials: &TinodeCredentials,
        current_principal_id: Option<&str>,
    ) -> std::result::Result<(), ConnectorError> {
        let state = tinode_shared_state().lock().map_err(|_| {
            connector_err(
                connector_error_codes::INTERNAL,
                "Tinode shared credential state is poisoned",
                true,
            )
        })?;
        let principal_prefix = format!("{}|principal:", self.shared_namespace);
        let mut contacts = Vec::new();
        for (principal_key, profile_id) in &state.principal_profiles {
            let Some(principal_id) = principal_key.strip_prefix(&principal_prefix) else {
                continue;
            };
            if current_principal_id == Some(principal_id) || profile_id == &credentials.profile_id {
                continue;
            }
            let Some(record) = state
                .credentials
                .get(&shared_credential_key(&self.shared_namespace, profile_id))
            else {
                continue;
            };
            if record.credential_status != ConnectorCredentialStatus::Ready {
                continue;
            }
            let Some(tinode_user_id) = record.upstream_user_id.clone() else {
                continue;
            };
            if tinode_user_id == credentials.tinode_user_id {
                continue;
            }
            contacts.push(TinodeContact {
                key: sanitize_path_key(principal_id),
                tinode_user_id,
                basic_login: record.login.clone(),
                display_name: record.display_name.clone(),
            });
        }
        drop(state);

        for contact in contacts {
            self.contacts.entry(contact.key.clone()).or_insert(contact);
        }
        Ok(())
    }

    fn resolve_recipient(
        &mut self,
        credentials: &TinodeCredentials,
        reference: RecipientRef,
    ) -> std::result::Result<TinodeContact, ConnectorError> {
        match reference {
            RecipientRef::Basic(login) => {
                let key = contact_key_from_basic_login(&login);
                if let Some(contact) = self.contacts.get(&key) {
                    return Ok(contact.clone());
                }
                let contact = self.gateway.resolve_basic_user(credentials, &login)?;
                self.contacts.insert(contact.key.clone(), contact.clone());
                Ok(contact)
            }
            RecipientRef::ContactKey(key) => self.contacts.get(&key).cloned().ok_or_else(|| {
                connector_err(
                    connector_error_codes::PROFILE_NOT_FOUND,
                    format!("Tinode contact `{key}` is not known yet; use contacts/send_message.act with to=\"basic:<login>\" first"),
                    false,
                )
            }),
            RecipientRef::TinodeUser(tinode_user_id) => Ok(TinodeContact {
                key: contact_key_from_tinode_user(&tinode_user_id),
                tinode_user_id,
                basic_login: None,
                display_name: None,
            }),
            RecipientRef::Principal(principal_id) => self.resolve_principal_contact(&principal_id),
        }
    }

    fn resolve_principal_contact(
        &self,
        principal_id: &str,
    ) -> std::result::Result<TinodeContact, ConnectorError> {
        let member = self.resolve_principal_group_member(principal_id)?;
        Ok(TinodeContact {
            key: sanitize_path_key(principal_id),
            tinode_user_id: member.tinode_user_id,
            basic_login: member.basic_login,
            display_name: member.display_name,
        })
    }

    fn resolve_principal_group_member(
        &self,
        principal_id: &str,
    ) -> std::result::Result<TinodeGroupMember, ConnectorError> {
        let state = tinode_shared_state().lock().map_err(|_| {
            connector_err(
                connector_error_codes::INTERNAL,
                "Tinode shared credential state is poisoned",
                true,
            )
        })?;
        let principal_key = shared_principal_key(&self.shared_namespace, principal_id);
        let profile_id = state
            .principal_profiles
            .get(&principal_key)
            .cloned()
            .ok_or_else(|| {
                connector_err(
                    connector_error_codes::PROFILE_NOT_READY,
                    format!(
                        "Tinode profile for principal:{principal_id} is not ready; start that agent and run _app/ensure_credentials.act first"
                    ),
                    true,
                )
            })?;
        let record = state
            .credentials
            .get(&shared_credential_key(&self.shared_namespace, &profile_id))
            .ok_or_else(|| {
                connector_err(
                    connector_error_codes::PROFILE_NOT_READY,
                    format!(
                        "Tinode credentials for principal:{principal_id} profile {profile_id} are not ready"
                    ),
                    true,
                )
            })?;
        let tinode_user_id = record.upstream_user_id.clone().ok_or_else(|| {
            connector_err(
                connector_error_codes::PROFILE_NOT_READY,
                format!("Tinode credentials for principal:{principal_id} have no upstream user id"),
                true,
            )
        })?;
        Ok(TinodeGroupMember {
            kind: "principal".to_string(),
            principal_id: Some(principal_id.to_string()),
            profile_id: Some(profile_id),
            contact_key: None,
            tinode_user_id,
            basic_login: record.login.clone(),
            display_name: record.display_name.clone(),
        })
    }

    fn submit_ensure_credentials(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let (record, _) = self.ensure_credentials(ctx)?;
        Ok(completed_response(
            ctx,
            serde_json::to_value(record.safe_summary()).map_err(|err| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    format!("failed to encode credential summary: {err}"),
                    true,
                )
            })?,
        ))
    }

    fn submit_resolve_contact(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let reference = recipient_ref_from_payload(&request.payload)?;
        let (record, created_credentials) = self.ensure_credentials(ctx)?;
        let credentials = credentials_from_record(&record)?;
        let contact = self.resolve_recipient(&credentials, reference)?;
        let mut content = json!({
            "ok": true,
            "contact": contact.to_resource_line(),
        });
        add_side_event(
            &mut content,
            "profile.credentials.ready",
            Some(serde_json::to_value(record.safe_summary()).map_err(|err| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    format!("failed to encode credential summary: {err}"),
                    true,
                )
            })?),
            created_credentials,
        );
        add_side_event(
            &mut content,
            "contact.resolved",
            Some(json!({ "contact": contact.to_resource_line() })),
            true,
        );
        Ok(completed_response(ctx, content))
    }

    fn submit_send_message(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let normalized_path = normalize_path(&request.path);
        let (reference, text) = message_target_and_text(&normalized_path, &request.payload)?;
        let (record, created_credentials) = self.ensure_credentials(ctx)?;
        let credentials = credentials_from_record(&record)?;
        let contact = self.resolve_recipient(&credentials, reference)?;
        let receipt = self.gateway.send_direct_message(
            &credentials,
            &contact,
            &text,
            ctx.client_token.as_deref(),
        )?;
        let message = TinodeMessageRecord {
            message_id: receipt.message_id.clone(),
            contact_key: contact.key.clone(),
            direction: "outbound".to_string(),
            text: text.clone(),
            created_at_ms: now_millis(),
            seq: receipt.seq,
        };
        self.direct_messages
            .entry(contact.key.clone())
            .or_default()
            .push(message.clone());

        let safe_summary = record.safe_summary();
        let mut content = json!({
            "ok": true,
            "conversation_type": "direct",
            "principal_id": ctx.principal_id.as_deref().unwrap_or("default"),
            "profile_id": safe_summary.profile_id,
            "message_id": receipt.message_id,
            "topic": receipt.topic,
            "to": contact.to_resource_line(),
            "text_preview": text_preview(&text),
        });
        add_side_event(
            &mut content,
            "action.accepted",
            Some(json!({
                "conversation_type": "direct",
                "path": request.path,
                "profile_id": safe_summary.profile_id,
            })),
            true,
        );
        add_side_event(
            &mut content,
            "profile.credentials.ready",
            Some(serde_json::to_value(safe_summary.clone()).map_err(|err| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    format!("failed to encode credential summary: {err}"),
                    true,
                )
            })?),
            created_credentials,
        );
        add_side_event(
            &mut content,
            "message.sent",
            Some(json!({
                "conversation_type": "direct",
                "principal_id": ctx.principal_id.as_deref().unwrap_or("default"),
                "profile_id": safe_summary.profile_id,
                "path": request.path,
                "message_id": message.message_id,
                "to_display_name": contact.display_name,
                "to_tinode_user_id": contact.tinode_user_id,
                "text_preview": text_preview(&text),
                "client_token": ctx.client_token,
            })),
            true,
        );

        Ok(completed_response(ctx, content))
    }

    fn submit_create_group(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let title = group_title_from_payload(&request.payload)?;
        let group_key = group_key_from_payload(&request.payload, &title);
        let member_refs = group_member_refs_from_payload(&request.payload)?;
        let initial_message = request
            .payload
            .get("initial_message")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);
        let (record, created_credentials) = self.ensure_credentials(ctx)?;
        let credentials = credentials_from_record(&record)?;
        let members = self.resolve_group_members(&credentials, member_refs)?;
        let receipt = self.gateway.create_group(
            &credentials,
            &title,
            &members,
            ctx.client_token.as_deref(),
        )?;
        let created_at_ms = now_millis();
        let group = TinodeGroupRecord {
            key: group_key.clone(),
            title: title.clone(),
            topic_id: receipt.topic.clone(),
            members: members.clone(),
            created_at_ms,
            last_message_at_ms: None,
        };
        self.groups.insert(group_key.clone(), group.clone());

        let mut sent_initial = None;
        if let Some(text) = initial_message {
            let receipt = self.gateway.send_group_message(
                &credentials,
                &group,
                &text,
                ctx.client_token.as_deref(),
            )?;
            let message = TinodeGroupMessageRecord::outbound(&group, receipt, text);
            sent_initial = Some(message.message_id.clone());
            self.group_messages
                .entry(group_key.clone())
                .or_default()
                .push(message);
            if let Some(group) = self.groups.get_mut(&group_key) {
                group.last_message_at_ms = Some(now_millis());
            }
        }

        let safe_summary = record.safe_summary();
        let mut content = json!({
            "ok": true,
            "conversation_type": "group",
            "principal_id": ctx.principal_id.as_deref().unwrap_or("default"),
            "profile_id": safe_summary.profile_id,
            "group_key": group_key,
            "topic": group.topic_id,
            "title": group.title,
            "member_count": group.members.len(),
            "initial_message_id": sent_initial,
            "structure_changed": true,
        });
        add_side_event(
            &mut content,
            "action.accepted",
            Some(json!({
                "conversation_type": "group",
                "path": request.path,
                "profile_id": safe_summary.profile_id,
            })),
            true,
        );
        add_side_event(
            &mut content,
            "profile.credentials.ready",
            Some(serde_json::to_value(safe_summary.clone()).map_err(|err| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    format!("failed to encode credential summary: {err}"),
                    true,
                )
            })?),
            created_credentials,
        );
        add_side_event(
            &mut content,
            "group.created",
            Some(json!({
                "principal_id": ctx.principal_id.as_deref().unwrap_or("default"),
                "profile_id": safe_summary.profile_id,
                "path": format!("groups/{}/group.res.json", group.key),
                "group_key": group.key,
                "topic": group.topic_id,
                "title": group.title,
                "member_count": group.members.len(),
            })),
            true,
        );
        Ok(completed_response(ctx, content))
    }

    fn submit_invite_group_members(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
        group_key: &str,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let member_refs = group_member_refs_from_payload(&request.payload)?;
        let (record, created_credentials) = self.ensure_credentials(ctx)?;
        let credentials = credentials_from_record(&record)?;
        let members = self.resolve_group_members(&credentials, member_refs)?;
        let group = self.groups.get(group_key).cloned().ok_or_else(|| {
            connector_err(
                connector_error_codes::PROFILE_NOT_FOUND,
                format!("Tinode group `{group_key}` is not known yet"),
                false,
            )
        })?;
        self.gateway
            .invite_group_members(&credentials, &group, &members)?;
        if let Some(stored) = self.groups.get_mut(group_key) {
            for member in members {
                if !stored
                    .members
                    .iter()
                    .any(|existing| existing.tinode_user_id == member.tinode_user_id)
                {
                    stored.members.push(member);
                }
            }
        }
        let safe_summary = record.safe_summary();
        let mut content = json!({
            "ok": true,
            "conversation_type": "group",
            "group_key": group_key,
            "topic": group.topic_id,
            "profile_id": safe_summary.profile_id,
            "structure_changed": true,
        });
        add_side_event(
            &mut content,
            "profile.credentials.ready",
            Some(serde_json::to_value(safe_summary.clone()).map_err(|err| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    format!("failed to encode credential summary: {err}"),
                    true,
                )
            })?),
            created_credentials,
        );
        add_side_event(
            &mut content,
            "group.members.invited",
            Some(json!({
                "profile_id": safe_summary.profile_id,
                "group_key": group_key,
                "topic": group.topic_id,
            })),
            true,
        );
        Ok(completed_response(ctx, content))
    }

    fn submit_send_group_message(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
        group_key: &str,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let text = text_from_payload(&request.payload, "Tinode group send_message")?;
        let (record, created_credentials) = self.ensure_credentials(ctx)?;
        let credentials = credentials_from_record(&record)?;
        let group = self.groups.get(group_key).cloned().ok_or_else(|| {
            connector_err(
                connector_error_codes::PROFILE_NOT_FOUND,
                format!("Tinode group `{group_key}` is not known yet"),
                false,
            )
        })?;
        let receipt = self.gateway.send_group_message(
            &credentials,
            &group,
            &text,
            ctx.client_token.as_deref(),
        )?;
        let message = TinodeGroupMessageRecord::outbound(&group, receipt.clone(), text.clone());
        self.group_messages
            .entry(group_key.to_string())
            .or_default()
            .push(message.clone());
        if let Some(group) = self.groups.get_mut(group_key) {
            group.last_message_at_ms = Some(now_millis());
        }

        let safe_summary = record.safe_summary();
        let mut content = json!({
            "ok": true,
            "conversation_type": "group",
            "principal_id": ctx.principal_id.as_deref().unwrap_or("default"),
            "profile_id": safe_summary.profile_id,
            "group_key": group_key,
            "topic": group.topic_id,
            "message_id": receipt.message_id,
            "text_preview": text_preview(&text),
        });
        add_side_event(
            &mut content,
            "profile.credentials.ready",
            Some(serde_json::to_value(safe_summary.clone()).map_err(|err| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    format!("failed to encode credential summary: {err}"),
                    true,
                )
            })?),
            created_credentials,
        );
        add_side_event(
            &mut content,
            "message.sent",
            Some(json!({
                "conversation_type": "group",
                "principal_id": ctx.principal_id.as_deref().unwrap_or("default"),
                "profile_id": safe_summary.profile_id,
                "path": request.path,
                "group_key": group_key,
                "topic": group.topic_id,
                "message_id": message.message_id,
                "text_preview": text_preview(&text),
                "client_token": ctx.client_token,
            })),
            true,
        );

        Ok(completed_response(ctx, content))
    }

    fn resolve_group_members(
        &mut self,
        credentials: &TinodeCredentials,
        refs: Vec<RecipientRef>,
    ) -> std::result::Result<Vec<TinodeGroupMember>, ConnectorError> {
        let mut members = Vec::new();
        let mut seen = HashSet::new();
        for reference in refs {
            let member = self.resolve_group_member(credentials, reference)?;
            if seen.insert(member.tinode_user_id.clone()) {
                members.push(member);
            }
        }
        Ok(members)
    }

    fn resolve_group_member(
        &mut self,
        credentials: &TinodeCredentials,
        reference: RecipientRef,
    ) -> std::result::Result<TinodeGroupMember, ConnectorError> {
        match reference {
            RecipientRef::Principal(principal_id) => {
                self.resolve_principal_group_member(&principal_id)
            }
            RecipientRef::ContactKey(key) => {
                let contact = self.resolve_recipient(credentials, RecipientRef::ContactKey(key))?;
                Ok(TinodeGroupMember::from_contact("contact", contact))
            }
            RecipientRef::Basic(login) => {
                let contact = self.resolve_recipient(credentials, RecipientRef::Basic(login))?;
                Ok(TinodeGroupMember::from_contact("basic", contact))
            }
            RecipientRef::TinodeUser(tinode_user_id) => Ok(TinodeGroupMember::from_contact(
                "tinode_user",
                TinodeContact {
                    key: contact_key_from_tinode_user(&tinode_user_id),
                    tinode_user_id,
                    basic_login: None,
                    display_name: None,
                },
            )),
        }
    }

    fn drain_inbound_for_ctx(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<Vec<ConnectorInboundEvent>, ConnectorError> {
        let Ok(profile_id) = effective_profile_id(ctx) else {
            return Ok(Vec::new());
        };
        let principal_id = ctx.principal_id.as_deref().unwrap_or("default");
        let Some(record) = self.ready_credential_record_for_profile(&profile_id, principal_id)
        else {
            return Ok(Vec::new());
        };
        if record.credential_status != ConnectorCredentialStatus::Ready {
            return Ok(Vec::new());
        }
        let credentials = credentials_from_record(&record)?;
        self.remember_shared_principal_contacts(&credentials, Some(principal_id))?;
        let contacts = self.contacts.values().cloned().collect::<Vec<_>>();
        if contacts.is_empty() {
            return Ok(Vec::new());
        }

        let mut events = Vec::new();
        for contact in contacts {
            let since_seq = self
                .last_direct_seq_by_contact
                .get(&contact.key)
                .copied()
                .or_else(|| {
                    self.direct_messages.get(&contact.key).and_then(|messages| {
                        messages.iter().filter_map(|message| message.seq).max()
                    })
                });
            let inbound_messages =
                self.gateway
                    .fetch_direct_messages(&credentials, &contact, since_seq)?;
            for inbound in inbound_messages {
                if inbound.from_tinode_user_id == credentials.tinode_user_id {
                    self.last_direct_seq_by_contact
                        .insert(contact.key.clone(), inbound.seq);
                    continue;
                }
                let message_id = format!("tinode:{}:{}", inbound.topic, inbound.seq);
                if self
                    .direct_messages
                    .get(&contact.key)
                    .map(|messages| {
                        messages
                            .iter()
                            .any(|message| message.message_id == message_id)
                    })
                    .unwrap_or(false)
                {
                    self.last_direct_seq_by_contact
                        .insert(contact.key.clone(), inbound.seq);
                    continue;
                }

                let message = TinodeMessageRecord {
                    message_id: message_id.clone(),
                    contact_key: contact.key.clone(),
                    direction: "inbound".to_string(),
                    text: inbound.text.clone(),
                    created_at_ms: now_millis(),
                    seq: Some(inbound.seq),
                };
                self.direct_messages
                    .entry(contact.key.clone())
                    .or_default()
                    .push(message.clone());
                self.last_direct_seq_by_contact
                    .insert(contact.key.clone(), inbound.seq);
                self.unread_message_ids.insert(message_id.clone());

                let inbox_record = InboxRecord {
                    message_id: message_id.clone(),
                    contact_key: contact.key.clone(),
                    conversation_type: "direct".to_string(),
                    from_tinode_user_id: inbound.from_tinode_user_id.clone(),
                    from_display_name: contact.display_name.clone(),
                    text: inbound.text.clone(),
                    created_at_ms: message.created_at_ms,
                    requires_attention: true,
                };
                self.inbox_recent.push(inbox_record.clone());
                events.push(ConnectorInboundEvent {
                    event_type: "message.received".to_string(),
                    path: format!("contacts/{}/messages.res.jsonl", contact.key),
                    content: Some(inbox_record.to_event_content(&profile_id)),
                    error: None,
                });
                events.push(ConnectorInboundEvent {
                    event_type: "inbox.updated".to_string(),
                    path: "inbox/unread.res.jsonl".to_string(),
                    content: Some(json!({
                        "message_id": message_id,
                        "conversation_type": "direct",
                        "contact_key": contact.key,
                        "unread_count": self.unread_message_ids.len(),
                    })),
                    error: None,
                });
            }
        }
        Ok(events)
    }

    fn submit_refresh_inbox(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let events = self.drain_inbound_for_ctx(ctx)?;
        let mut content = json!({
            "ok": true,
            "refreshed": true,
            "event_count": events.len(),
            "unread_count": self.unread_message_ids.len(),
        });
        for event in events {
            add_side_event_with_path(
                &mut content,
                &event.event_type,
                &event.path,
                event.content,
                true,
            );
        }
        Ok(completed_response(ctx, content))
    }

    fn submit_mark_read(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        let mark_all = request
            .payload
            .get("all")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let message_id = request
            .payload
            .get("message_id")
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        let mut cleared = Vec::new();
        if mark_all {
            cleared.extend(self.unread_message_ids.drain());
        } else if let Some(message_id) = message_id {
            if self.unread_message_ids.remove(&message_id) {
                cleared.push(message_id);
            }
        } else {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode inbox/mark_read.act requires `all=true` or a non-empty `message_id`",
                false,
            ));
        }

        let mut content = json!({
            "ok": true,
            "cleared": cleared,
            "unread_count": self.unread_message_ids.len(),
        });
        let cleared_for_event = content.get("cleared").cloned().unwrap_or_else(|| json!([]));
        add_side_event_with_path(
            &mut content,
            "message.read",
            "inbox/unread.res.jsonl",
            Some(json!({
                "cleared": cleared_for_event,
                "unread_count": self.unread_message_ids.len(),
            })),
            true,
        );
        Ok(completed_response(ctx, content))
    }
}

impl AppConnector for TinodeConnector {
    fn connector_id(&self) -> std::result::Result<ConnectorInfo, ConnectorError> {
        Ok(ConnectorInfo {
            connector_id: "tinode-in-process".to_string(),
            version: "0.2.0".to_string(),
            app_id: TINODE_APP_ID.to_string(),
            transport: ConnectorTransport::InProcess,
            supports_snapshot: true,
            supports_live: false,
            supports_action: true,
            optional_features: vec![
                "tinode".to_string(),
                "skeleton_tree".to_string(),
                "credential_policy:auto-create".to_string(),
                "direct_message".to_string(),
                "inbound_inbox".to_string(),
            ],
        })
    }

    fn health(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<HealthStatus, ConnectorError> {
        let auth_status = effective_profile_id(ctx)
            .ok()
            .and_then(|profile_id| self.credentials.get(&profile_id).cloned())
            .map(|record| {
                if record.credential_status == ConnectorCredentialStatus::Ready {
                    AuthStatus::Valid
                } else {
                    AuthStatus::Invalid
                }
            })
            .unwrap_or(AuthStatus::Invalid);
        Ok(HealthStatus {
            healthy: true,
            auth_status,
            message: Some("Tinode connector is configured".to_string()),
            checked_at: now_millis_string(),
        })
    }

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        _timeout: Duration,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<SnapshotMeta, ConnectorError> {
        if !is_tinode_snapshot_resource(resource_path) {
            return Err(connector_err(
                connector_error_codes::NOT_SUPPORTED,
                format!("unknown Tinode snapshot resource: {resource_path}"),
                false,
            ));
        }
        Ok(SnapshotMeta {
            size_bytes: Some(0),
            revision: Some(TINODE_STRUCTURE_REVISION.to_string()),
            last_modified: Some(now_millis_string()),
            item_count: Some(0),
        })
    }

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchSnapshotChunkResponse, ConnectorError> {
        let normalized = normalize_path(&request.resource_path);
        if tinode_snapshot_read_should_refresh_inbound(&normalized) {
            self.drain_inbound_for_ctx(ctx)?;
        }
        self.fetch_snapshot_response(request)
    }

    fn fetch_live_page(
        &mut self,
        _request: FetchLivePageRequest,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<FetchLivePageResponse, ConnectorError> {
        Err(connector_err(
            connector_error_codes::NOT_SUPPORTED,
            "Tinode connector v0 does not expose live pageable resources",
            false,
        ))
    }

    fn submit_action(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError> {
        if !matches!(request.execution_mode, ActionExecutionMode::Inline) {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode actions must be inline in v0",
                false,
            ));
        }

        let normalized_path = normalize_path(&request.path);
        match normalized_path.as_str() {
            "_app/ensure_credentials.act" => self.submit_ensure_credentials(ctx),
            "contacts/resolve.act" => self.submit_resolve_contact(request, ctx),
            "contacts/send_message.act" => self.submit_send_message(request, ctx),
            "_app/refresh_inbox.act" => self.submit_refresh_inbox(ctx),
            "_app/refresh_structure.act" => Ok(completed_response(
                ctx,
                json!({ "ok": true, "refreshed": false, "reason": "no-op in Tinode connector v0" }),
            )),
            "inbox/mark_read.act" => self.submit_mark_read(request, ctx),
            "groups/create_group.act" => self.submit_create_group(request, ctx),
            path if group_send_message_key(path).is_some() => {
                let group_key = group_send_message_key(path).expect("checked group send path");
                self.submit_send_group_message(request, ctx, group_key)
            }
            path if group_invite_members_key(path).is_some() => {
                let group_key = group_invite_members_key(path).expect("checked group invite path");
                self.submit_invite_group_members(request, ctx, group_key)
            }
            path if contact_send_message_key(path).is_some() => {
                self.submit_send_message(request, ctx)
            }
            _ => Err(connector_err(
                connector_error_codes::NOT_SUPPORTED,
                format!("unknown Tinode action: {}", request.path),
                false,
            )),
        }
    }

    fn drain_inbound_events(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<Vec<ConnectorInboundEvent>, ConnectorError> {
        self.drain_inbound_for_ctx(ctx)
    }

    fn get_app_structure(
        &mut self,
        request: GetAppStructureRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<GetAppStructureResponse, ConnectorError> {
        let snapshot = self.snapshot(ctx);
        if request.known_revision.as_deref() == Some(snapshot.revision.as_str()) {
            return Ok(GetAppStructureResponse {
                result: AppStructureSyncResult::Unchanged {
                    app_id: request.app_id,
                    revision: snapshot.revision,
                    active_scope: snapshot.active_scope,
                },
            });
        }
        Ok(GetAppStructureResponse {
            result: AppStructureSyncResult::Snapshot { snapshot },
        })
    }

    fn refresh_app_structure(
        &mut self,
        request: RefreshAppStructureRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<RefreshAppStructureResponse, ConnectorError> {
        let snapshot = self.snapshot(ctx);
        if request.known_revision.as_deref() == Some(snapshot.revision.as_str()) {
            return Ok(RefreshAppStructureResponse {
                result: AppStructureSyncResult::Unchanged {
                    app_id: request.app_id,
                    revision: snapshot.revision,
                    active_scope: snapshot.active_scope,
                },
            });
        }
        Ok(RefreshAppStructureResponse {
            result: AppStructureSyncResult::Snapshot { snapshot },
        })
    }
}

trait TinodeGateway: Send {
    fn create_or_reuse_account(
        &mut self,
        request: TinodeAccountRequest,
    ) -> std::result::Result<TinodeAccount, ConnectorError>;

    fn resolve_basic_user(
        &mut self,
        credentials: &TinodeCredentials,
        login: &str,
    ) -> std::result::Result<TinodeContact, ConnectorError>;

    fn send_direct_message(
        &mut self,
        credentials: &TinodeCredentials,
        contact: &TinodeContact,
        text: &str,
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeSendReceipt, ConnectorError>;

    fn fetch_direct_messages(
        &mut self,
        credentials: &TinodeCredentials,
        contact: &TinodeContact,
        since_seq: Option<i64>,
    ) -> std::result::Result<Vec<TinodeInboundMessage>, ConnectorError>;

    fn create_group(
        &mut self,
        credentials: &TinodeCredentials,
        title: &str,
        members: &[TinodeGroupMember],
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeGroupReceipt, ConnectorError>;

    fn invite_group_members(
        &mut self,
        credentials: &TinodeCredentials,
        group: &TinodeGroupRecord,
        members: &[TinodeGroupMember],
    ) -> std::result::Result<(), ConnectorError>;

    fn send_group_message(
        &mut self,
        credentials: &TinodeCredentials,
        group: &TinodeGroupRecord,
        text: &str,
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeSendReceipt, ConnectorError>;
}

#[derive(Debug, Clone)]
struct TinodeAccountRequest {
    profile_id: String,
    login: String,
    password: String,
    display_name: String,
    tags: Vec<String>,
}

#[derive(Debug, Clone)]
struct TinodeAccount {
    tinode_user_id: String,
    login: String,
    display_name: String,
    token: String,
}

#[derive(Debug, Clone)]
struct TinodeCredentials {
    profile_id: String,
    tinode_user_id: String,
    login: String,
    token: String,
}

#[derive(Debug, Clone)]
struct TinodeContact {
    key: String,
    tinode_user_id: String,
    basic_login: Option<String>,
    display_name: Option<String>,
}

impl TinodeContact {
    fn to_resource_line(&self) -> JsonValue {
        json!({
            "contact_key": self.key,
            "tinode_user_id": self.tinode_user_id,
            "basic_login": self.basic_login,
            "display_name": self.display_name,
        })
    }
}

#[derive(Debug, Clone)]
struct TinodeSendReceipt {
    topic: String,
    message_id: String,
    seq: Option<i64>,
}

#[derive(Debug, Clone)]
struct TinodeGroupReceipt {
    topic: String,
}

#[derive(Debug, Clone)]
struct TinodeGroupMember {
    kind: String,
    principal_id: Option<String>,
    profile_id: Option<String>,
    contact_key: Option<String>,
    tinode_user_id: String,
    basic_login: Option<String>,
    display_name: Option<String>,
}

impl TinodeGroupMember {
    fn from_contact(kind: &str, contact: TinodeContact) -> Self {
        Self {
            kind: kind.to_string(),
            principal_id: None,
            profile_id: None,
            contact_key: Some(contact.key),
            tinode_user_id: contact.tinode_user_id,
            basic_login: contact.basic_login,
            display_name: contact.display_name,
        }
    }

    fn to_resource_line(&self) -> JsonValue {
        json!({
            "kind": self.kind,
            "principal_id": self.principal_id,
            "profile_id": self.profile_id,
            "contact_key": self.contact_key,
            "tinode_user_id": self.tinode_user_id,
            "basic_login": self.basic_login,
            "display_name": self.display_name,
        })
    }
}

#[derive(Debug, Clone)]
struct TinodeGroupRecord {
    key: String,
    title: String,
    topic_id: String,
    members: Vec<TinodeGroupMember>,
    created_at_ms: u128,
    last_message_at_ms: Option<u128>,
}

impl TinodeGroupRecord {
    fn to_resource_line(&self) -> JsonValue {
        json!({
            "group_key": self.key,
            "title": self.title,
            "topic_id": self.topic_id,
            "path": format!("groups/{}", self.key),
            "members": self
                .members
                .iter()
                .map(TinodeGroupMember::to_resource_line)
                .collect::<Vec<_>>(),
            "member_count": self.members.len(),
            "created_at_ms": self.created_at_ms,
            "last_message_at_ms": self.last_message_at_ms,
        })
    }
}

#[derive(Debug, Clone)]
struct TinodeGroupMessageRecord {
    message_id: String,
    group_key: String,
    topic_id: String,
    direction: String,
    text: String,
    created_at_ms: u128,
    seq: Option<i64>,
}

impl TinodeGroupMessageRecord {
    fn outbound(group: &TinodeGroupRecord, receipt: TinodeSendReceipt, text: String) -> Self {
        Self {
            message_id: receipt.message_id,
            group_key: group.key.clone(),
            topic_id: group.topic_id.clone(),
            direction: "outbound".to_string(),
            text,
            created_at_ms: now_millis(),
            seq: receipt.seq,
        }
    }

    fn to_resource_line(&self) -> JsonValue {
        json!({
            "message_id": self.message_id,
            "conversation_type": "group",
            "group_key": self.group_key,
            "topic_id": self.topic_id,
            "direction": self.direction,
            "text": self.text,
            "created_at_ms": self.created_at_ms,
            "seq": self.seq,
        })
    }
}

#[derive(Debug, Clone)]
struct TinodeInboundMessage {
    topic: String,
    seq: i64,
    from_tinode_user_id: String,
    text: String,
}

#[derive(Debug, Clone)]
struct TinodeMessageRecord {
    message_id: String,
    contact_key: String,
    direction: String,
    text: String,
    created_at_ms: u128,
    seq: Option<i64>,
}

impl TinodeMessageRecord {
    fn to_resource_line(&self) -> JsonValue {
        json!({
            "message_id": self.message_id,
            "contact_key": self.contact_key,
            "direction": self.direction,
            "text": self.text,
            "created_at_ms": self.created_at_ms,
            "seq": self.seq,
        })
    }
}

#[derive(Debug, Clone)]
struct InboxRecord {
    message_id: String,
    contact_key: String,
    conversation_type: String,
    from_tinode_user_id: String,
    from_display_name: Option<String>,
    text: String,
    created_at_ms: u128,
    requires_attention: bool,
}

impl InboxRecord {
    fn to_resource_line(&self, unread: bool) -> JsonValue {
        json!({
            "message_id": self.message_id,
            "conversation_type": self.conversation_type,
            "contact_key": self.contact_key,
            "from_tinode_user_id": self.from_tinode_user_id,
            "from_display_name": self.from_display_name,
            "text": self.text,
            "text_preview": text_preview(&self.text),
            "created_at_ms": self.created_at_ms,
            "requires_attention": self.requires_attention,
            "unread": unread,
        })
    }

    fn to_event_content(&self, profile_id: &str) -> JsonValue {
        json!({
            "profile_id": profile_id,
            "conversation_type": self.conversation_type,
            "contact_key": self.contact_key,
            "message_id": self.message_id,
            "from_tinode_user_id": self.from_tinode_user_id,
            "from_display_name": self.from_display_name,
            "text_preview": text_preview(&self.text),
            "requires_attention": self.requires_attention,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RecipientRef {
    Basic(String),
    ContactKey(String),
    TinodeUser(String),
    Principal(String),
}

struct WebSocketTinodeGateway {
    config: TinodeConnectorConfig,
    clients: HashMap<String, TinodeWsClient>,
}

impl WebSocketTinodeGateway {
    fn new(config: TinodeConnectorConfig) -> Self {
        Self {
            config,
            clients: HashMap::new(),
        }
    }

    fn ensure_client(
        &mut self,
        credentials: &TinodeCredentials,
    ) -> std::result::Result<(), ConnectorError> {
        if credentials.tinode_user_id.is_empty()
            || credentials.login.is_empty()
            || credentials.token.is_empty()
        {
            return Err(connector_err(
                connector_error_codes::PROFILE_NOT_READY,
                "Tinode credential record is incomplete",
                false,
            ));
        }
        if !self.clients.contains_key(&credentials.profile_id) {
            let mut client = TinodeWsClient::connect(&self.config, &credentials.profile_id)?;
            client.login_with_token(credentials)?;
            self.clients.insert(credentials.profile_id.clone(), client);
        }
        Ok(())
    }

    fn with_client<T>(
        &mut self,
        credentials: &TinodeCredentials,
        mut op: impl FnMut(&mut TinodeWsClient) -> std::result::Result<T, ConnectorError>,
    ) -> std::result::Result<T, ConnectorError> {
        self.ensure_client(credentials)?;
        let profile_id = credentials.profile_id.clone();
        let first = {
            let client = self.clients.get_mut(&profile_id).ok_or_else(|| {
                connector_err(
                    connector_error_codes::INTERNAL,
                    "missing Tinode client",
                    true,
                )
            })?;
            op(client)
        };
        if let Err(err) = first {
            if is_tinode_session_reconnectable(&err) {
                self.clients.remove(&profile_id);
                self.ensure_client(credentials)?;
                let client = self.clients.get_mut(&profile_id).ok_or_else(|| {
                    connector_err(
                        connector_error_codes::INTERNAL,
                        "missing Tinode client after reconnect",
                        true,
                    )
                })?;
                return op(client);
            }
            return Err(err);
        }
        first
    }
}

impl TinodeGateway for WebSocketTinodeGateway {
    fn create_or_reuse_account(
        &mut self,
        request: TinodeAccountRequest,
    ) -> std::result::Result<TinodeAccount, ConnectorError> {
        let mut client = TinodeWsClient::connect(&self.config, &request.profile_id)?;
        let account = match client.create_account(&request) {
            Ok(account) => account,
            Err(err) if is_tinode_duplicate_credential_error(&err) => {
                client.login_with_basic_account(&request)?
            }
            Err(err) => return Err(err),
        };
        self.clients.insert(request.profile_id, client);
        Ok(account)
    }

    fn resolve_basic_user(
        &mut self,
        credentials: &TinodeCredentials,
        login: &str,
    ) -> std::result::Result<TinodeContact, ConnectorError> {
        self.with_client(credentials, |client| client.search_basic_user(login))
    }

    fn send_direct_message(
        &mut self,
        credentials: &TinodeCredentials,
        contact: &TinodeContact,
        text: &str,
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeSendReceipt, ConnectorError> {
        self.with_client(credentials, |client| {
            client.send_direct_message(contact, text, client_token)
        })
    }

    fn fetch_direct_messages(
        &mut self,
        credentials: &TinodeCredentials,
        contact: &TinodeContact,
        since_seq: Option<i64>,
    ) -> std::result::Result<Vec<TinodeInboundMessage>, ConnectorError> {
        self.with_client(credentials, |client| {
            client.fetch_direct_messages(contact, since_seq)
        })
    }

    fn create_group(
        &mut self,
        credentials: &TinodeCredentials,
        title: &str,
        members: &[TinodeGroupMember],
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeGroupReceipt, ConnectorError> {
        self.with_client(credentials, |client| {
            client.create_group(title, members, client_token)
        })
    }

    fn invite_group_members(
        &mut self,
        credentials: &TinodeCredentials,
        group: &TinodeGroupRecord,
        members: &[TinodeGroupMember],
    ) -> std::result::Result<(), ConnectorError> {
        self.with_client(credentials, |client| {
            client.invite_group_members(group, members)
        })
    }

    fn send_group_message(
        &mut self,
        credentials: &TinodeCredentials,
        group: &TinodeGroupRecord,
        text: &str,
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeSendReceipt, ConnectorError> {
        self.with_client(credentials, |client| {
            client.send_group_message(group, text, client_token)
        })
    }
}

struct TinodeWsClient {
    socket: TinodeSocket,
    profile_id: String,
    user_id: Option<String>,
    token: Option<String>,
    request_seq: u64,
}

impl TinodeWsClient {
    fn connect(
        config: &TinodeConnectorConfig,
        profile_id: &str,
    ) -> std::result::Result<Self, ConnectorError> {
        let ws_url = to_websocket_url(&config.endpoint, &config.api_key);
        let (socket, _) = connect(ws_url.as_str()).map_err(|err| {
            connector_err(
                connector_error_codes::UPSTREAM_UNAVAILABLE,
                format!("failed to connect to Tinode websocket: {err}"),
                true,
            )
        })?;
        let mut client = Self {
            socket,
            profile_id: profile_id.to_string(),
            user_id: None,
            token: None,
            request_seq: 0,
        };
        client.request(
            "hi",
            json!({
                "ver": config.protocol_version,
                "ua": "appfs-tinode-connector/0.2",
            }),
            "hi",
        )?;
        Ok(client)
    }

    fn create_account(
        &mut self,
        request: &TinodeAccountRequest,
    ) -> std::result::Result<TinodeAccount, ConnectorError> {
        let secret = BASE64_STANDARD.encode(format!("{}:{}", request.login, request.password));
        let ctrl = self.request(
            "acc",
            json!({
                "user": "new",
                "scheme": "basic",
                "secret": secret,
                "login": true,
                "tags": request.tags,
                "desc": {
                    "public": { "fn": request.display_name },
                    "defacs": {
                        "auth": "JRWPA",
                        "anon": "N"
                    }
                }
            }),
            &format!("acc-{}", request.login),
        )?;
        let params = ctrl.get("params").cloned().unwrap_or_else(|| json!({}));
        let user_id = params
            .get("user")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                connector_err(
                    connector_error_codes::UPSTREAM_UNAVAILABLE,
                    "Tinode account creation did not return params.user",
                    true,
                )
            })?;
        let token = params
            .get("token")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_default();

        self.user_id = Some(user_id.clone());
        self.token = Some(token.clone());
        self.request(
            "sub",
            json!({
                "topic": "me",
                "get": { "what": "desc sub" }
            }),
            &format!("sub-me-{}", request.login),
        )?;

        Ok(TinodeAccount {
            tinode_user_id: user_id,
            login: request.login.clone(),
            display_name: request.display_name.clone(),
            token,
        })
    }

    fn login_with_token(
        &mut self,
        credentials: &TinodeCredentials,
    ) -> std::result::Result<(), ConnectorError> {
        let suffix = self.next_suffix();
        let ctrl = self.request(
            "login",
            json!({
                "scheme": "token",
                "secret": credentials.token,
            }),
            &format!("login-token-{suffix}"),
        )?;
        let params = ctrl.get("params").cloned().unwrap_or_else(|| json!({}));
        self.user_id = Some(credentials.tinode_user_id.clone());
        self.token = params
            .get("token")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| Some(credentials.token.clone()));
        self.request(
            "sub",
            json!({
                "topic": "me",
                "get": { "what": "desc sub" },
            }),
            &format!("sub-me-token-{suffix}"),
        )?;
        Ok(())
    }

    fn login_with_basic_account(
        &mut self,
        request: &TinodeAccountRequest,
    ) -> std::result::Result<TinodeAccount, ConnectorError> {
        let secret = BASE64_STANDARD.encode(format!("{}:{}", request.login, request.password));
        let ctrl = self.request(
            "login",
            json!({
                "scheme": "basic",
                "secret": secret,
            }),
            &format!("login-basic-{}", request.login),
        )?;
        let params = ctrl.get("params").cloned().unwrap_or_else(|| json!({}));
        let user_id = params
            .get("user")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                connector_err(
                    connector_error_codes::UPSTREAM_UNAVAILABLE,
                    "Tinode basic login did not return params.user",
                    true,
                )
            })?;
        let token = params
            .get("token")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .unwrap_or_default();

        self.user_id = Some(user_id.clone());
        self.token = Some(token.clone());
        self.request(
            "sub",
            json!({
                "topic": "me",
                "get": { "what": "desc sub" },
            }),
            &format!("sub-me-basic-{}", request.login),
        )?;
        Ok(TinodeAccount {
            tinode_user_id: user_id,
            login: request.login.clone(),
            display_name: request.display_name.clone(),
            token,
        })
    }

    fn search_basic_user(
        &mut self,
        login: &str,
    ) -> std::result::Result<TinodeContact, ConnectorError> {
        let query = format!("basic:{login}");
        let suffix = self.next_suffix();
        self.request(
            "sub",
            json!({ "topic": "fnd" }),
            &format!("sub-fnd-{suffix}"),
        )?;
        self.request(
            "set",
            json!({ "topic": "fnd", "desc": { "public": query } }),
            &format!("set-fnd-{suffix}"),
        )?;
        let meta = self.request_meta(
            "get",
            json!({ "topic": "fnd", "what": "sub" }),
            &format!("get-fnd-{suffix}"),
            Some("fnd"),
        )?;
        let _ = self.request(
            "leave",
            json!({ "topic": "fnd" }),
            &format!("leave-fnd-{suffix}"),
        );

        let matches = meta
            .get("sub")
            .and_then(JsonValue::as_array)
            .cloned()
            .unwrap_or_default();
        let found = matches
            .into_iter()
            .find_map(|entry| contact_from_search_entry(login, &entry))
            .ok_or_else(|| {
                connector_err(
                    connector_error_codes::PROFILE_NOT_FOUND,
                    format!("Tinode user not found for basic:{login}"),
                    false,
                )
            })?;
        Ok(found)
    }

    fn send_direct_message(
        &mut self,
        contact: &TinodeContact,
        text: &str,
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeSendReceipt, ConnectorError> {
        let suffix = self.next_suffix();
        let topic = contact.tinode_user_id.clone();
        self.request(
            "sub",
            json!({ "topic": topic }),
            &format!("sub-direct-{suffix}"),
        )?;
        let ctrl = self.request(
            "pub",
            json!({
                "topic": contact.tinode_user_id,
                "noecho": false,
                "head": {
                    "mime": "text/plain"
                },
                "content": tinode_text_plain_content(text)
            }),
            client_token.unwrap_or(&format!("pub-direct-{suffix}")),
        )?;
        let params = ctrl.get("params").cloned().unwrap_or_else(|| json!({}));
        let seq = params.get("seq").and_then(JsonValue::as_i64);
        let message_id = seq
            .map(|seq| format!("tinode:{}:{seq}", contact.tinode_user_id))
            .unwrap_or_else(|| format!("tinode:{}:{suffix}", contact.tinode_user_id));
        Ok(TinodeSendReceipt {
            topic: contact.tinode_user_id.clone(),
            message_id,
            seq,
        })
    }

    fn fetch_direct_messages(
        &mut self,
        contact: &TinodeContact,
        since_seq: Option<i64>,
    ) -> std::result::Result<Vec<TinodeInboundMessage>, ConnectorError> {
        let suffix = self.next_suffix();
        let mut messages = self.request_data(
            "sub",
            tinode_sub_data_payload(&contact.tinode_user_id, since_seq, 50),
            &format!("sub-inbox-{suffix}"),
            &contact.tinode_user_id,
        );
        if messages.is_err() {
            let _ = self.request(
                "sub",
                json!({ "topic": contact.tinode_user_id }),
                &format!("sub-inbox-fallback-{suffix}"),
            );
            messages = Ok(Vec::new());
        }
        let payload = tinode_get_data_payload(&contact.tinode_user_id, since_seq, 50);
        let mut fetched = self.request_data(
            "get",
            payload,
            &format!("get-data-{suffix}"),
            &contact.tinode_user_id,
        )?;
        fetched.extend(messages?);
        Ok(dedupe_tinode_inbound_messages(fetched))
    }

    fn create_group(
        &mut self,
        title: &str,
        members: &[TinodeGroupMember],
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeGroupReceipt, ConnectorError> {
        let suffix = self.next_suffix();
        let ctrl = self.request(
            "sub",
            json!({
                "topic": "new",
                "set": {
                    "desc": {
                        "public": { "fn": title },
                        "defacs": {
                            "auth": "JRWPA",
                            "anon": "N"
                        }
                    },
                    "tags": [sanitize_path_key(title), "appfs-agent-group"]
                }
            }),
            client_token.unwrap_or(&format!("sub-group-{suffix}")),
        )?;
        let topic = ctrl
            .get("topic")
            .and_then(JsonValue::as_str)
            .or_else(|| {
                ctrl.get("params")
                    .and_then(|params| params.get("topic"))
                    .and_then(JsonValue::as_str)
            })
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                connector_err(
                    connector_error_codes::UPSTREAM_UNAVAILABLE,
                    "Tinode group creation did not return a topic",
                    true,
                )
            })?;
        let record = TinodeGroupRecord {
            key: sanitize_path_key(title),
            title: title.to_string(),
            topic_id: topic.clone(),
            members: Vec::new(),
            created_at_ms: now_millis(),
            last_message_at_ms: None,
        };
        self.invite_group_members(&record, members)?;
        Ok(TinodeGroupReceipt { topic })
    }

    fn invite_group_members(
        &mut self,
        group: &TinodeGroupRecord,
        members: &[TinodeGroupMember],
    ) -> std::result::Result<(), ConnectorError> {
        for member in members {
            self.request(
                "set",
                json!({
                    "topic": group.topic_id,
                    "sub": {
                        "user": member.tinode_user_id,
                        "mode": "JRWPA"
                    }
                }),
                &format!("invite-{}-{}", group.topic_id, member.tinode_user_id),
            )?;
        }
        Ok(())
    }

    fn send_group_message(
        &mut self,
        group: &TinodeGroupRecord,
        text: &str,
        client_token: Option<&str>,
    ) -> std::result::Result<TinodeSendReceipt, ConnectorError> {
        let suffix = self.next_suffix();
        let _ = self.request(
            "sub",
            json!({ "topic": group.topic_id }),
            &format!("sub-group-send-{suffix}"),
        );
        let ctrl = self.request(
            "pub",
            json!({
                "topic": group.topic_id,
                "noecho": false,
                "head": {
                    "mime": "text/plain"
                },
                "content": tinode_text_plain_content(text)
            }),
            client_token.unwrap_or(&format!("pub-group-{suffix}")),
        )?;
        let params = ctrl.get("params").cloned().unwrap_or_else(|| json!({}));
        let seq = params.get("seq").and_then(JsonValue::as_i64);
        let message_id = seq
            .map(|seq| format!("tinode:{}:{seq}", group.topic_id))
            .unwrap_or_else(|| format!("tinode:{}:{suffix}", group.topic_id));
        Ok(TinodeSendReceipt {
            topic: group.topic_id.clone(),
            message_id,
            seq,
        })
    }

    fn request(
        &mut self,
        kind: &str,
        payload: JsonValue,
        id: &str,
    ) -> std::result::Result<JsonValue, ConnectorError> {
        let mut object = payload.as_object().cloned().ok_or_else(|| {
            connector_err(
                connector_error_codes::INVALID_PAYLOAD,
                "Tinode request payload must be a JSON object",
                false,
            )
        })?;
        object.insert("id".to_string(), json!(id));
        self.send_packet(json!({ kind: object }))?;
        self.wait_for_ctrl(id, kind)
    }

    fn request_meta(
        &mut self,
        kind: &str,
        payload: JsonValue,
        id: &str,
        topic: Option<&str>,
    ) -> std::result::Result<JsonValue, ConnectorError> {
        let mut object = payload.as_object().cloned().ok_or_else(|| {
            connector_err(
                connector_error_codes::INVALID_PAYLOAD,
                "Tinode meta request payload must be a JSON object",
                false,
            )
        })?;
        object.insert("id".to_string(), json!(id));
        self.send_packet(json!({ kind: object }))?;

        let mut meta = None;
        for _ in 0..200 {
            let msg = self.read_message_json()?;
            if let Some(ctrl) = msg.get("ctrl") {
                if ctrl.get("id").and_then(JsonValue::as_str) == Some(id) {
                    let code = ctrl.get("code").and_then(JsonValue::as_i64).unwrap_or(200);
                    if code >= 400 {
                        return Err(tinode_ctrl_error(kind, ctrl));
                    }
                    return Ok(meta.unwrap_or_else(|| json!({})));
                }
            }
            if let Some(candidate) = msg.get("meta") {
                let id_matches = candidate.get("id").and_then(JsonValue::as_str) == Some(id);
                let topic_matches = topic
                    .map(|topic| candidate.get("topic").and_then(JsonValue::as_str) == Some(topic))
                    .unwrap_or(false);
                if id_matches || topic_matches {
                    meta = Some(candidate.clone());
                }
            }
        }

        meta.ok_or_else(|| {
            connector_err(
                connector_error_codes::TIMEOUT,
                format!("Tinode did not return metadata for {kind} {id}"),
                true,
            )
        })
    }

    fn request_data(
        &mut self,
        kind: &str,
        payload: JsonValue,
        id: &str,
        topic: &str,
    ) -> std::result::Result<Vec<TinodeInboundMessage>, ConnectorError> {
        let mut object = payload.as_object().cloned().ok_or_else(|| {
            connector_err(
                connector_error_codes::INVALID_PAYLOAD,
                "Tinode data request payload must be a JSON object",
                false,
            )
        })?;
        object.insert("id".to_string(), json!(id));
        self.send_packet(json!({ kind: object }))?;

        let mut messages = Vec::new();
        for _ in 0..400 {
            let msg = self.read_message_json()?;
            if let Some(data) = msg.get("data") {
                if data.get("topic").and_then(JsonValue::as_str) == Some(topic) {
                    if let Some(message) = inbound_message_from_data(data) {
                        messages.push(message);
                    }
                }
                continue;
            }
            if let Some(meta) = msg.get("meta") {
                let id_matches = meta.get("id").and_then(JsonValue::as_str) == Some(id);
                let topic_matches = meta.get("topic").and_then(JsonValue::as_str) == Some(topic);
                if id_matches || topic_matches {
                    messages.extend(inbound_messages_from_meta(meta, topic));
                }
                continue;
            }
            if let Some(ctrl) = msg.get("ctrl") {
                if ctrl.get("id").and_then(JsonValue::as_str) == Some(id) {
                    let code = ctrl.get("code").and_then(JsonValue::as_i64).unwrap_or(200);
                    if code >= 400 {
                        return Err(tinode_ctrl_error(kind, ctrl));
                    }
                    return Ok(messages);
                }
            }
        }
        Err(connector_err(
            connector_error_codes::TIMEOUT,
            format!("Tinode did not return ctrl for {kind} id={id}"),
            true,
        ))
    }

    fn wait_for_ctrl(
        &mut self,
        id: &str,
        label: &str,
    ) -> std::result::Result<JsonValue, ConnectorError> {
        for _ in 0..200 {
            let msg = self.read_message_json()?;
            let Some(ctrl) = msg.get("ctrl") else {
                continue;
            };
            let id_matches = ctrl.get("id").and_then(JsonValue::as_str) == Some(id);
            let anonymous_error = ctrl.get("code").and_then(JsonValue::as_i64).unwrap_or(200)
                >= 400
                && ctrl.get("id").is_none();
            if id_matches || anonymous_error {
                let code = ctrl.get("code").and_then(JsonValue::as_i64).unwrap_or(200);
                if code >= 400 {
                    return Err(tinode_ctrl_error(label, ctrl));
                }
                return Ok(ctrl.clone());
            }
        }
        Err(connector_err(
            connector_error_codes::TIMEOUT,
            format!("Tinode did not return ctrl for {label} id={id}"),
            true,
        ))
    }

    fn send_packet(&mut self, packet: JsonValue) -> std::result::Result<(), ConnectorError> {
        let text = serde_json::to_string(&packet).map_err(|err| {
            connector_err(
                connector_error_codes::INVALID_PAYLOAD,
                format!("failed to encode Tinode packet: {err}"),
                false,
            )
        })?;
        self.socket.send(Message::Text(text)).map_err(|err| {
            connector_err(
                connector_error_codes::UPSTREAM_UNAVAILABLE,
                format!("failed to send Tinode packet: {err}"),
                true,
            )
        })
    }

    fn read_message_json(&mut self) -> std::result::Result<JsonValue, ConnectorError> {
        loop {
            let message = self.socket.read().map_err(|err| {
                connector_err(
                    connector_error_codes::UPSTREAM_UNAVAILABLE,
                    format!("failed to read Tinode message: {err}"),
                    true,
                )
            })?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).map_err(|err| {
                        connector_err(
                            connector_error_codes::UPSTREAM_UNAVAILABLE,
                            format!("failed to parse Tinode message: {err}"),
                            true,
                        )
                    });
                }
                Message::Binary(bytes) => {
                    return serde_json::from_slice(&bytes).map_err(|err| {
                        connector_err(
                            connector_error_codes::UPSTREAM_UNAVAILABLE,
                            format!("failed to parse Tinode binary message: {err}"),
                            true,
                        )
                    });
                }
                Message::Ping(payload) => {
                    let _ = self.socket.send(Message::Pong(payload));
                }
                Message::Pong(_) => {}
                Message::Close(_) => {
                    return Err(connector_err(
                        connector_error_codes::UPSTREAM_UNAVAILABLE,
                        "Tinode websocket closed",
                        true,
                    ));
                }
                Message::Frame(_) => {}
            }
        }
    }

    fn next_suffix(&mut self) -> String {
        self.request_seq = self.request_seq.saturating_add(1);
        format!("{}-{}", self.profile_id.replace(':', "-"), self.request_seq)
    }
}

fn completed_response(ctx: &ConnectorContext, content: JsonValue) -> SubmitActionResponse {
    SubmitActionResponse {
        request_id: ctx.request_id.clone(),
        estimated_duration_ms: None,
        outcome: SubmitActionOutcome::Completed { content },
    }
}

fn dir(path: &str) -> AppStructureNode {
    AppStructureNode {
        path: path.to_string(),
        kind: AppStructureNodeKind::Directory,
        manifest_entry: None,
        seed_content: None,
        mutable: false,
        scope: None,
    }
}

fn action(path: &str) -> AppStructureNode {
    AppStructureNode {
        path: path.to_string(),
        kind: AppStructureNodeKind::ActionFile,
        manifest_entry: Some(json!({
            "template": path,
            "kind": "action",
            "input_mode": "json",
            "execution_mode": "inline",
        })),
        seed_content: None,
        mutable: true,
        scope: None,
    }
}

fn static_json(path: &str, seed_content: JsonValue) -> AppStructureNode {
    AppStructureNode {
        path: path.to_string(),
        kind: AppStructureNodeKind::StaticJsonResource,
        manifest_entry: None,
        seed_content: Some(seed_content),
        mutable: false,
        scope: None,
    }
}

fn snapshot_jsonl(path: &str) -> AppStructureNode {
    AppStructureNode {
        path: path.to_string(),
        kind: AppStructureNodeKind::SnapshotResource,
        manifest_entry: Some(json!({
            "template": path,
            "kind": "resource",
            "output_mode": "jsonl",
            "snapshot": {
                "max_materialized_bytes": 1048576,
                "prewarm": true,
                "prewarm_timeout_ms": 5000,
                "read_through_timeout_ms": 10000,
                "on_timeout": "return_stale"
            }
        })),
        seed_content: None,
        mutable: false,
        scope: None,
    }
}

fn is_tinode_snapshot_resource(path: &str) -> bool {
    let normalized = normalize_path(path);
    matches!(
        normalized.as_str(),
        "contacts/index.res.jsonl"
            | "contacts/search_results.res.jsonl"
            | "groups/index.res.jsonl"
            | "inbox/recent.res.jsonl"
            | "inbox/unread.res.jsonl"
            | "topics/index.res.jsonl"
    ) || contact_messages_key(&normalized).is_some()
        || group_messages_key(&normalized).is_some()
}

fn tinode_snapshot_read_should_refresh_inbound(normalized_path: &str) -> bool {
    matches!(
        normalized_path,
        "inbox/recent.res.jsonl" | "inbox/unread.res.jsonl"
    ) || contact_messages_key(normalized_path).is_some()
}

fn contact_send_message_key(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("contacts"), Some(key), Some("send_message.act"), None) if !key.is_empty() => {
            Some(key)
        }
        _ => None,
    }
}

fn contact_messages_key(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("contacts"), Some(key), Some("messages.res.jsonl"), None) if !key.is_empty() => {
            Some(key)
        }
        _ => None,
    }
}

fn group_send_message_key(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("groups"), Some(key), Some("send_message.act"), None) if !key.is_empty() => Some(key),
        _ => None,
    }
}

fn group_invite_members_key(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("groups"), Some(key), Some("invite_members.act"), None) if !key.is_empty() => {
            Some(key)
        }
        _ => None,
    }
}

fn group_messages_key(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("groups"), Some(key), Some("messages.res.jsonl"), None) if !key.is_empty() => {
            Some(key)
        }
        _ => None,
    }
}

fn normalize_path(path: &str) -> String {
    path.trim_start_matches('/')
        .replace('\\', "/")
        .trim()
        .to_string()
}

fn is_safe_login_prefix(prefix: &str) -> bool {
    !prefix.is_empty()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn effective_profile_id(ctx: &ConnectorContext) -> std::result::Result<String, ConnectorError> {
    ctx.profile_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            connector_err(
                connector_error_codes::PROFILE_NOT_READY,
                "Tinode private app requires profile_id in connector context",
                false,
            )
        })
}

fn login_for_profile(
    config: &TinodeConnectorConfig,
    principal_id: &str,
    profile_id: &str,
) -> String {
    let raw = format!("{}_{}", config.login_prefix, principal_id);
    let sanitized = sanitize_tinode_login(&raw);
    if sanitized.len() >= TINODE_LOGIN_MIN_LEN {
        return constrain_tinode_login(&sanitized);
    }
    constrain_tinode_login(&sanitize_tinode_login(&format!(
        "{}_{}",
        config.login_prefix,
        profile_id.replace(':', "_")
    )))
}

fn shared_state_namespace(config: &TinodeConnectorConfig) -> String {
    format!("{}|{}", config.endpoint, config.login_prefix)
}

fn shared_credential_key(namespace: &str, profile_id: &str) -> String {
    format!("{namespace}|profile:{profile_id}")
}

fn shared_principal_key(namespace: &str, principal_id: &str) -> String {
    format!("{namespace}|principal:{principal_id}")
}

fn sanitize_tinode_login(value: &str) -> String {
    let mut out = value
        .to_ascii_lowercase()
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
                Some(ch)
            } else if ch == '-' {
                Some('_')
            } else {
                None
            }
        })
        .collect::<String>();
    while out.starts_with(['_', '.']) {
        out.remove(0);
    }
    while out.ends_with(['_', '.']) {
        out.pop();
    }
    if out.is_empty() {
        out = "appfsagent".to_string();
    }
    out
}

fn constrain_tinode_login(value: &str) -> String {
    let mut login = value.to_string();
    if login.len() > TINODE_LOGIN_MAX_LEN {
        let hash = stable_login_hash(&login);
        let prefix_len = TINODE_LOGIN_MAX_LEN
            .saturating_sub(1)
            .saturating_sub(TINODE_LOGIN_HASH_LEN);
        let mut prefix = login.chars().take(prefix_len).collect::<String>();
        while prefix.ends_with(['_', '.']) {
            prefix.pop();
        }
        if prefix.len() < TINODE_LOGIN_MIN_LEN {
            prefix = "appfsagent".to_string();
        }
        login = format!("{prefix}_{hash}");
    }
    while login.len() < TINODE_LOGIN_MIN_LEN {
        login.push('0');
    }
    login
}

fn stable_login_hash(value: &str) -> String {
    let mut hash = 0x811c9dc5_u32;
    for byte in value.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

fn display_name_for_principal(principal_id: &str) -> String {
    if principal_id.trim().is_empty() {
        "AppFS Agent".to_string()
    } else {
        format!("AppFS Agent {principal_id}")
    }
}

fn credentials_from_record(
    record: &ConnectorCredentialRecord,
) -> std::result::Result<TinodeCredentials, ConnectorError> {
    let tinode_user_id = record.upstream_user_id.clone().ok_or_else(|| {
        connector_err(
            connector_error_codes::PROFILE_NOT_READY,
            "Tinode credential record has no upstream user id",
            false,
        )
    })?;
    let login = record.login.clone().ok_or_else(|| {
        connector_err(
            connector_error_codes::PROFILE_NOT_READY,
            "Tinode credential record has no login",
            false,
        )
    })?;
    let token = record
        .credentials
        .as_ref()
        .and_then(|value| value.get("token"))
        .and_then(JsonValue::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            connector_err(
                connector_error_codes::PROFILE_NOT_READY,
                "Tinode credential record has no token",
                false,
            )
        })?;
    Ok(TinodeCredentials {
        profile_id: record.profile_id.clone(),
        tinode_user_id,
        login,
        token,
    })
}

fn recipient_ref_from_payload(
    payload: &JsonValue,
) -> std::result::Result<RecipientRef, ConnectorError> {
    let value = payload
        .get("to")
        .or_else(|| payload.get("query"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode recipient payload requires non-empty `to` or `query`",
                false,
            )
        })?;
    parse_recipient_ref(value)
}

fn message_target_and_text(
    normalized_path: &str,
    payload: &JsonValue,
) -> std::result::Result<(RecipientRef, String), ConnectorError> {
    let text = text_from_payload(payload, "Tinode send_message")?;

    let reference = if normalized_path == "contacts/send_message.act" {
        recipient_ref_from_payload(payload)?
    } else if let Some(key) = contact_send_message_key(normalized_path) {
        RecipientRef::ContactKey(key.to_string())
    } else {
        return Err(connector_err(
            connector_error_codes::NOT_SUPPORTED,
            format!("unknown Tinode send_message path: {normalized_path}"),
            false,
        ));
    };

    Ok((reference, text))
}

fn text_from_payload(
    payload: &JsonValue,
    label: &str,
) -> std::result::Result<String, ConnectorError> {
    payload
        .get("text")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                format!("{label} payload requires non-empty `text`"),
                false,
            )
        })
}

fn group_title_from_payload(payload: &JsonValue) -> std::result::Result<String, ConnectorError> {
    payload
        .get("title")
        .or_else(|| payload.get("display_name"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode create_group payload requires non-empty `title` or `display_name`",
                false,
            )
        })
}

fn group_key_from_payload(payload: &JsonValue, title: &str) -> String {
    payload
        .get("group_key")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(sanitize_path_key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| sanitize_path_key(title))
}

fn group_member_refs_from_payload(
    payload: &JsonValue,
) -> std::result::Result<Vec<RecipientRef>, ConnectorError> {
    let Some(members) = payload.get("members").and_then(JsonValue::as_array) else {
        return Ok(Vec::new());
    };
    members
        .iter()
        .map(|member| {
            let value = member.as_str().ok_or_else(|| {
                connector_err(
                    connector_error_codes::INVALID_ARGUMENT,
                    "Tinode group members must be strings",
                    false,
                )
            })?;
            parse_recipient_ref(value)
        })
        .collect()
}

fn parse_recipient_ref(value: &str) -> std::result::Result<RecipientRef, ConnectorError> {
    let value = value.trim();
    if let Some(login) = value.strip_prefix("basic:") {
        let login = login.trim();
        if login.is_empty() {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode basic recipient cannot be empty",
                false,
            ));
        }
        return Ok(RecipientRef::Basic(login.to_string()));
    }
    if let Some(user_id) = value.strip_prefix("tinode_user:") {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode user recipient cannot be empty",
                false,
            ));
        }
        return Ok(RecipientRef::TinodeUser(user_id.to_string()));
    }
    if let Some(principal_id) = value.strip_prefix("principal:") {
        let principal_id = principal_id.trim();
        if !is_safe_principal_ref(principal_id) {
            return Err(connector_err(
                connector_error_codes::INVALID_ARGUMENT,
                "Tinode principal recipient must use a safe AppFS principal id",
                false,
            ));
        }
        return Ok(RecipientRef::Principal(principal_id.to_string()));
    }
    Err(connector_err(
        connector_error_codes::INVALID_ARGUMENT,
        "Tinode v0 supports explicit recipients in the form basic:<login>, tinode_user:<usr-id>, or principal:<principal-id>",
        false,
    ))
}

fn contact_key_from_basic_login(login: &str) -> String {
    sanitize_path_key(login)
}

fn contact_key_from_tinode_user(user_id: &str) -> String {
    sanitize_path_key(user_id)
}

fn is_safe_principal_ref(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn sanitize_path_key(value: &str) -> String {
    let mut out = value
        .to_ascii_lowercase()
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
                Some(ch)
            } else {
                None
            }
        })
        .collect::<String>();
    if out.is_empty() {
        out = "contact".to_string();
    }
    out
}

fn contact_from_search_entry(login: &str, entry: &JsonValue) -> Option<TinodeContact> {
    let user_id = entry
        .get("topic")
        .or_else(|| entry.get("user"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let display_name = entry
        .get("public")
        .and_then(|value| value.get("fn"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    Some(TinodeContact {
        key: contact_key_from_basic_login(login),
        tinode_user_id: user_id,
        basic_login: Some(login.to_string()),
        display_name,
    })
}

fn inbound_message_from_data(data: &JsonValue) -> Option<TinodeInboundMessage> {
    let topic = data
        .get("topic")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let seq = data.get("seq").and_then(JsonValue::as_i64)?;
    let from_tinode_user_id = data
        .get("from")
        .or_else(|| data.get("from_user_id"))
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_string();
    let text = tinode_text_from_content(data.get("content")?)?
        .trim()
        .to_string();
    if text.is_empty() {
        return None;
    }
    Some(TinodeInboundMessage {
        topic,
        seq,
        from_tinode_user_id,
        text,
    })
}

fn tinode_text_from_content(content: &JsonValue) -> Option<&str> {
    content.as_str().or_else(|| {
        content
            .get("txt")
            .or_else(|| content.get("text"))
            .and_then(JsonValue::as_str)
    })
}

fn tinode_text_plain_content(text: &str) -> JsonValue {
    json!(text)
}

fn tinode_get_data_options(since_seq: Option<i64>, limit: i64) -> JsonValue {
    let mut data = json!({ "limit": limit });
    if let Some(since_seq) = since_seq {
        data["since"] = json!(since_seq.saturating_add(1));
    }
    data
}

fn tinode_get_data_payload(topic: &str, since_seq: Option<i64>, limit: i64) -> JsonValue {
    json!({
        "topic": topic,
        "what": "data",
        "data": tinode_get_data_options(since_seq, limit),
    })
}

fn tinode_sub_data_payload(topic: &str, since_seq: Option<i64>, limit: i64) -> JsonValue {
    json!({
        "topic": topic,
        "get": {
            "what": "data",
            "data": tinode_get_data_options(since_seq, limit),
        },
    })
}

fn inbound_messages_from_meta(meta: &JsonValue, fallback_topic: &str) -> Vec<TinodeInboundMessage> {
    let topic = meta
        .get("topic")
        .and_then(JsonValue::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_topic);
    meta.get("data")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            if entry.get("topic").is_some() {
                inbound_message_from_data(entry)
            } else {
                let mut entry = entry.clone();
                if let Some(object) = entry.as_object_mut() {
                    object.insert("topic".to_string(), json!(topic));
                }
                inbound_message_from_data(&entry)
            }
        })
        .collect()
}

fn dedupe_tinode_inbound_messages(
    messages: Vec<TinodeInboundMessage>,
) -> Vec<TinodeInboundMessage> {
    let mut seen = HashSet::new();
    let mut deduped = messages
        .into_iter()
        .filter(|message| seen.insert((message.topic.clone(), message.seq)))
        .collect::<Vec<_>>();
    deduped.sort_by(|left, right| {
        left.topic
            .cmp(&right.topic)
            .then_with(|| left.seq.cmp(&right.seq))
    });
    deduped
}

fn add_side_event(
    content: &mut JsonValue,
    event_type: &str,
    event_content: Option<JsonValue>,
    include: bool,
) {
    if !include {
        return;
    }
    let Some(object) = content.as_object_mut() else {
        return;
    };
    let events = object
        .entry(CONNECTOR_SIDE_EVENTS_FIELD)
        .or_insert_with(|| JsonValue::Array(Vec::new()));
    let Some(events) = events.as_array_mut() else {
        return;
    };
    let mut event = JsonMap::new();
    event.insert("type".to_string(), json!(event_type));
    if let Some(content) = event_content {
        event.insert("content".to_string(), content);
    }
    events.push(JsonValue::Object(event));
}

fn add_side_event_with_path(
    content: &mut JsonValue,
    event_type: &str,
    event_path: &str,
    event_content: Option<JsonValue>,
    include: bool,
) {
    if !include {
        return;
    }
    let Some(object) = content.as_object_mut() else {
        return;
    };
    let events = object
        .entry(CONNECTOR_SIDE_EVENTS_FIELD)
        .or_insert_with(|| JsonValue::Array(Vec::new()));
    let Some(events) = events.as_array_mut() else {
        return;
    };
    let mut event = JsonMap::new();
    event.insert("type".to_string(), json!(event_type));
    event.insert("path".to_string(), json!(event_path));
    if let Some(content) = event_content {
        event.insert("content".to_string(), content);
    }
    events.push(JsonValue::Object(event));
}

fn text_preview(text: &str) -> String {
    const MAX_CHARS: usize = 80;
    let mut preview = text.chars().take(MAX_CHARS).collect::<String>();
    if text.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn now_millis_string() -> String {
    now_millis().to_string()
}

fn credential_status_label(status: ConnectorCredentialStatus) -> &'static str {
    match status {
        ConnectorCredentialStatus::Missing => "missing",
        ConnectorCredentialStatus::Ready => "ready",
        ConnectorCredentialStatus::Expired => "expired",
        ConnectorCredentialStatus::Failed => "failed",
    }
}

fn to_websocket_url(endpoint: &str, api_key: &str) -> String {
    let base = endpoint.trim_end_matches('/');
    let ws_base = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws_base}/v0/channels?apikey={api_key}")
}

fn tinode_ctrl_error(label: &str, ctrl: &JsonValue) -> ConnectorError {
    let text = ctrl
        .get("text")
        .and_then(JsonValue::as_str)
        .unwrap_or("Tinode request failed");
    let code = ctrl.get("code").and_then(JsonValue::as_i64).unwrap_or(500);
    let credential_creation_server_error = label == "acc" && code >= 500;
    let appfs_code = if code == 401 || code == 403 {
        connector_error_codes::AUTH_EXPIRED
    } else if code == 404 {
        connector_error_codes::PROFILE_NOT_FOUND
    } else if code == 429 {
        connector_error_codes::RATE_LIMITED
    } else if credential_creation_server_error {
        connector_error_codes::CREDENTIALS_FAILED
    } else if code >= 500 {
        connector_error_codes::UPSTREAM_UNAVAILABLE
    } else {
        connector_error_codes::CREDENTIALS_FAILED
    };
    connector_err(
        appfs_code,
        format!("{label} failed in Tinode: code={code} text={text}"),
        (code >= 500 || code == 429) && !credential_creation_server_error,
    )
}

fn is_tinode_duplicate_credential_error(err: &ConnectorError) -> bool {
    err.code == connector_error_codes::CREDENTIALS_FAILED
        && err.message.contains("code=409")
        && err
            .message
            .to_ascii_lowercase()
            .contains("duplicate credential")
}

fn is_tinode_session_reconnectable(err: &ConnectorError) -> bool {
    err.retryable
        && (err.code == connector_error_codes::UPSTREAM_UNAVAILABLE
            || err.code == connector_error_codes::TIMEOUT)
}

fn connector_err(code: &str, message: impl Into<String>, retryable: bool) -> ConnectorError {
    ConnectorError {
        code: code.to_string(),
        message: message.into(),
        retryable,
        details: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TinodeAccount, TinodeAccountRequest, TinodeConnector, TinodeConnectorConfig, TinodeContact,
        TinodeCredentials, TinodeGateway, TinodeGroupMember, TinodeGroupReceipt, TinodeGroupRecord,
        TinodeInboundMessage, TinodeSendReceipt, CONNECTOR_SIDE_EVENTS_FIELD,
    };
    use crate::{
        connector_error_codes, ActionExecutionMode, AppConnector, AppStructureSyncResult,
        ConnectorContext, ConnectorError, FetchSnapshotChunkRequest, GetAppStructureRequest,
        SnapshotResume, SubmitActionOutcome, SubmitActionRequest,
    };
    use serde_json::{json, Value as JsonValue};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    static TEST_PREFIX_SEQ: AtomicUsize = AtomicUsize::new(0);

    fn config() -> TinodeConnectorConfig {
        let seq = TEST_PREFIX_SEQ.fetch_add(1, Ordering::Relaxed);
        TinodeConnectorConfig::new(
            "http://127.0.0.1:6060",
            "auto-create",
            format!("appfs{seq}"),
        )
        .expect("tinode config")
    }

    fn ctx() -> ConnectorContext {
        ConnectorContext {
            app_id: "tinode".to_string(),
            session_id: "sess-1".to_string(),
            request_id: "req-1".to_string(),
            client_token: Some("client-1".to_string()),
            trace_id: None,
            principal_id: Some("default".to_string()),
            profile_id: Some("tinode:default".to_string()),
        }
    }

    #[derive(Default)]
    struct MockGatewayState {
        created: Vec<TinodeAccountRequest>,
        resolved: Vec<String>,
        sent: Vec<(String, String, Option<String>)>,
        groups_created: Vec<(String, Vec<String>, Option<String>)>,
        group_invites: Vec<(String, Vec<String>)>,
        group_sent: Vec<(String, String, Option<String>)>,
        inbound: Vec<TinodeInboundMessage>,
        fetched_since: Vec<Option<i64>>,
        fail_resolve: bool,
    }

    struct MockTinodeGateway {
        state: Arc<Mutex<MockGatewayState>>,
    }

    impl MockTinodeGateway {
        fn new(state: Arc<Mutex<MockGatewayState>>) -> Self {
            Self { state }
        }
    }

    impl TinodeGateway for MockTinodeGateway {
        fn create_or_reuse_account(
            &mut self,
            request: TinodeAccountRequest,
        ) -> std::result::Result<TinodeAccount, ConnectorError> {
            self.state
                .lock()
                .expect("mock state")
                .created
                .push(request.clone());
            Ok(TinodeAccount {
                tinode_user_id: format!("usr-{}", request.login),
                login: request.login,
                display_name: request.display_name,
                token: "secret-token".to_string(),
            })
        }

        fn resolve_basic_user(
            &mut self,
            _credentials: &TinodeCredentials,
            login: &str,
        ) -> std::result::Result<TinodeContact, ConnectorError> {
            let mut state = self.state.lock().expect("mock state");
            state.resolved.push(login.to_string());
            if state.fail_resolve {
                return Err(ConnectorError {
                    code: connector_error_codes::PROFILE_NOT_FOUND.to_string(),
                    message: format!("not found: basic:{login}"),
                    retryable: false,
                    details: None,
                });
            }
            Ok(TinodeContact {
                key: login.to_string(),
                tinode_user_id: format!("usr-{login}"),
                basic_login: Some(login.to_string()),
                display_name: Some(format!("User {login}")),
            })
        }

        fn send_direct_message(
            &mut self,
            _credentials: &TinodeCredentials,
            contact: &TinodeContact,
            text: &str,
            client_token: Option<&str>,
        ) -> std::result::Result<TinodeSendReceipt, ConnectorError> {
            self.state.lock().expect("mock state").sent.push((
                contact.tinode_user_id.clone(),
                text.to_string(),
                client_token.map(ToOwned::to_owned),
            ));
            Ok(TinodeSendReceipt {
                topic: contact.tinode_user_id.clone(),
                message_id: format!("tinode:{}:1", contact.tinode_user_id),
                seq: Some(1),
            })
        }

        fn fetch_direct_messages(
            &mut self,
            _credentials: &TinodeCredentials,
            _contact: &TinodeContact,
            since_seq: Option<i64>,
        ) -> std::result::Result<Vec<TinodeInboundMessage>, ConnectorError> {
            let mut state = self.state.lock().expect("mock state");
            state.fetched_since.push(since_seq);
            let since_seq = since_seq.unwrap_or(0);
            Ok(state
                .inbound
                .iter()
                .filter(|message| message.seq > since_seq)
                .cloned()
                .collect())
        }

        fn create_group(
            &mut self,
            _credentials: &TinodeCredentials,
            title: &str,
            members: &[TinodeGroupMember],
            client_token: Option<&str>,
        ) -> std::result::Result<TinodeGroupReceipt, ConnectorError> {
            self.state.lock().expect("mock state").groups_created.push((
                title.to_string(),
                members
                    .iter()
                    .map(|member| member.tinode_user_id.clone())
                    .collect(),
                client_token.map(ToOwned::to_owned),
            ));
            Ok(TinodeGroupReceipt {
                topic: format!("grp-{}", super::sanitize_path_key(title)),
            })
        }

        fn invite_group_members(
            &mut self,
            _credentials: &TinodeCredentials,
            group: &TinodeGroupRecord,
            members: &[TinodeGroupMember],
        ) -> std::result::Result<(), ConnectorError> {
            self.state.lock().expect("mock state").group_invites.push((
                group.topic_id.clone(),
                members
                    .iter()
                    .map(|member| member.tinode_user_id.clone())
                    .collect(),
            ));
            Ok(())
        }

        fn send_group_message(
            &mut self,
            _credentials: &TinodeCredentials,
            group: &TinodeGroupRecord,
            text: &str,
            client_token: Option<&str>,
        ) -> std::result::Result<TinodeSendReceipt, ConnectorError> {
            self.state.lock().expect("mock state").group_sent.push((
                group.topic_id.clone(),
                text.to_string(),
                client_token.map(ToOwned::to_owned),
            ));
            Ok(TinodeSendReceipt {
                topic: group.topic_id.clone(),
                message_id: format!("tinode:{}:1", group.topic_id),
                seq: Some(1),
            })
        }
    }

    fn connector_with_mock(state: Arc<Mutex<MockGatewayState>>) -> TinodeConnector {
        TinodeConnector::new_with_gateway(config(), Box::new(MockTinodeGateway::new(state)))
    }

    fn connector_with_mock_config(
        config: TinodeConnectorConfig,
        state: Arc<Mutex<MockGatewayState>>,
    ) -> TinodeConnector {
        TinodeConnector::new_with_gateway(config, Box::new(MockTinodeGateway::new(state)))
    }

    fn completed_content(response: crate::SubmitActionResponse) -> JsonValue {
        let SubmitActionOutcome::Completed { content } = response.outcome else {
            panic!("expected completed response");
        };
        content
    }

    #[test]
    fn tinode_config_requires_endpoint_policy_and_safe_prefix() {
        assert!(TinodeConnectorConfig::new("", "auto-create", "appfs").is_err());
        assert!(TinodeConnectorConfig::new("ws://127.0.0.1:6060", "auto-create", "appfs").is_err());
        assert!(TinodeConnectorConfig::new("http://127.0.0.1:6060", "", "appfs").is_err());
        assert!(TinodeConnectorConfig::new("http://127.0.0.1:6060", "manual", "appfs").is_err());
        assert!(
            TinodeConnectorConfig::new("http://127.0.0.1:6060", "auto-create", "bad prefix")
                .is_err()
        );
    }

    #[test]
    fn tinode_generated_basic_login_respects_server_policy() {
        let config = TinodeConnectorConfig::new(
            "http://127.0.0.1:6060",
            "auto-create",
            "appfsmanual20260507082331",
        )
        .expect("tinode config");

        let default_login = super::login_for_profile(&config, "default", "tinode:default");
        let code_login =
            super::login_for_profile(&config, "code-implementer", "tinode:code-implementer");

        assert!(default_login.len() <= 26, "{default_login}");
        assert!(code_login.len() <= 26, "{code_login}");
        assert_ne!(default_login, code_login);
        for login in [default_login, code_login] {
            assert!(login.len() >= 4, "{login}");
            assert!(login
                .chars()
                .next()
                .is_some_and(|ch| ch.is_ascii_alphanumeric()));
            assert!(login
                .chars()
                .last()
                .is_some_and(|ch| ch.is_ascii_alphanumeric()));
            assert!(login
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '.'));
        }
    }

    #[test]
    fn tinode_account_server_error_is_terminal_credentials_failure() {
        let err = super::tinode_ctrl_error(
            "acc",
            &json!({
                "code": 500,
                "text": "internal error"
            }),
        );

        assert_eq!(err.code, connector_error_codes::CREDENTIALS_FAILED);
        assert!(!err.retryable);
    }

    #[test]
    fn tinode_duplicate_credential_can_be_reused_by_basic_login() {
        let err = super::tinode_ctrl_error(
            "acc",
            &json!({
                "code": 409,
                "text": "duplicate credential"
            }),
        );

        assert_eq!(err.code, connector_error_codes::CREDENTIALS_FAILED);
        assert!(super::is_tinode_duplicate_credential_error(&err));
    }

    #[test]
    fn tinode_structure_exposes_safe_skeleton_without_credentials() {
        let mut connector = TinodeConnector::new(config());
        let response = connector
            .get_app_structure(
                GetAppStructureRequest {
                    app_id: "tinode".to_string(),
                    known_revision: None,
                },
                &ctx(),
            )
            .expect("structure");
        let AppStructureSyncResult::Snapshot { snapshot } = response.result else {
            panic!("expected snapshot");
        };
        assert_eq!(connector.credential_create_attempts(), 0);
        let paths = snapshot
            .nodes
            .iter()
            .map(|node| node.path.as_str())
            .collect::<Vec<_>>();
        for expected in [
            "_app/actions.res.json",
            "_app/control.res.json",
            "_app/self.res.json",
            "_app/skill.res.json",
            "_app/ensure_credentials.act",
            "_app/refresh_structure.act",
            "_app/refresh_inbox.act",
            "contacts/index.res.jsonl",
            "contacts/send_message.act",
            "contacts/resolve.act",
            "contacts/search_results.res.jsonl",
            "groups/index.res.jsonl",
            "groups/create_group.act",
            "inbox/recent.res.jsonl",
            "inbox/unread.res.jsonl",
            "inbox/mark_read.act",
            "topics/index.res.jsonl",
        ] {
            assert!(paths.contains(&expected), "missing {expected}");
        }
        assert!(!paths.contains(&"_stream/events.evt.jsonl"));
        let skill_doc = snapshot
            .nodes
            .iter()
            .find(|node| node.path == "_app/skill.res.json")
            .and_then(|node| node.seed_content.as_ref())
            .expect("tinode skill seed content");
        assert_eq!(skill_doc["app_id"], "tinode");
        assert_eq!(
            skill_doc["description"],
            "Tinode private chat app for the current AppFS principal."
        );
        let actions_doc = snapshot
            .nodes
            .iter()
            .find(|node| node.path == "_app/actions.res.json")
            .and_then(|node| node.seed_content.as_ref())
            .expect("tinode actions seed content");
        assert!(actions_doc["recommended_actions"]
            .as_array()
            .expect("recommended actions")
            .iter()
            .any(|action| action["path"] == "contacts/send_message.act"));
    }

    #[test]
    fn tinode_self_resource_is_safe_and_supports_non_ascii_principal() {
        let connector = TinodeConnector::new(config());
        let mut ctx = ctx();
        ctx.principal_id = Some("zhangsan-agent".to_string());
        ctx.profile_id = Some("tinode:zhangsan-agent".to_string());
        let self_doc = connector.self_resource(&ctx);
        assert_eq!(self_doc["credential_status"], "missing");
        assert_eq!(self_doc["principal_id"], "zhangsan-agent");
        assert_eq!(self_doc["profile_id"], "tinode:zhangsan-agent");
        let rendered = self_doc.to_string();
        for forbidden in ["token", "refresh_token", "password", "secret", "cookie"] {
            assert!(!rendered.contains(forbidden));
        }
    }

    #[test]
    fn tinode_snapshot_resources_are_empty_before_credentials() {
        let mut connector = TinodeConnector::new(config());
        let response = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/contacts/index.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("empty snapshot");
        assert!(response.records.is_empty());
        assert!(!response.has_more);
    }

    #[test]
    fn root_send_message_auto_creates_credentials_and_sends_direct_message() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(Arc::clone(&state));
        let content = completed_content(
            connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/contacts/send_message.act".to_string(),
                        payload: json!({"to":"basic:zhangsan","text":"hello"}),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("send message"),
        );

        assert_eq!(connector.credential_create_attempts(), 1);
        assert_eq!(content["ok"], true);
        assert_eq!(content["profile_id"], "tinode:default");
        assert_eq!(content["text_preview"], "hello");
        let events = content
            .get(CONNECTOR_SIDE_EVENTS_FIELD)
            .and_then(JsonValue::as_array)
            .expect("side events");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["type"], "action.accepted");
        assert_eq!(events[1]["type"], "profile.credentials.ready");
        assert_eq!(events[2]["type"], "message.sent");
        assert!(!content.to_string().contains("secret-token"));

        let state = state.lock().expect("mock state");
        assert_eq!(state.created.len(), 1);
        assert_eq!(state.resolved, vec!["zhangsan"]);
        assert_eq!(state.sent.len(), 1);
        assert_eq!(state.sent[0].1, "hello");
    }

    #[test]
    fn root_send_message_reuses_existing_credentials() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(Arc::clone(&state));
        for text in ["one", "two"] {
            connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/contacts/send_message.act".to_string(),
                        payload: json!({"to":"basic:zhangsan","text":text}),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("send message");
        }

        assert_eq!(connector.credential_create_attempts(), 1);
        let state = state.lock().expect("mock state");
        assert_eq!(state.created.len(), 1);
        assert_eq!(state.sent.len(), 2);
    }

    #[test]
    fn action_payload_cannot_override_effective_profile_id() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(state);
        let content = completed_content(
            connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/contacts/send_message.act".to_string(),
                        payload: json!({
                            "to":"basic:zhangsan",
                            "text":"hello",
                            "profile_id":"tinode:attacker"
                        }),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("send message"),
        );

        assert_eq!(content["profile_id"], "tinode:default");
        assert!(connector.credentials.contains_key("tinode:default"));
        assert!(!connector.credentials.contains_key("tinode:attacker"));
        assert!(!content.to_string().contains("tinode:attacker"));
    }

    #[test]
    fn bad_recipient_returns_useful_failure() {
        let state = Arc::new(Mutex::new(MockGatewayState {
            fail_resolve: true,
            ..MockGatewayState::default()
        }));
        let mut connector = connector_with_mock(state);
        let err = connector
            .submit_action(
                SubmitActionRequest {
                    path: "/contacts/send_message.act".to_string(),
                    payload: json!({"to":"basic:missing","text":"hello"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect_err("recipient should fail");
        assert_eq!(err.code, connector_error_codes::PROFILE_NOT_FOUND);
        assert!(err.message.contains("basic:missing"));
    }

    #[test]
    fn malformed_send_payload_does_not_create_credentials() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(Arc::clone(&state));
        let err = connector
            .submit_action(
                SubmitActionRequest {
                    path: "/contacts/send_message.act".to_string(),
                    payload: json!({"text":"hello"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect_err("missing recipient should fail before upstream auth");

        assert_eq!(err.code, connector_error_codes::INVALID_ARGUMENT);
        assert_eq!(connector.credential_create_attempts(), 0);
        assert!(state.lock().expect("mock state").created.is_empty());
    }

    #[test]
    fn contact_index_and_messages_are_safe_snapshot_resources_after_send() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(state);
        connector
            .submit_action(
                SubmitActionRequest {
                    path: "/contacts/send_message.act".to_string(),
                    payload: json!({"to":"basic:zhangsan","text":"hello"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("send message");

        let contacts = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/contacts/index.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("contacts");
        assert_eq!(contacts.records.len(), 1);
        assert_eq!(contacts.records[0].line["basic_login"], "zhangsan");

        let messages = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/contacts/zhangsan/messages.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("messages");
        assert_eq!(messages.records.len(), 1);
        assert_eq!(messages.records[0].line["text"], "hello");
        assert!(!messages.records[0]
            .line
            .to_string()
            .contains("secret-token"));
    }

    #[test]
    fn inbound_direct_messages_become_events_and_inbox_records() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(Arc::clone(&state));
        connector
            .submit_action(
                SubmitActionRequest {
                    path: "/contacts/send_message.act".to_string(),
                    payload: json!({"to":"basic:zhangsan","text":"outbound"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("seed contact");
        state
            .lock()
            .expect("mock state")
            .inbound
            .push(TinodeInboundMessage {
                topic: "usr-zhangsan".to_string(),
                seq: 2,
                from_tinode_user_id: "usr-zhangsan".to_string(),
                text: "reply from user".to_string(),
            });

        let events = connector
            .drain_inbound_events(&ctx())
            .expect("drain inbound");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "message.received");
        assert_eq!(events[0].path, "contacts/zhangsan/messages.res.jsonl");
        assert_eq!(
            events[0]
                .content
                .as_ref()
                .and_then(|value| value.get("text_preview"))
                .and_then(JsonValue::as_str),
            Some("reply from user")
        );
        assert_eq!(events[1].event_type, "inbox.updated");

        let unread = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/inbox/unread.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("unread inbox");
        assert_eq!(unread.records.len(), 1);
        assert_eq!(unread.records[0].line["text"], "reply from user");
        assert_eq!(unread.records[0].line["unread"], true);

        let messages = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/contacts/zhangsan/messages.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("messages");
        assert_eq!(messages.records.len(), 2);
        assert_eq!(messages.records[1].line["direction"], "inbound");

        let duplicate = connector
            .drain_inbound_events(&ctx())
            .expect("second drain");
        assert!(duplicate.is_empty());
    }

    #[test]
    fn refresh_inbox_action_returns_side_events_for_inbound_messages() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(Arc::clone(&state));
        connector
            .submit_action(
                SubmitActionRequest {
                    path: "/contacts/send_message.act".to_string(),
                    payload: json!({"to":"basic:zhangsan","text":"outbound"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("seed contact");
        state
            .lock()
            .expect("mock state")
            .inbound
            .push(TinodeInboundMessage {
                topic: "usr-zhangsan".to_string(),
                seq: 2,
                from_tinode_user_id: "usr-zhangsan".to_string(),
                text: "refresh reply".to_string(),
            });

        let content = completed_content(
            connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/_app/refresh_inbox.act".to_string(),
                        payload: json!({}),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("refresh inbox"),
        );
        assert_eq!(content["event_count"], 2);
        let side_events = content
            .get(CONNECTOR_SIDE_EVENTS_FIELD)
            .and_then(JsonValue::as_array)
            .expect("side events");
        assert_eq!(side_events[0]["type"], "message.received");
        assert_eq!(
            side_events[0]["path"],
            "contacts/zhangsan/messages.res.jsonl"
        );
    }

    #[test]
    fn mark_read_clears_unread_inbox_without_upstream_receipts() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(Arc::clone(&state));
        connector
            .submit_action(
                SubmitActionRequest {
                    path: "/contacts/send_message.act".to_string(),
                    payload: json!({"to":"basic:zhangsan","text":"outbound"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("seed contact");
        state
            .lock()
            .expect("mock state")
            .inbound
            .push(TinodeInboundMessage {
                topic: "usr-zhangsan".to_string(),
                seq: 2,
                from_tinode_user_id: "usr-zhangsan".to_string(),
                text: "reply".to_string(),
            });
        connector
            .drain_inbound_events(&ctx())
            .expect("drain inbound");

        let content = completed_content(
            connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/inbox/mark_read.act".to_string(),
                        payload: json!({"all": true}),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("mark read"),
        );
        assert_eq!(content["unread_count"], 0);

        let unread = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/inbox/unread.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("unread inbox");
        assert!(unread.records.is_empty());
    }

    #[test]
    fn direct_message_can_target_ready_principal_without_guessing_login() {
        let config = config();
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut default_connector = connector_with_mock_config(config.clone(), Arc::clone(&state));
        let mut incident_connector = connector_with_mock_config(config, Arc::clone(&state));

        let mut incident_ctx = ctx();
        incident_ctx.principal_id = Some("incident-reporter".to_string());
        incident_ctx.profile_id = Some("tinode:incident-reporter".to_string());
        incident_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &incident_ctx,
            )
            .expect("incident credentials");

        let content = completed_content(
            default_connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/contacts/send_message.act".to_string(),
                        payload: json!({
                            "to": "principal:incident-reporter",
                            "text": "hello incident agent"
                        }),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("principal direct message"),
        );

        assert_eq!(content["ok"], true);
        assert_eq!(content["conversation_type"], "direct");
        let state = state.lock().expect("mock state");
        assert_eq!(state.sent.len(), 1);
        assert!(state.sent[0].0.contains("incident_reporter"));
        assert!(
            state.resolved.is_empty(),
            "principal ref must not use basic search"
        );
    }

    #[test]
    fn principal_receiver_discovers_ready_sender_for_inbox_drain() {
        let config = config();
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut default_connector = connector_with_mock_config(config.clone(), Arc::clone(&state));
        let mut code_connector = connector_with_mock_config(config, Arc::clone(&state));

        default_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("default credentials");

        let mut code_ctx = ctx();
        code_ctx.principal_id = Some("code-implementer".to_string());
        code_ctx.profile_id = Some("tinode:code-implementer".to_string());
        code_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &code_ctx,
            )
            .expect("code credentials");

        assert!(
            code_connector.contacts.is_empty(),
            "receiver should not need an explicit local contact"
        );
        let default_login = state
            .lock()
            .expect("mock state")
            .created
            .iter()
            .find(|request| request.profile_id == "tinode:default")
            .expect("default account request")
            .login
            .clone();
        state
            .lock()
            .expect("mock state")
            .inbound
            .push(TinodeInboundMessage {
                topic: format!("usr-{default_login}"),
                seq: 1,
                from_tinode_user_id: format!("usr-{default_login}"),
                text: "hello from default".to_string(),
            });

        let events = code_connector
            .drain_inbound_events(&code_ctx)
            .expect("drain principal inbound");

        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "message.received");
        assert_eq!(events[0].path, "contacts/default/messages.res.jsonl");
        assert_eq!(
            events[0]
                .content
                .as_ref()
                .and_then(|value| value.get("contact_key"))
                .and_then(JsonValue::as_str),
            Some("default")
        );
        assert!(code_connector.contacts.contains_key("default"));
    }

    #[test]
    fn principal_receiver_inbox_read_through_discovers_ready_sender() {
        let config = config();
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut default_connector = connector_with_mock_config(config.clone(), Arc::clone(&state));
        let mut code_connector = connector_with_mock_config(config, Arc::clone(&state));

        default_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("default credentials");

        let mut code_ctx = ctx();
        code_ctx.principal_id = Some("code-implementer".to_string());
        code_ctx.profile_id = Some("tinode:code-implementer".to_string());
        code_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &code_ctx,
            )
            .expect("code credentials");

        let default_login = state
            .lock()
            .expect("mock state")
            .created
            .iter()
            .find(|request| request.profile_id == "tinode:default")
            .expect("default account request")
            .login
            .clone();
        state
            .lock()
            .expect("mock state")
            .inbound
            .push(TinodeInboundMessage {
                topic: format!("usr-{default_login}"),
                seq: 1,
                from_tinode_user_id: format!("usr-{default_login}"),
                text: "read-through hello".to_string(),
            });

        let inbox = code_connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/inbox/recent.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &code_ctx,
            )
            .expect("inbox read-through");

        assert_eq!(inbox.records.len(), 1);
        assert_eq!(inbox.records[0].line["contact_key"], "default");
        assert_eq!(inbox.records[0].line["text"], "read-through hello");
    }

    #[test]
    fn principal_receiver_inbox_read_through_uses_shared_credentials_for_fresh_connector() {
        let config = config();
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut default_runtime_connector =
            connector_with_mock_config(config.clone(), Arc::clone(&state));
        let mut code_runtime_connector =
            connector_with_mock_config(config.clone(), Arc::clone(&state));
        let mut fresh_read_through_connector =
            connector_with_mock_config(config, Arc::clone(&state));

        default_runtime_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect("default credentials");

        let mut code_ctx = ctx();
        code_ctx.principal_id = Some("code-implementer".to_string());
        code_ctx.profile_id = Some("tinode:code-implementer".to_string());
        code_runtime_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &code_ctx,
            )
            .expect("code credentials");

        let default_login = state
            .lock()
            .expect("mock state")
            .created
            .iter()
            .find(|request| request.profile_id == "tinode:default")
            .expect("default account request")
            .login
            .clone();
        state
            .lock()
            .expect("mock state")
            .inbound
            .push(TinodeInboundMessage {
                topic: format!("usr-{default_login}"),
                seq: 1,
                from_tinode_user_id: format!("usr-{default_login}"),
                text: "fresh connector hello".to_string(),
            });

        assert!(
            fresh_read_through_connector.credentials.is_empty(),
            "mount read-through starts as a fresh connector instance"
        );
        let inbox = fresh_read_through_connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/inbox/recent.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &code_ctx,
            )
            .expect("fresh connector inbox read-through");

        assert_eq!(inbox.records.len(), 1);
        assert_eq!(inbox.records[0].line["contact_key"], "default");
        assert_eq!(inbox.records[0].line["text"], "fresh connector hello");
        assert!(
            fresh_read_through_connector
                .credentials
                .contains_key("tinode:code-implementer"),
            "fresh read-through connector should hydrate its credential from shared state"
        );
    }

    #[test]
    fn inbound_messages_parse_from_tinode_meta_data_payloads() {
        let messages = super::inbound_messages_from_meta(
            &json!({
                "topic": "usrDefault",
                "data": [
                    {
                        "seq": 7,
                        "from": "usrCode",
                        "content": { "txt": "hello from meta" }
                    }
                ]
            }),
            "usrFallback",
        );

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].topic, "usrDefault");
        assert_eq!(messages[0].seq, 7);
        assert_eq!(messages[0].from_tinode_user_id, "usrCode");
        assert_eq!(messages[0].text, "hello from meta");
    }

    #[test]
    fn tinode_text_plain_content_is_sent_as_string_and_read_back() {
        let content = super::tinode_text_plain_content("hello plain text");
        assert_eq!(content, json!("hello plain text"));

        let inbound = super::inbound_message_from_data(&json!({
            "topic": "usrDefault",
            "seq": 8,
            "from": "usrCode",
            "content": "hello plain text"
        }))
        .expect("plain text inbound");
        assert_eq!(inbound.text, "hello plain text");
    }

    #[test]
    fn tinode_inbound_messages_are_deduped_after_sub_and_get() {
        let messages = super::dedupe_tinode_inbound_messages(vec![
            TinodeInboundMessage {
                topic: "usrA".to_string(),
                seq: 2,
                from_tinode_user_id: "usrA".to_string(),
                text: "two".to_string(),
            },
            TinodeInboundMessage {
                topic: "usrA".to_string(),
                seq: 1,
                from_tinode_user_id: "usrA".to_string(),
                text: "one".to_string(),
            },
            TinodeInboundMessage {
                topic: "usrA".to_string(),
                seq: 2,
                from_tinode_user_id: "usrA".to_string(),
                text: "two duplicate".to_string(),
            },
        ]);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].seq, 1);
        assert_eq!(messages[1].seq, 2);
        assert_eq!(messages[1].text, "two");
    }

    #[test]
    fn tinode_get_data_payload_uses_nested_data_options() {
        let payload = super::tinode_get_data_payload("usrCode", Some(7), 50);

        assert_eq!(payload["topic"], "usrCode");
        assert_eq!(payload["what"], "data");
        assert_eq!(payload["data"]["since"], 8);
        assert_eq!(payload["data"]["limit"], 50);
        assert!(payload.get("since").is_none());
        assert!(payload.get("limit").is_none());

        let sub = super::tinode_sub_data_payload("usrCode", Some(7), 50);
        assert_eq!(sub["topic"], "usrCode");
        assert_eq!(sub["get"]["what"], "data");
        assert_eq!(sub["get"]["data"]["since"], 8);
        assert_eq!(sub["get"]["data"]["limit"], 50);
    }

    #[test]
    fn group_create_invite_and_send_support_basic_and_principal_members() {
        let config = config();
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut default_connector = connector_with_mock_config(config.clone(), Arc::clone(&state));
        let mut incident_connector = connector_with_mock_config(config, Arc::clone(&state));

        let mut incident_ctx = ctx();
        incident_ctx.principal_id = Some("incident-reporter".to_string());
        incident_ctx.profile_id = Some("tinode:incident-reporter".to_string());
        incident_connector
            .submit_action(
                SubmitActionRequest {
                    path: "/_app/ensure_credentials.act".to_string(),
                    payload: json!({}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &incident_ctx,
            )
            .expect("incident credentials");

        let create = completed_content(
            default_connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/groups/create_group.act".to_string(),
                        payload: json!({
                            "title": "Incident Room",
                            "members": ["basic:zhangsan", "principal:incident-reporter"],
                            "initial_message": "group boot"
                        }),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("create group"),
        );
        assert_eq!(create["ok"], true);
        assert_eq!(create["group_key"], "incidentroom");
        assert_eq!(create["member_count"], 2);

        let send = completed_content(
            default_connector
                .submit_action(
                    SubmitActionRequest {
                        path: "/groups/incidentroom/send_message.act".to_string(),
                        payload: json!({"text":"follow up"}),
                        execution_mode: ActionExecutionMode::Inline,
                    },
                    &ctx(),
                )
                .expect("send group"),
        );
        assert_eq!(send["conversation_type"], "group");

        let groups = default_connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/groups/index.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("groups index");
        assert_eq!(groups.records.len(), 1);
        assert_eq!(groups.records[0].line["member_count"], 2);
        assert_eq!(groups.records[0].line["group_key"], "incidentroom");

        let messages = default_connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/groups/incidentroom/messages.res.jsonl".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx(),
            )
            .expect("group messages");
        assert_eq!(messages.records.len(), 2);
        assert_eq!(messages.records[1].line["text"], "follow up");

        let state = state.lock().expect("mock state");
        assert_eq!(state.groups_created.len(), 1);
        assert_eq!(state.groups_created[0].1.len(), 2);
        assert_eq!(state.group_sent.len(), 2);
        assert_eq!(state.resolved, vec!["zhangsan"]);
    }

    #[test]
    fn principal_group_member_requires_ready_credentials() {
        let state = Arc::new(Mutex::new(MockGatewayState::default()));
        let mut connector = connector_with_mock(state);
        let err = connector
            .submit_action(
                SubmitActionRequest {
                    path: "/groups/create_group.act".to_string(),
                    payload: json!({
                        "title": "Incident Room",
                        "members": ["principal:missing-agent"]
                    }),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx(),
            )
            .expect_err("missing principal should fail");

        assert_eq!(err.code, connector_error_codes::PROFILE_NOT_READY);
        assert!(err.message.contains("principal:missing-agent"));
    }
}
