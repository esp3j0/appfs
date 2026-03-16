use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use thiserror::Error;

/// v0.1 frozen input payload mode model for adapter dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterInputModeV1 {
    Text,
    Json,
    TextOrJson,
}

/// v0.1 frozen execution mode model for adapter dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterExecutionModeV1 {
    Inline,
    Streaming,
}

/// Runtime correlation and principal context passed to adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestContextV1 {
    pub app_id: String,
    pub session_id: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_token: Option<String>,
}

/// Streaming lifecycle payload plan emitted by runtime.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterStreamingPlanV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_content: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_content: Option<JsonValue>,
    pub terminal_content: JsonValue,
}

/// v0.1 frozen adapter submit outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterSubmitOutcomeV1 {
    Completed { content: JsonValue },
    Streaming { plan: AdapterStreamingPlanV1 },
}

/// v0.1 frozen adapter error contract.
#[derive(Debug, Clone, PartialEq, Eq, Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterErrorV1 {
    #[error("{code}: {message}")]
    Rejected {
        code: String,
        message: String,
        #[serde(default)]
        retryable: bool,
    },
    #[error("adapter internal error: {message}")]
    Internal { message: String },
}

/// AppFS adapter SDK v0.1 frozen trait surface.
///
/// Compatibility:
/// 1. `v0.1.x` allows additive-only, backward-compatible changes.
/// 2. Breaking method/behavior changes require a `v0.2` trait surface.
pub trait AppAdapterV1: Send {
    fn app_id(&self) -> &str;

    fn submit_action(
        &mut self,
        path: &str,
        payload: &str,
        input_mode: AdapterInputModeV1,
        execution_mode: AdapterExecutionModeV1,
        ctx: &RequestContextV1,
    ) -> std::result::Result<AdapterSubmitOutcomeV1, AdapterErrorV1>;
}
