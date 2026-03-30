use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Canonical AppFS connector SDK surface version after runtime closure cleanup.
pub const APPFS_CONNECTOR_SDK_VERSION: &str = "0.4.0";

/// Canonical connector error code constants.
pub mod connector_error_codes {
    pub const INVALID_ARGUMENT: &str = "INVALID_ARGUMENT";
    pub const INVALID_PAYLOAD: &str = "INVALID_PAYLOAD";
    pub const NOT_SUPPORTED: &str = "NOT_SUPPORTED";
    pub const SNAPSHOT_TOO_LARGE: &str = "SNAPSHOT_TOO_LARGE";
    pub const CACHE_MISS_EXPAND_FAILED: &str = "CACHE_MISS_EXPAND_FAILED";
    pub const INTERNAL: &str = "INTERNAL";
    pub const UPSTREAM_UNAVAILABLE: &str = "UPSTREAM_UNAVAILABLE";
    pub const RATE_LIMITED: &str = "RATE_LIMITED";
    pub const AUTH_EXPIRED: &str = "AUTH_EXPIRED";
    pub const PERMISSION_DENIED: &str = "PERMISSION_DENIED";
    pub const RESOURCE_EXHAUSTED: &str = "RESOURCE_EXHAUSTED";
    pub const TIMEOUT: &str = "TIMEOUT";
    pub const CURSOR_INVALID: &str = "CURSOR_INVALID";
    pub const CURSOR_EXPIRED: &str = "CURSOR_EXPIRED";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorTransport {
    InProcess,
    HttpBridge,
    GrpcBridge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorInfo {
    pub connector_id: String,
    pub version: String,
    pub app_id: String,
    pub transport: ConnectorTransport,
    pub supports_snapshot: bool,
    pub supports_live: bool,
    pub supports_action: bool,
    #[serde(default)]
    pub optional_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorContext {
    pub app_id: String,
    pub session_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStatus {
    Valid,
    Expired,
    Refreshing,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStatus {
    pub healthy: bool,
    pub auth_status: AuthStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub checked_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SnapshotMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum SnapshotResume {
    Start,
    Cursor(String),
    Offset(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchSnapshotChunkRequest {
    pub resource_path: String,
    pub resume: SnapshotResume,
    pub budget_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub record_key: String,
    pub ordering_key: String,
    pub line: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchSnapshotChunkResponse {
    pub records: Vec<SnapshotRecord>,
    pub emitted_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchLivePageRequest {
    pub resource_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub page_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveMode {
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePageInfo {
    pub handle_id: String,
    pub page_no: u32,
    pub has_more: bool,
    pub mode: LiveMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchLivePageResponse {
    pub items: Vec<JsonValue>,
    pub page: LivePageInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionExecutionMode {
    Inline,
    Streaming,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitActionRequest {
    pub path: String,
    pub payload: JsonValue,
    pub execution_mode: ActionExecutionMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionStreamingPlan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_content: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_content: Option<JsonValue>,
    pub terminal_content: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubmitActionOutcome {
    Completed { content: JsonValue },
    Streaming { plan: ActionStreamingPlan },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitActionResponse {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_duration_ms: Option<u32>,
    pub outcome: SubmitActionOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("{code}: {message}")]
pub struct ConnectorError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStructureSyncReason {
    Initialize,
    EnterScope,
    Refresh,
    Recover,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetAppStructureRequest {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshAppStructureRequest {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_revision: Option<String>,
    pub reason: AppStructureSyncReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_action_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStructureNodeKind {
    Directory,
    ActionFile,
    SnapshotResource,
    LiveResource,
    StaticJsonResource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStructureNode {
    pub path: String,
    pub kind: AppStructureNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_entry: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_content: Option<JsonValue>,
    #[serde(default)]
    pub mutable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppStructureSnapshot {
    pub app_id: String,
    pub revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_scope: Option<String>,
    #[serde(default)]
    pub ownership_prefixes: Vec<String>,
    pub nodes: Vec<AppStructureNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppStructureSyncResult {
    Unchanged {
        app_id: String,
        revision: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_scope: Option<String>,
    },
    Snapshot {
        snapshot: AppStructureSnapshot,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GetAppStructureResponse {
    pub result: AppStructureSyncResult,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshAppStructureResponse {
    pub result: AppStructureSyncResult,
}

fn not_supported_structure_sync() -> ConnectorError {
    ConnectorError {
        code: connector_error_codes::NOT_SUPPORTED.to_string(),
        message: "app structure sync is not supported by this connector".to_string(),
        retryable: false,
        details: None,
    }
}

/// Canonical AppFS connector trait used by the runtime and mount-side read-through.
pub trait AppConnector: Send {
    fn connector_id(&self) -> std::result::Result<ConnectorInfo, ConnectorError>;

    fn health(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<HealthStatus, ConnectorError>;

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: std::time::Duration,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SnapshotMeta, ConnectorError>;

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchSnapshotChunkResponse, ConnectorError>;

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchLivePageResponse, ConnectorError>;

    fn submit_action(
        &mut self,
        request: SubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SubmitActionResponse, ConnectorError>;

    fn get_app_structure(
        &mut self,
        _request: GetAppStructureRequest,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<GetAppStructureResponse, ConnectorError> {
        Err(not_supported_structure_sync())
    }

    fn refresh_app_structure(
        &mut self,
        _request: RefreshAppStructureRequest,
        _ctx: &ConnectorContext,
    ) -> std::result::Result<RefreshAppStructureResponse, ConnectorError> {
        Err(not_supported_structure_sync())
    }
}

#[cfg(test)]
mod tests {
    use super::connector_error_codes as code;

    #[test]
    fn connector_error_codes_match_expected_set() {
        let actual = vec![
            code::INVALID_ARGUMENT,
            code::INVALID_PAYLOAD,
            code::NOT_SUPPORTED,
            code::SNAPSHOT_TOO_LARGE,
            code::CACHE_MISS_EXPAND_FAILED,
            code::INTERNAL,
            code::UPSTREAM_UNAVAILABLE,
            code::RATE_LIMITED,
            code::AUTH_EXPIRED,
            code::PERMISSION_DENIED,
            code::RESOURCE_EXHAUSTED,
            code::TIMEOUT,
            code::CURSOR_INVALID,
            code::CURSOR_EXPIRED,
        ];

        let expected = vec![
            "INVALID_ARGUMENT",
            "INVALID_PAYLOAD",
            "NOT_SUPPORTED",
            "SNAPSHOT_TOO_LARGE",
            "CACHE_MISS_EXPAND_FAILED",
            "INTERNAL",
            "UPSTREAM_UNAVAILABLE",
            "RATE_LIMITED",
            "AUTH_EXPIRED",
            "PERMISSION_DENIED",
            "RESOURCE_EXHAUSTED",
            "TIMEOUT",
            "CURSOR_INVALID",
            "CURSOR_EXPIRED",
        ];

        assert_eq!(actual, expected);
    }
}
