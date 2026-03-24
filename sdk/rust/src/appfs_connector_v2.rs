use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// Frozen AppFS connector SDK surface version for v0.3.
pub const APPFS_CONNECTOR_SDK_V2_VERSION: &str = "0.3.0";

/// Frozen connector error code constants for v0.3.
pub mod connector_error_codes_v2 {
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
pub enum ConnectorTransportV2 {
    InProcess,
    HttpBridge,
    GrpcBridge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorInfoV2 {
    pub connector_id: String,
    pub version: String,
    pub app_id: String,
    pub transport: ConnectorTransportV2,
    pub supports_snapshot: bool,
    pub supports_live: bool,
    pub supports_action: bool,
    #[serde(default)]
    pub optional_features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectorContextV2 {
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
pub enum AuthStatusV2 {
    Valid,
    Expired,
    Refreshing,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthStatusV2 {
    pub healthy: bool,
    pub auth_status: AuthStatusV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub checked_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SnapshotMetaV2 {
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
pub enum SnapshotResumeV2 {
    Start,
    Cursor(String),
    Offset(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchSnapshotChunkRequestV2 {
    pub resource_path: String,
    pub resume: SnapshotResumeV2,
    pub budget_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRecordV2 {
    pub record_key: String,
    pub ordering_key: String,
    pub line: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchSnapshotChunkResponseV2 {
    pub records: Vec<SnapshotRecordV2>,
    pub emitted_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub has_more: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchLivePageRequestV2 {
    pub resource_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub page_size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveModeV2 {
    Live,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePageInfoV2 {
    pub handle_id: String,
    pub page_no: u32,
    pub has_more: bool,
    pub mode: LiveModeV2,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_ms: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FetchLivePageResponseV2 {
    pub items: Vec<JsonValue>,
    pub page: LivePageInfoV2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionExecutionModeV2 {
    Inline,
    Streaming,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitActionRequestV2 {
    pub path: String,
    pub payload: JsonValue,
    pub execution_mode: ActionExecutionModeV2,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionStreamingPlanV2 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_content: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_content: Option<JsonValue>,
    pub terminal_content: JsonValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SubmitActionOutcomeV2 {
    Completed { content: JsonValue },
    Streaming { plan: ActionStreamingPlanV2 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubmitActionResponseV2 {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_duration_ms: Option<u32>,
    pub outcome: SubmitActionOutcomeV2,
}

#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[error("{code}: {message}")]
pub struct ConnectorErrorV2 {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
}

/// AppFS connector v0.3 frozen canonical trait surface.
pub trait AppConnectorV2: Send {
    fn connector_id(&self) -> std::result::Result<ConnectorInfoV2, ConnectorErrorV2>;

    fn health(
        &mut self,
        ctx: &ConnectorContextV2,
    ) -> std::result::Result<HealthStatusV2, ConnectorErrorV2>;

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: std::time::Duration,
        ctx: &ConnectorContextV2,
    ) -> std::result::Result<SnapshotMetaV2, ConnectorErrorV2>;

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequestV2,
        ctx: &ConnectorContextV2,
    ) -> std::result::Result<FetchSnapshotChunkResponseV2, ConnectorErrorV2>;

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequestV2,
        ctx: &ConnectorContextV2,
    ) -> std::result::Result<FetchLivePageResponseV2, ConnectorErrorV2>;

    fn submit_action(
        &mut self,
        request: SubmitActionRequestV2,
        ctx: &ConnectorContextV2,
    ) -> std::result::Result<SubmitActionResponseV2, ConnectorErrorV2>;
}

#[cfg(test)]
mod tests {
    use super::connector_error_codes_v2 as code;

    #[test]
    fn connector_error_codes_match_frozen_set() {
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
