use agentfs_sdk::{
    AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
    AdapterInputModeV1, AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1,
    RequestContextV1,
};
use serde_json::json;

/// Minimal Rust adapter template implementing the frozen AppFS v0.1 SDK surface.
///
/// Replace business logic inside `submit_action` and `submit_control_action`
/// with your real app integration calls.
pub struct MinimalAdapter {
    app_id: String,
}

impl MinimalAdapter {
    pub fn new(app_id: impl Into<String>) -> Self {
        Self {
            app_id: app_id.into(),
        }
    }
}

impl AppAdapterV1 for MinimalAdapter {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn submit_action(
        &mut self,
        path: &str,
        payload: &str,
        _input_mode: AdapterInputModeV1,
        execution_mode: AdapterExecutionModeV1,
        _ctx: &RequestContextV1,
    ) -> std::result::Result<AdapterSubmitOutcomeV1, AdapterErrorV1> {
        if path.ends_with("/reject.act") {
            return Err(AdapterErrorV1::Rejected {
                code: "INVALID_ARGUMENT".to_string(),
                message: "payload rejected by template".to_string(),
                retryable: false,
            });
        }
        if path.ends_with("/internal.act") {
            return Err(AdapterErrorV1::Internal {
                message: "template internal error path".to_string(),
            });
        }

        match execution_mode {
            AdapterExecutionModeV1::Inline => Ok(AdapterSubmitOutcomeV1::Completed {
                content: json!({
                    "ok": true,
                    "path": path,
                    "echo": payload.trim_end()
                }),
            }),
            AdapterExecutionModeV1::Streaming => Ok(AdapterSubmitOutcomeV1::Streaming {
                plan: AdapterStreamingPlanV1 {
                    accepted_content: Some(json!({ "state": "accepted" })),
                    progress_content: Some(json!({ "percent": 50 })),
                    terminal_content: json!({ "ok": true }),
                },
            }),
        }
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
                    "items": [{"id": format!("m-{page_no}"), "text": "template page item"}],
                    "page": {
                        "handle_id": handle_id,
                        "page_no": page_no,
                        "has_more": has_more,
                        "mode": "snapshot"
                    }
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

#[cfg(test)]
mod tests {
    use super::MinimalAdapter;
    use agentfs_sdk::{
        default_request_context_v1, run_error_case_matrix_v1, run_required_case_matrix_v1,
        ErrorCaseMatrixV1, RequiredCaseMatrixV1,
    };

    #[test]
    fn template_passes_required_matrix() {
        let mut adapter = MinimalAdapter::new("aiim");
        run_required_case_matrix_v1(
            &mut adapter,
            &default_request_context_v1("aiim"),
            &RequiredCaseMatrixV1::default(),
        )
        .expect("required matrix should pass");
    }

    #[test]
    fn template_passes_error_matrix() {
        let mut adapter = MinimalAdapter::new("aiim");
        run_error_case_matrix_v1(
            &mut adapter,
            &default_request_context_v1("aiim"),
            &ErrorCaseMatrixV1::default(),
        )
        .expect("error matrix should pass");
    }
}
