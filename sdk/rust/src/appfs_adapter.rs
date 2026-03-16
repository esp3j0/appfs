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

/// v0.1 control-path action model (non-resource action channels such as paging control).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterControlActionV1 {
    PagingFetchNext {
        handle_id: String,
        page_no: u64,
        has_more: bool,
    },
    PagingClose {
        handle_id: String,
    },
}

/// v0.1 frozen adapter submit outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterSubmitOutcomeV1 {
    Completed { content: JsonValue },
    Streaming { plan: AdapterStreamingPlanV1 },
}

/// v0.1 frozen adapter control-path outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdapterControlOutcomeV1 {
    Completed { content: JsonValue },
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

    fn submit_control_action(
        &mut self,
        path: &str,
        action: AdapterControlActionV1,
        _ctx: &RequestContextV1,
    ) -> std::result::Result<AdapterControlOutcomeV1, AdapterErrorV1> {
        let _ = path;
        let _ = action;
        Err(AdapterErrorV1::Rejected {
            code: "NOT_SUPPORTED".to_string(),
            message: "control action is not supported by this adapter".to_string(),
            retryable: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
        AdapterInputModeV1, AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1,
        RequestContextV1,
    };
    use serde_json::json;

    struct SmokeAdapter;

    impl AppAdapterV1 for SmokeAdapter {
        fn app_id(&self) -> &str {
            "aiim"
        }

        fn submit_action(
            &mut self,
            path: &str,
            _payload: &str,
            _input_mode: AdapterInputModeV1,
            execution_mode: AdapterExecutionModeV1,
            _ctx: &RequestContextV1,
        ) -> std::result::Result<AdapterSubmitOutcomeV1, AdapterErrorV1> {
            if execution_mode == AdapterExecutionModeV1::Inline {
                return Ok(AdapterSubmitOutcomeV1::Completed {
                    content: json!({ "path": path }),
                });
            }
            Ok(AdapterSubmitOutcomeV1::Streaming {
                plan: AdapterStreamingPlanV1 {
                    accepted_content: Some(json!("accepted")),
                    progress_content: Some(json!({ "percent": 50 })),
                    terminal_content: json!({ "ok": true }),
                },
            })
        }

        fn submit_control_action(
            &mut self,
            _path: &str,
            action: AdapterControlActionV1,
            _ctx: &RequestContextV1,
        ) -> std::result::Result<AdapterControlOutcomeV1, AdapterErrorV1> {
            match action {
                AdapterControlActionV1::PagingFetchNext {
                    handle_id,
                    page_no,
                    has_more,
                } => Ok(AdapterControlOutcomeV1::Completed {
                    content: json!({
                        "page": { "handle_id": handle_id, "page_no": page_no, "has_more": has_more }
                    }),
                }),
                AdapterControlActionV1::PagingClose { handle_id } => {
                    Ok(AdapterControlOutcomeV1::Completed {
                        content: json!({ "closed": true, "handle_id": handle_id }),
                    })
                }
            }
        }
    }

    #[test]
    fn sdk_trait_smoke_submit_and_control() {
        let mut adapter = SmokeAdapter;
        let ctx = RequestContextV1 {
            app_id: "aiim".to_string(),
            session_id: "sess-test".to_string(),
            request_id: "req-test".to_string(),
            client_token: Some("tok-1".to_string()),
        };

        let inline = adapter
            .submit_action(
                "/contacts/zhangsan/send_message.act",
                "hello\n",
                AdapterInputModeV1::Text,
                AdapterExecutionModeV1::Inline,
                &ctx,
            )
            .expect("inline should succeed");
        match inline {
            AdapterSubmitOutcomeV1::Completed { content } => {
                assert_eq!(content["path"], "/contacts/zhangsan/send_message.act");
            }
            _ => panic!("expected completed"),
        }

        let streaming = adapter
            .submit_action(
                "/files/file-001/download.act",
                "{\"target\":\"/tmp/a.bin\"}",
                AdapterInputModeV1::Json,
                AdapterExecutionModeV1::Streaming,
                &ctx,
            )
            .expect("streaming should succeed");
        match streaming {
            AdapterSubmitOutcomeV1::Streaming { plan } => {
                assert_eq!(plan.accepted_content, Some(json!("accepted")));
                assert_eq!(plan.progress_content, Some(json!({ "percent": 50 })));
                assert_eq!(plan.terminal_content["ok"], true);
            }
            _ => panic!("expected streaming"),
        }

        let fetch = adapter
            .submit_control_action(
                "/_paging/fetch_next.act",
                AdapterControlActionV1::PagingFetchNext {
                    handle_id: "ph_abc".to_string(),
                    page_no: 1,
                    has_more: true,
                },
                &ctx,
            )
            .expect("fetch should succeed");
        match fetch {
            AdapterControlOutcomeV1::Completed { content } => {
                assert_eq!(content["page"]["handle_id"], "ph_abc");
                assert_eq!(content["page"]["page_no"], 1);
            }
        }
    }
}
