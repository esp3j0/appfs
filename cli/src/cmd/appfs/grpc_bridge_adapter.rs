use super::bridge_resilience::{
    is_retryable_grpc_code, BridgeCircuitBreaker, BridgeMetrics, BridgeRuntimeOptions,
};
use agentfs_sdk::{
    AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
    AdapterInputModeV1, AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1,
    RequestContextV1,
};
use serde_json::Value as JsonValue;
use std::future::Future;
use std::time::{Duration, Instant};
use tonic::transport::{Channel, Endpoint};

pub(super) mod proto {
    tonic::include_proto!("appfs.adapter.v1");
}

use proto::appfs_adapter_bridge_client::AppfsAdapterBridgeClient;
use proto::submit_action_response::Result as SubmitActionResult;
use proto::submit_control_action_request::Action as SubmitControlAction;
use proto::submit_control_action_response::Result as SubmitControlResult;
use proto::{
    ControlCompletedOutcome, ExecutionMode, InputMode, PagingCloseAction, PagingFetchNextAction,
    RequestContext, SubmitActionRequest, SubmitControlActionRequest,
};

pub(super) struct GrpcBridgeAdapterV1 {
    app_id: String,
    client: AppfsAdapterBridgeClient<Channel>,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

impl GrpcBridgeAdapterV1 {
    pub(super) fn new(
        app_id: String,
        endpoint: String,
        timeout: Duration,
        runtime_options: BridgeRuntimeOptions,
    ) -> Result<Self, AdapterErrorV1> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        let endpoint =
            Endpoint::from_shared(endpoint.clone()).map_err(|err| AdapterErrorV1::Internal {
                message: format!("invalid grpc endpoint {endpoint}: {err}"),
            })?;

        let channel = endpoint
            .connect_timeout(timeout)
            .timeout(timeout)
            .tcp_nodelay(true)
            .connect_lazy();

        Ok(Self {
            app_id,
            client: AppfsAdapterBridgeClient::new(channel),
            runtime_options,
            metrics: BridgeMetrics::default(),
            circuit_breaker: BridgeCircuitBreaker::default(),
        })
    }

    fn submit_action_rpc(
        &mut self,
        request: SubmitActionRequest,
    ) -> Result<proto::SubmitActionResponse, AdapterErrorV1> {
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            let message = format!(
                "bridge grpc circuit open for SubmitAction; retry_in_ms={} metrics={}",
                remaining.as_millis(),
                self.metrics.snapshot()
            );
            if self.metrics.short_circuited_total <= 3
                || self.metrics.short_circuited_total.is_multiple_of(10)
            {
                eprintln!("AppFS bridge grpc short-circuit: {message}");
            }
            return Err(AdapterErrorV1::Internal { message });
        }

        let max_attempts = self.runtime_options.max_retries.saturating_add(1).max(1);
        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let mut client = self.client.clone();
            match run_async(client.submit_action(request.clone())) {
                Ok(response) => {
                    self.circuit_breaker.record_success();
                    self.metrics.record_request(attempt, true);
                    self.log_observation("SubmitAction", attempt, started.elapsed(), "ok");
                    return Ok(response.into_inner());
                }
                Err(status) => {
                    let retryable = is_retryable_grpc_code(status.code());
                    if retryable && attempt < max_attempts {
                        let backoff = self.runtime_options.retry_backoff_for_attempt(attempt);
                        eprintln!(
                            "AppFS bridge grpc retry method=SubmitAction attempt={}/{} code={} backoff_ms={}",
                            attempt,
                            max_attempts,
                            status.code(),
                            backoff.as_millis()
                        );
                        std::thread::sleep(backoff);
                        continue;
                    }

                    if retryable {
                        let opened = self
                            .circuit_breaker
                            .record_failure(Instant::now(), self.runtime_options);
                        if opened {
                            eprintln!(
                                "AppFS bridge grpc circuit opened after SubmitAction failure code={} {}",
                                status.code(),
                                self.metrics.snapshot()
                            );
                        }
                    } else {
                        self.circuit_breaker.record_success();
                    }

                    self.metrics.record_request(attempt, false);
                    self.log_observation("SubmitAction", attempt, started.elapsed(), "failed");
                    return Err(map_grpc_status(
                        "SubmitAction",
                        status,
                        attempt,
                        &self.metrics.snapshot(),
                    ));
                }
            }
        }
    }

    fn submit_control_rpc(
        &mut self,
        request: SubmitControlActionRequest,
    ) -> Result<proto::SubmitControlActionResponse, AdapterErrorV1> {
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            let message = format!(
                "bridge grpc circuit open for SubmitControlAction; retry_in_ms={} metrics={}",
                remaining.as_millis(),
                self.metrics.snapshot()
            );
            if self.metrics.short_circuited_total <= 3
                || self.metrics.short_circuited_total.is_multiple_of(10)
            {
                eprintln!("AppFS bridge grpc short-circuit: {message}");
            }
            return Err(AdapterErrorV1::Internal { message });
        }

        let max_attempts = self.runtime_options.max_retries.saturating_add(1).max(1);
        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let mut client = self.client.clone();
            match run_async(client.submit_control_action(request.clone())) {
                Ok(response) => {
                    self.circuit_breaker.record_success();
                    self.metrics.record_request(attempt, true);
                    self.log_observation("SubmitControlAction", attempt, started.elapsed(), "ok");
                    return Ok(response.into_inner());
                }
                Err(status) => {
                    let retryable = is_retryable_grpc_code(status.code());
                    if retryable && attempt < max_attempts {
                        let backoff = self.runtime_options.retry_backoff_for_attempt(attempt);
                        eprintln!(
                            "AppFS bridge grpc retry method=SubmitControlAction attempt={}/{} code={} backoff_ms={}",
                            attempt,
                            max_attempts,
                            status.code(),
                            backoff.as_millis()
                        );
                        std::thread::sleep(backoff);
                        continue;
                    }

                    if retryable {
                        let opened = self
                            .circuit_breaker
                            .record_failure(Instant::now(), self.runtime_options);
                        if opened {
                            eprintln!(
                                "AppFS bridge grpc circuit opened after SubmitControlAction failure code={} {}",
                                status.code(),
                                self.metrics.snapshot()
                            );
                        }
                    } else {
                        self.circuit_breaker.record_success();
                    }

                    self.metrics.record_request(attempt, false);
                    self.log_observation(
                        "SubmitControlAction",
                        attempt,
                        started.elapsed(),
                        "failed",
                    );
                    return Err(map_grpc_status(
                        "SubmitControlAction",
                        status,
                        attempt,
                        &self.metrics.snapshot(),
                    ));
                }
            }
        }
    }

    fn log_observation(&self, method: &str, attempts: u32, elapsed: Duration, outcome: &str) {
        if attempts > 1
            || outcome != "ok"
            || self.metrics.requests_total.is_multiple_of(50)
        {
            eprintln!(
                "AppFS bridge grpc metrics method={} outcome={} attempts={} latency_ms={} {}",
                method,
                outcome,
                attempts,
                elapsed.as_millis(),
                self.metrics.snapshot()
            );
        }
    }
}

impl AppAdapterV1 for GrpcBridgeAdapterV1 {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn submit_action(
        &mut self,
        path: &str,
        payload: &str,
        input_mode: AdapterInputModeV1,
        execution_mode: AdapterExecutionModeV1,
        ctx: &RequestContextV1,
    ) -> Result<AdapterSubmitOutcomeV1, AdapterErrorV1> {
        let request = SubmitActionRequest {
            app_id: self.app_id.clone(),
            path: path.to_string(),
            payload: payload.to_string(),
            input_mode: to_proto_input_mode(input_mode) as i32,
            execution_mode: to_proto_execution_mode(execution_mode) as i32,
            context: Some(to_proto_context(ctx)),
        };

        let response = self.submit_action_rpc(request)?;
        match response.result {
            Some(SubmitActionResult::Completed(outcome)) => {
                let content = parse_json_text(&outcome.content_json, "completed.content_json")?;
                Ok(AdapterSubmitOutcomeV1::Completed { content })
            }
            Some(SubmitActionResult::Streaming(outcome)) => {
                let terminal = parse_json_text(
                    &outcome.terminal_content_json,
                    "streaming.terminal_content_json",
                )?;
                let accepted_content = if outcome.has_accepted_content {
                    Some(parse_json_text(
                        &outcome.accepted_content_json,
                        "streaming.accepted_content_json",
                    )?)
                } else {
                    None
                };
                let progress_content = if outcome.has_progress_content {
                    Some(parse_json_text(
                        &outcome.progress_content_json,
                        "streaming.progress_content_json",
                    )?)
                } else {
                    None
                };
                Ok(AdapterSubmitOutcomeV1::Streaming {
                    plan: AdapterStreamingPlanV1 {
                        accepted_content,
                        progress_content,
                        terminal_content: terminal,
                    },
                })
            }
            Some(SubmitActionResult::Error(err)) => Err(AdapterErrorV1::Rejected {
                code: err.code,
                message: err.message,
                retryable: err.retryable,
            }),
            None => Err(AdapterErrorV1::Internal {
                message: "bridge grpc SubmitAction returned empty result".to_string(),
            }),
        }
    }

    fn submit_control_action(
        &mut self,
        path: &str,
        action: AdapterControlActionV1,
        ctx: &RequestContextV1,
    ) -> Result<AdapterControlOutcomeV1, AdapterErrorV1> {
        let action = match action {
            AdapterControlActionV1::PagingFetchNext {
                handle_id,
                page_no,
                has_more,
            } => SubmitControlAction::PagingFetchNext(PagingFetchNextAction {
                handle_id,
                page_no,
                has_more,
            }),
            AdapterControlActionV1::PagingClose { handle_id } => {
                SubmitControlAction::PagingClose(PagingCloseAction { handle_id })
            }
        };
        let request = SubmitControlActionRequest {
            app_id: self.app_id.clone(),
            path: path.to_string(),
            action: Some(action),
            context: Some(to_proto_context(ctx)),
        };
        let response = self.submit_control_rpc(request)?;
        match response.result {
            Some(SubmitControlResult::Completed(ControlCompletedOutcome { content_json })) => {
                let content = parse_json_text(&content_json, "control.completed.content_json")?;
                Ok(AdapterControlOutcomeV1::Completed { content })
            }
            Some(SubmitControlResult::Error(err)) => Err(AdapterErrorV1::Rejected {
                code: err.code,
                message: err.message,
                retryable: err.retryable,
            }),
            None => Err(AdapterErrorV1::Internal {
                message: "bridge grpc SubmitControlAction returned empty result".to_string(),
            }),
        }
    }
}

fn to_proto_input_mode(mode: AdapterInputModeV1) -> InputMode {
    match mode {
        AdapterInputModeV1::Text => InputMode::Text,
        AdapterInputModeV1::Json => InputMode::Json,
        AdapterInputModeV1::TextOrJson => InputMode::TextOrJson,
    }
}

fn to_proto_execution_mode(mode: AdapterExecutionModeV1) -> ExecutionMode {
    match mode {
        AdapterExecutionModeV1::Inline => ExecutionMode::Inline,
        AdapterExecutionModeV1::Streaming => ExecutionMode::Streaming,
    }
}

fn to_proto_context(ctx: &RequestContextV1) -> RequestContext {
    RequestContext {
        app_id: ctx.app_id.clone(),
        session_id: ctx.session_id.clone(),
        request_id: ctx.request_id.clone(),
        client_token: ctx.client_token.clone().unwrap_or_default(),
    }
}

fn parse_json_text(text: &str, field: &str) -> Result<JsonValue, AdapterErrorV1> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| AdapterErrorV1::Internal {
        message: format!("bridge grpc invalid json in {field}: {err}"),
    })
}

fn map_grpc_status(
    method: &str,
    status: tonic::Status,
    attempts: u32,
    metrics: &str,
) -> AdapterErrorV1 {
    AdapterErrorV1::Internal {
        message: format!(
            "bridge grpc {} error: code={} message={} attempts={} metrics={}",
            method,
            status.code(),
            status.message(),
            attempts,
            metrics
        ),
    }
}

fn run_async<F, T>(fut: F) -> T
where
    F: Future<Output = T>,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return tokio::task::block_in_place(|| handle.block_on(fut));
    }
    tokio::runtime::Runtime::new()
        .expect("failed to create temporary tokio runtime for grpc bridge call")
        .block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::{
        parse_json_text, to_proto_execution_mode, to_proto_input_mode, GrpcBridgeAdapterV1,
    };
    use agentfs_sdk::{
        AdapterControlActionV1, AdapterControlOutcomeV1, AdapterExecutionModeV1,
        AdapterInputModeV1, AdapterSubmitOutcomeV1, AppAdapterV1, RequestContextV1,
    };
    use std::net::SocketAddr;
    use std::time::Duration;
    use tonic::{Request, Response, Status};

    #[test]
    fn maps_input_mode_to_proto() {
        assert_eq!(
            to_proto_input_mode(AdapterInputModeV1::Text) as i32,
            super::InputMode::Text as i32
        );
        assert_eq!(
            to_proto_input_mode(AdapterInputModeV1::Json) as i32,
            super::InputMode::Json as i32
        );
        assert_eq!(
            to_proto_input_mode(AdapterInputModeV1::TextOrJson) as i32,
            super::InputMode::TextOrJson as i32
        );
    }

    #[test]
    fn maps_execution_mode_to_proto() {
        assert_eq!(
            to_proto_execution_mode(AdapterExecutionModeV1::Inline) as i32,
            super::ExecutionMode::Inline as i32
        );
        assert_eq!(
            to_proto_execution_mode(AdapterExecutionModeV1::Streaming) as i32,
            super::ExecutionMode::Streaming as i32
        );
    }

    #[test]
    fn parse_json_text_rejects_invalid_payload() {
        let err = parse_json_text("not-json", "f").expect_err("should fail");
        let msg = err.to_string();
        assert!(msg.contains("invalid json"));
    }

    #[derive(Default)]
    struct TestBridgeService;

    #[tonic::async_trait]
    impl super::proto::appfs_adapter_bridge_server::AppfsAdapterBridge for TestBridgeService {
        async fn submit_action(
            &self,
            request: Request<super::SubmitActionRequest>,
        ) -> Result<Response<super::proto::SubmitActionResponse>, Status> {
            let req = request.into_inner();
            if req.path.ends_with("/send_message.act") {
                return Ok(Response::new(super::proto::SubmitActionResponse {
                    result: Some(super::SubmitActionResult::Completed(
                        super::proto::CompletedOutcome {
                            content_json: "\"send success\"".to_string(),
                        },
                    )),
                }));
            }
            Ok(Response::new(super::proto::SubmitActionResponse {
                result: Some(super::SubmitActionResult::Streaming(
                    super::proto::StreamingOutcome {
                        accepted_content_json: "\"accepted\"".to_string(),
                        progress_content_json: "{\"percent\":50}".to_string(),
                        terminal_content_json: "{\"ok\":true}".to_string(),
                        has_accepted_content: true,
                        has_progress_content: true,
                    },
                )),
            }))
        }

        async fn submit_control_action(
            &self,
            request: Request<super::SubmitControlActionRequest>,
        ) -> Result<Response<super::proto::SubmitControlActionResponse>, Status> {
            let req = request.into_inner();
            let result = match req.action {
                Some(super::SubmitControlAction::PagingClose(close)) => {
                    super::SubmitControlResult::Completed(super::ControlCompletedOutcome {
                        content_json: format!(
                            "{{\"closed\":true,\"handle_id\":\"{}\"}}",
                            close.handle_id
                        ),
                    })
                }
                Some(super::SubmitControlAction::PagingFetchNext(fetch)) => {
                    super::SubmitControlResult::Completed(super::ControlCompletedOutcome {
                        content_json: format!(
                            "{{\"page\":{{\"handle_id\":\"{}\",\"page_no\":{},\"has_more\":{}}}}}",
                            fetch.handle_id,
                            fetch.page_no,
                            if fetch.has_more { "true" } else { "false" }
                        ),
                    })
                }
                None => super::SubmitControlResult::Error(super::proto::BridgeError {
                    code: "INVALID_ARGUMENT".to_string(),
                    message: "missing action".to_string(),
                    retryable: false,
                }),
            };

            Ok(Response::new(super::proto::SubmitControlActionResponse {
                result: Some(result),
            }))
        }
    }

    fn test_ctx() -> RequestContextV1 {
        RequestContextV1 {
            app_id: "aiim".to_string(),
            session_id: "sess-test".to_string(),
            request_id: "req-test".to_string(),
            client_token: Some("tok-1".to_string()),
        }
    }

    async fn spawn_test_grpc_server() -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test grpc listener");
        let addr = listener.local_addr().expect("read local addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    super::proto::appfs_adapter_bridge_server::AppfsAdapterBridgeServer::new(
                        TestBridgeService,
                    ),
                )
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("run test grpc server");
        });

        (addr, shutdown_tx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_adapter_roundtrip() {
        let (addr, shutdown) = spawn_test_grpc_server().await;
        let mut adapter = GrpcBridgeAdapterV1::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge adapter");

        let inline = adapter
            .submit_action(
                "/contacts/zhangsan/send_message.act",
                "hello\n",
                AdapterInputModeV1::Text,
                AdapterExecutionModeV1::Inline,
                &test_ctx(),
            )
            .expect("inline action");
        match inline {
            AdapterSubmitOutcomeV1::Completed { content } => {
                assert_eq!(content, "send success");
            }
            _ => panic!("expected completed"),
        }

        let streaming = adapter
            .submit_action(
                "/files/file-001/download.act",
                "{\"target\":\"/tmp/a.bin\"}",
                AdapterInputModeV1::Json,
                AdapterExecutionModeV1::Streaming,
                &test_ctx(),
            )
            .expect("streaming action");
        match streaming {
            AdapterSubmitOutcomeV1::Streaming { plan } => {
                assert_eq!(plan.accepted_content, Some(serde_json::json!("accepted")));
                assert_eq!(
                    plan.progress_content,
                    Some(serde_json::json!({ "percent": 50 }))
                );
                assert_eq!(plan.terminal_content, serde_json::json!({ "ok": true }));
            }
            _ => panic!("expected streaming"),
        }

        let control = adapter
            .submit_control_action(
                "/_paging/close.act",
                AdapterControlActionV1::PagingClose {
                    handle_id: "ph_abc".to_string(),
                },
                &test_ctx(),
            )
            .expect("control close");
        match control {
            AdapterControlOutcomeV1::Completed { content } => {
                assert_eq!(content["closed"], true);
                assert_eq!(content["handle_id"], "ph_abc");
            }
        }

        let _ = shutdown.send(());
    }
}
