use super::bridge_resilience::{
    is_retryable_grpc_code, BridgeCircuitBreaker, BridgeMetrics, BridgeRuntimeOptions,
};
use agentfs_sdk::{
    connector_error_codes_v2, ActionExecutionModeV2, ActionStreamingPlanV2, AdapterControlActionV1,
    AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1, AdapterInputModeV1,
    AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1, AppConnectorV2, AuthStatusV2,
    ConnectorContextV2, ConnectorErrorV2, ConnectorInfoV2, ConnectorTransportV2,
    FetchLivePageRequestV2, FetchLivePageResponseV2, FetchSnapshotChunkRequestV2,
    FetchSnapshotChunkResponseV2, HealthStatusV2, LiveModeV2, LivePageInfoV2, RequestContextV1,
    SnapshotMetaV2, SnapshotRecordV2, SnapshotResumeV2, SubmitActionOutcomeV2,
    SubmitActionRequestV2, SubmitActionResponseV2,
};
use agentfs_sdk::{
    AppConnectorV3, AppStructureNodeKindV3, AppStructureNodeV3, AppStructureSnapshotV3,
    AppStructureSyncReasonV3, AppStructureSyncResultV3, ConnectorContextV3, ConnectorErrorV3,
    GetAppStructureRequestV3, GetAppStructureResponseV3, RefreshAppStructureRequestV3,
    RefreshAppStructureResponseV3,
};
use serde_json::Value as JsonValue;
use std::future::Future;
use std::time::{Duration, Instant};
use tonic::transport::{Channel, Endpoint};

pub(super) mod proto {
    tonic::include_proto!("appfs.adapter.v1");
}
pub(super) mod proto_v2 {
    tonic::include_proto!("appfs.connector.v2");
}
pub(super) mod proto_v3 {
    tonic::include_proto!("appfs.connector.v3");
}

use proto::appfs_adapter_bridge_client::AppfsAdapterBridgeClient;
use proto::submit_action_response::Result as SubmitActionResult;
use proto::submit_control_action_request::Action as SubmitControlAction;
use proto::submit_control_action_response::Result as SubmitControlResult;
use proto::{
    ControlCompletedOutcome, ExecutionMode, InputMode, PagingCloseAction, PagingFetchNextAction,
    RequestContext, SubmitActionRequest, SubmitControlActionRequest,
};
use proto_v2::appfs_connector_v2_client::AppfsConnectorV2Client;
use proto_v3::appfs_connector_v3_client::AppfsConnectorV3Client;

#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct GrpcBridgeAdapterV1 {
    app_id: String,
    client: AppfsAdapterBridgeClient<Channel>,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

pub(super) struct GrpcBridgeConnectorV2 {
    app_id: String,
    client: AppfsConnectorV2Client<Channel>,
    client_v3: AppfsConnectorV3Client<Channel>,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

#[cfg_attr(not(test), allow(dead_code))]
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
        if attempts > 1 || outcome != "ok" || self.metrics.requests_total.is_multiple_of(50) {
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

impl GrpcBridgeConnectorV2 {
    pub(super) fn new(
        app_id: String,
        endpoint: String,
        timeout: Duration,
        runtime_options: BridgeRuntimeOptions,
    ) -> Result<Self, ConnectorErrorV2> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        let endpoint = Endpoint::from_shared(endpoint.clone()).map_err(|err| ConnectorErrorV2 {
            code: connector_error_codes_v2::INVALID_ARGUMENT.to_string(),
            message: format!("invalid grpc endpoint {endpoint}: {err}"),
            retryable: false,
            details: None,
        })?;

        let channel = endpoint
            .connect_timeout(timeout)
            .timeout(timeout)
            .tcp_nodelay(true)
            .connect_lazy();

        Ok(Self {
            app_id,
            client: AppfsConnectorV2Client::new(channel.clone()),
            client_v3: AppfsConnectorV3Client::new(channel),
            runtime_options,
            metrics: BridgeMetrics::default(),
            circuit_breaker: BridgeCircuitBreaker::default(),
        })
    }

    #[allow(clippy::result_large_err)]
    fn run_v2_rpc<Resp, F>(&mut self, method: &str, mut f: F) -> Result<Resp, ConnectorErrorV2>
    where
        F: FnMut(AppfsConnectorV2Client<Channel>) -> Result<tonic::Response<Resp>, tonic::Status>,
    {
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            return Err(ConnectorErrorV2 {
                code: connector_error_codes_v2::INTERNAL.to_string(),
                message: format!(
                    "bridge grpc circuit open for {method}; retry_in_ms={} metrics={}",
                    remaining.as_millis(),
                    self.metrics.snapshot()
                ),
                retryable: true,
                details: None,
            });
        }

        let max_attempts = self.runtime_options.max_retries.saturating_add(1).max(1);
        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let client = self.client.clone();
            match f(client) {
                Ok(response) => {
                    self.circuit_breaker.record_success();
                    self.metrics.record_request(attempt, true);
                    self.log_observation(method, attempt, started.elapsed(), "ok");
                    return Ok(response.into_inner());
                }
                Err(status) => {
                    let retryable = is_retryable_grpc_code(status.code());
                    if retryable && attempt < max_attempts {
                        let backoff = self.runtime_options.retry_backoff_for_attempt(attempt);
                        std::thread::sleep(backoff);
                        continue;
                    }

                    if retryable {
                        let opened = self
                            .circuit_breaker
                            .record_failure(Instant::now(), self.runtime_options);
                        if opened {
                            eprintln!(
                                "AppFS bridge grpc circuit opened after {} failure code={} {}",
                                method,
                                status.code(),
                                self.metrics.snapshot()
                            );
                        }
                    } else {
                        self.circuit_breaker.record_success();
                    }

                    self.metrics.record_request(attempt, false);
                    self.log_observation(method, attempt, started.elapsed(), "failed");
                    return Err(map_grpc_status_v2(
                        method,
                        status,
                        attempt,
                        &self.metrics.snapshot(),
                    ));
                }
            }
        }
    }

    fn log_observation(&self, method: &str, attempts: u32, elapsed: Duration, outcome: &str) {
        if attempts > 1 || outcome != "ok" || self.metrics.requests_total.is_multiple_of(50) {
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

    #[allow(clippy::result_large_err)]
    fn run_v3_rpc<Resp, F>(&mut self, method: &str, mut f: F) -> Result<Resp, ConnectorErrorV3>
    where
        F: FnMut(AppfsConnectorV3Client<Channel>) -> Result<tonic::Response<Resp>, tonic::Status>,
    {
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            return Err(ConnectorErrorV3 {
                code: connector_error_codes_v2::INTERNAL.to_string(),
                message: format!(
                    "bridge grpc circuit open for {method}; retry_in_ms={} metrics={}",
                    remaining.as_millis(),
                    self.metrics.snapshot()
                ),
                retryable: true,
                details: None,
            });
        }

        let max_attempts = self.runtime_options.max_retries.saturating_add(1).max(1);
        let started = Instant::now();
        let mut attempt = 0u32;
        loop {
            attempt = attempt.saturating_add(1);
            let client = self.client_v3.clone();
            match f(client) {
                Ok(response) => {
                    self.circuit_breaker.record_success();
                    self.metrics.record_request(attempt, true);
                    self.log_observation(method, attempt, started.elapsed(), "ok");
                    return Ok(response.into_inner());
                }
                Err(status) => {
                    let retryable = is_retryable_grpc_code(status.code());
                    if retryable && attempt < max_attempts {
                        let backoff = self.runtime_options.retry_backoff_for_attempt(attempt);
                        std::thread::sleep(backoff);
                        continue;
                    }

                    if retryable {
                        let opened = self
                            .circuit_breaker
                            .record_failure(Instant::now(), self.runtime_options);
                        if opened {
                            eprintln!(
                                "AppFS bridge grpc circuit opened after {} failure code={} {}",
                                method,
                                status.code(),
                                self.metrics.snapshot()
                            );
                        }
                    } else {
                        self.circuit_breaker.record_success();
                    }

                    self.metrics.record_request(attempt, false);
                    self.log_observation(method, attempt, started.elapsed(), "failed");
                    return Err(map_grpc_status_v3(
                        method,
                        status,
                        attempt,
                        &self.metrics.snapshot(),
                    ));
                }
            }
        }
    }

    fn ensure_app_match(&self, app_id: &str) -> Result<(), ConnectorErrorV2> {
        if app_id != self.app_id {
            return Err(ConnectorErrorV2 {
                code: connector_error_codes_v2::INVALID_ARGUMENT.to_string(),
                message: format!(
                    "grpc connector app_id mismatch expected={} got={}",
                    self.app_id, app_id
                ),
                retryable: false,
                details: None,
            });
        }
        Ok(())
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

impl AppConnectorV2 for GrpcBridgeConnectorV2 {
    fn connector_id(&self) -> Result<ConnectorInfoV2, ConnectorErrorV2> {
        let mut client = self.client.clone();
        let response = run_async(client.get_connector_info(proto_v2::GetConnectorInfoRequest {}))
            .map_err(|status| map_grpc_status_v2("GetConnectorInfo", status, 1, "n/a"))?
            .into_inner();
        match response.result {
            Some(proto_v2::get_connector_info_response::Result::Info(info)) => {
                let app_id = info.app_id.clone();
                self.ensure_app_match(&app_id)?;
                from_proto_connector_info(info)
            }
            Some(proto_v2::get_connector_info_response::Result::Error(err)) => {
                Err(from_proto_connector_error(err))
            }
            None => Err(ConnectorErrorV2 {
                code: connector_error_codes_v2::INTERNAL.to_string(),
                message: "bridge grpc GetConnectorInfo returned empty result".to_string(),
                retryable: true,
                details: None,
            }),
        }
    }

    #[allow(clippy::result_large_err)]
    fn health(&mut self, ctx: &ConnectorContextV2) -> Result<HealthStatusV2, ConnectorErrorV2> {
        let req = proto_v2::HealthRequest {
            context: Some(to_proto_context_v2(ctx)),
        };
        let response =
            self.run_v2_rpc("Health", |mut client| run_async(client.health(req.clone())))?;
        match response.result {
            Some(proto_v2::health_response::Result::Status(status)) => {
                from_proto_health_status(status)
            }
            Some(proto_v2::health_response::Result::Error(err)) => {
                Err(from_proto_connector_error(err))
            }
            None => Err(empty_result_error("Health")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: Duration,
        ctx: &ConnectorContextV2,
    ) -> Result<SnapshotMetaV2, ConnectorErrorV2> {
        let timeout_ms = timeout.as_millis().max(1).min(u128::from(u64::MAX)) as u64;
        let req = proto_v2::PrewarmSnapshotMetaRequest {
            context: Some(to_proto_context_v2(ctx)),
            resource_path: resource_path.to_string(),
            timeout_ms,
        };
        let response = self.run_v2_rpc("PrewarmSnapshotMeta", |mut client| {
            run_async(client.prewarm_snapshot_meta(req.clone()))
        })?;
        match response.result {
            Some(proto_v2::prewarm_snapshot_meta_response::Result::Meta(meta)) => {
                Ok(from_proto_snapshot_meta(meta))
            }
            Some(proto_v2::prewarm_snapshot_meta_response::Result::Error(err)) => {
                Err(from_proto_connector_error(err))
            }
            None => Err(empty_result_error("PrewarmSnapshotMeta")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequestV2,
        ctx: &ConnectorContextV2,
    ) -> Result<FetchSnapshotChunkResponseV2, ConnectorErrorV2> {
        let req = proto_v2::FetchSnapshotChunkRequest {
            context: Some(to_proto_context_v2(ctx)),
            request: Some(to_proto_fetch_snapshot_chunk_request(request)),
        };
        let response = self.run_v2_rpc("FetchSnapshotChunk", |mut client| {
            run_async(client.fetch_snapshot_chunk(req.clone()))
        })?;
        match response.result {
            Some(proto_v2::fetch_snapshot_chunk_response::Result::Response(resp)) => {
                from_proto_fetch_snapshot_chunk_response(resp)
            }
            Some(proto_v2::fetch_snapshot_chunk_response::Result::Error(err)) => {
                Err(from_proto_connector_error(err))
            }
            None => Err(empty_result_error("FetchSnapshotChunk")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequestV2,
        ctx: &ConnectorContextV2,
    ) -> Result<FetchLivePageResponseV2, ConnectorErrorV2> {
        let req = proto_v2::FetchLivePageRequest {
            context: Some(to_proto_context_v2(ctx)),
            request: Some(to_proto_fetch_live_page_request(request)),
        };
        let response = self.run_v2_rpc("FetchLivePage", |mut client| {
            run_async(client.fetch_live_page(req.clone()))
        })?;
        match response.result {
            Some(proto_v2::fetch_live_page_response::Result::Response(resp)) => {
                from_proto_fetch_live_page_response(resp)
            }
            Some(proto_v2::fetch_live_page_response::Result::Error(err)) => {
                Err(from_proto_connector_error(err))
            }
            None => Err(empty_result_error("FetchLivePage")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn submit_action(
        &mut self,
        request: SubmitActionRequestV2,
        ctx: &ConnectorContextV2,
    ) -> Result<SubmitActionResponseV2, ConnectorErrorV2> {
        let req = proto_v2::SubmitActionRequest {
            context: Some(to_proto_context_v2(ctx)),
            request: Some(to_proto_submit_action_request(request)?),
        };
        let response = self.run_v2_rpc("SubmitAction", |mut client| {
            run_async(client.submit_action(req.clone()))
        })?;
        match response.result {
            Some(proto_v2::submit_action_response::Result::Response(resp)) => {
                from_proto_submit_action_response(resp)
            }
            Some(proto_v2::submit_action_response::Result::Error(err)) => {
                Err(from_proto_connector_error(err))
            }
            None => Err(empty_result_error("SubmitAction")),
        }
    }
}

impl AppConnectorV3 for GrpcBridgeConnectorV2 {
    fn get_app_structure(
        &mut self,
        request: GetAppStructureRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<GetAppStructureResponseV3, ConnectorErrorV3> {
        if request.app_id != self.app_id {
            return Err(ConnectorErrorV3 {
                code: connector_error_codes_v2::INVALID_ARGUMENT.to_string(),
                message: format!(
                    "grpc structure connector app_id mismatch expected={} got={}",
                    self.app_id, request.app_id
                ),
                retryable: false,
                details: None,
            });
        }
        let req = proto_v3::GetAppStructureRequest {
            context: Some(to_proto_context_v3(ctx)),
            request: Some(to_proto_get_app_structure_request_v3(request)),
        };
        let response = self.run_v3_rpc("GetAppStructure", |mut client| {
            run_async(client.get_app_structure(req.clone()))
        })?;
        match response.result {
            Some(proto_v3::get_app_structure_response::Result::Response(resp)) => {
                from_proto_get_app_structure_response_v3(resp)
            }
            Some(proto_v3::get_app_structure_response::Result::Error(err)) => {
                Err(from_proto_connector_error_v3(err))
            }
            None => Err(empty_result_error_v3("GetAppStructure")),
        }
    }

    fn refresh_app_structure(
        &mut self,
        request: RefreshAppStructureRequestV3,
        ctx: &ConnectorContextV3,
    ) -> Result<RefreshAppStructureResponseV3, ConnectorErrorV3> {
        if request.app_id != self.app_id {
            return Err(ConnectorErrorV3 {
                code: connector_error_codes_v2::INVALID_ARGUMENT.to_string(),
                message: format!(
                    "grpc structure connector app_id mismatch expected={} got={}",
                    self.app_id, request.app_id
                ),
                retryable: false,
                details: None,
            });
        }
        let req = proto_v3::RefreshAppStructureRequest {
            context: Some(to_proto_context_v3(ctx)),
            request: Some(to_proto_refresh_app_structure_request_v3(request)),
        };
        let response = self.run_v3_rpc("RefreshAppStructure", |mut client| {
            run_async(client.refresh_app_structure(req.clone()))
        })?;
        match response.result {
            Some(proto_v3::refresh_app_structure_response::Result::Response(resp)) => {
                from_proto_refresh_app_structure_response_v3(resp)
            }
            Some(proto_v3::refresh_app_structure_response::Result::Error(err)) => {
                Err(from_proto_connector_error_v3(err))
            }
            None => Err(empty_result_error_v3("RefreshAppStructure")),
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn to_proto_input_mode(mode: AdapterInputModeV1) -> InputMode {
    match mode {
        AdapterInputModeV1::Text => InputMode::Text,
        AdapterInputModeV1::Json => InputMode::Json,
        AdapterInputModeV1::TextOrJson => InputMode::TextOrJson,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn to_proto_execution_mode(mode: AdapterExecutionModeV1) -> ExecutionMode {
    match mode {
        AdapterExecutionModeV1::Inline => ExecutionMode::Inline,
        AdapterExecutionModeV1::Streaming => ExecutionMode::Streaming,
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn to_proto_context(ctx: &RequestContextV1) -> RequestContext {
    RequestContext {
        app_id: ctx.app_id.clone(),
        session_id: ctx.session_id.clone(),
        request_id: ctx.request_id.clone(),
        client_token: ctx.client_token.clone().unwrap_or_default(),
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn parse_json_text(text: &str, field: &str) -> Result<JsonValue, AdapterErrorV1> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| AdapterErrorV1::Internal {
        message: format!("bridge grpc invalid json in {field}: {err}"),
    })
}

#[cfg_attr(not(test), allow(dead_code))]
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

fn map_grpc_status_v2(
    method: &str,
    status: tonic::Status,
    attempts: u32,
    metrics: &str,
) -> ConnectorErrorV2 {
    ConnectorErrorV2 {
        code: if is_retryable_grpc_code(status.code()) {
            connector_error_codes_v2::UPSTREAM_UNAVAILABLE.to_string()
        } else {
            connector_error_codes_v2::INTERNAL.to_string()
        },
        message: format!(
            "bridge grpc {} error: code={} message={} attempts={} metrics={}",
            method,
            status.code(),
            status.message(),
            attempts,
            metrics
        ),
        retryable: is_retryable_grpc_code(status.code()),
        details: None,
    }
}

fn empty_result_error(method: &str) -> ConnectorErrorV2 {
    ConnectorErrorV2 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: format!("bridge grpc {} returned empty result", method),
        retryable: true,
        details: None,
    }
}

fn empty_result_error_v3(method: &str) -> ConnectorErrorV3 {
    ConnectorErrorV3 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: format!("bridge grpc {} returned empty result", method),
        retryable: true,
        details: None,
    }
}

fn map_grpc_status_v3(
    method: &str,
    status: tonic::Status,
    attempts: u32,
    metrics: &str,
) -> ConnectorErrorV3 {
    ConnectorErrorV3 {
        code: if is_retryable_grpc_code(status.code()) {
            connector_error_codes_v2::UPSTREAM_UNAVAILABLE.to_string()
        } else {
            connector_error_codes_v2::INTERNAL.to_string()
        },
        message: format!(
            "bridge grpc {} error: code={} message={} attempts={} metrics={}",
            method,
            status.code(),
            status.message(),
            attempts,
            metrics
        ),
        retryable: is_retryable_grpc_code(status.code()),
        details: None,
    }
}

fn to_proto_context_v2(ctx: &ConnectorContextV2) -> proto_v2::ConnectorContextV2 {
    proto_v2::ConnectorContextV2 {
        app_id: ctx.app_id.clone(),
        session_id: ctx.session_id.clone(),
        request_id: ctx.request_id.clone(),
        client_token: ctx.client_token.clone(),
        trace_id: ctx.trace_id.clone(),
    }
}

fn to_proto_context_v3(ctx: &ConnectorContextV3) -> proto_v3::ConnectorContextV3 {
    proto_v3::ConnectorContextV3 {
        app_id: ctx.app_id.clone(),
        session_id: ctx.session_id.clone(),
        request_id: ctx.request_id.clone(),
        client_token: ctx.client_token.clone(),
        trace_id: ctx.trace_id.clone(),
    }
}

fn to_proto_get_app_structure_request_v3(
    request: GetAppStructureRequestV3,
) -> proto_v3::GetAppStructureRequestV3 {
    proto_v3::GetAppStructureRequestV3 {
        app_id: request.app_id,
        known_revision: request.known_revision,
    }
}

fn to_proto_refresh_app_structure_request_v3(
    request: RefreshAppStructureRequestV3,
) -> proto_v3::RefreshAppStructureRequestV3 {
    let reason = match request.reason {
        AppStructureSyncReasonV3::Initialize => {
            proto_v3::AppStructureSyncReasonV3::Initialize as i32
        }
        AppStructureSyncReasonV3::EnterScope => {
            proto_v3::AppStructureSyncReasonV3::EnterScope as i32
        }
        AppStructureSyncReasonV3::Refresh => proto_v3::AppStructureSyncReasonV3::Refresh as i32,
        AppStructureSyncReasonV3::Recover => proto_v3::AppStructureSyncReasonV3::Recover as i32,
    };
    proto_v3::RefreshAppStructureRequestV3 {
        app_id: request.app_id,
        known_revision: request.known_revision,
        reason,
        target_scope: request.target_scope,
        trigger_action_path: request.trigger_action_path,
    }
}

fn from_proto_connector_info(
    info: proto_v2::ConnectorInfoV2,
) -> Result<ConnectorInfoV2, ConnectorErrorV2> {
    let transport = proto_v2::ConnectorTransportV2::try_from(info.transport).map_err(|_| {
        malformed_payload(
            "connector_info.transport",
            format!("unknown enum value {}", info.transport),
        )
    })?;
    let transport = match transport {
        proto_v2::ConnectorTransportV2::InProcess => ConnectorTransportV2::InProcess,
        proto_v2::ConnectorTransportV2::HttpBridge => ConnectorTransportV2::HttpBridge,
        proto_v2::ConnectorTransportV2::GrpcBridge => ConnectorTransportV2::GrpcBridge,
        proto_v2::ConnectorTransportV2::Unspecified => {
            return Err(malformed_payload(
                "connector_info.transport",
                "UNSPECIFIED enum value is not allowed",
            ))
        }
    };
    Ok(ConnectorInfoV2 {
        connector_id: info.connector_id,
        version: info.version,
        app_id: info.app_id,
        transport,
        supports_snapshot: info.supports_snapshot,
        supports_live: info.supports_live,
        supports_action: info.supports_action,
        optional_features: info.optional_features,
    })
}

fn from_proto_health_status(
    status: proto_v2::HealthStatusV2,
) -> Result<HealthStatusV2, ConnectorErrorV2> {
    let auth = proto_v2::AuthStatusV2::try_from(status.auth_status).map_err(|_| {
        malformed_payload(
            "health.auth_status",
            format!("unknown enum value {}", status.auth_status),
        )
    })?;
    let auth_status = match auth {
        proto_v2::AuthStatusV2::Valid => AuthStatusV2::Valid,
        proto_v2::AuthStatusV2::Expired => AuthStatusV2::Expired,
        proto_v2::AuthStatusV2::Refreshing => AuthStatusV2::Refreshing,
        proto_v2::AuthStatusV2::Invalid => AuthStatusV2::Invalid,
        proto_v2::AuthStatusV2::Unspecified => {
            return Err(malformed_payload(
                "health.auth_status",
                "UNSPECIFIED enum value is not allowed",
            ))
        }
    };
    Ok(HealthStatusV2 {
        healthy: status.healthy,
        auth_status,
        message: status.message,
        checked_at: status.checked_at,
    })
}

fn from_proto_snapshot_meta(meta: proto_v2::SnapshotMetaV2) -> SnapshotMetaV2 {
    SnapshotMetaV2 {
        size_bytes: meta.size_bytes,
        revision: meta.revision,
        last_modified: meta.last_modified,
        item_count: meta.item_count,
    }
}

fn to_proto_fetch_snapshot_chunk_request(
    request: FetchSnapshotChunkRequestV2,
) -> proto_v2::FetchSnapshotChunkRequestV2 {
    let resume = match request.resume {
        SnapshotResumeV2::Start => proto_v2::SnapshotResumeV2 {
            kind: Some(proto_v2::snapshot_resume_v2::Kind::Start(true)),
        },
        SnapshotResumeV2::Cursor(cursor) => proto_v2::SnapshotResumeV2 {
            kind: Some(proto_v2::snapshot_resume_v2::Kind::Cursor(cursor)),
        },
        SnapshotResumeV2::Offset(offset) => proto_v2::SnapshotResumeV2 {
            kind: Some(proto_v2::snapshot_resume_v2::Kind::Offset(offset)),
        },
    };
    proto_v2::FetchSnapshotChunkRequestV2 {
        resource_path: request.resource_path,
        resume: Some(resume),
        budget_bytes: request.budget_bytes,
    }
}

fn from_proto_fetch_snapshot_chunk_response(
    response: proto_v2::FetchSnapshotChunkResponseV2,
) -> Result<FetchSnapshotChunkResponseV2, ConnectorErrorV2> {
    let mut records = Vec::with_capacity(response.records.len());
    for record in response.records {
        if record.line_json.trim().is_empty() {
            return Err(ConnectorErrorV2 {
                code: connector_error_codes_v2::INTERNAL.to_string(),
                message: "snapshot record missing line".to_string(),
                retryable: true,
                details: None,
            });
        }
        records.push(SnapshotRecordV2 {
            record_key: record.record_key,
            ordering_key: record.ordering_key,
            line: parse_json_object_text_v2(&record.line_json, "snapshot.records[].line_json")?,
        });
    }
    Ok(FetchSnapshotChunkResponseV2 {
        records,
        emitted_bytes: response.emitted_bytes,
        next_cursor: parse_optional_cursor_v2(response.next_cursor, "snapshot.next_cursor")?,
        has_more: response.has_more,
        revision: response.revision,
    })
}

fn to_proto_fetch_live_page_request(
    request: FetchLivePageRequestV2,
) -> proto_v2::FetchLivePageRequestV2 {
    proto_v2::FetchLivePageRequestV2 {
        resource_path: request.resource_path,
        handle_id: request.handle_id,
        cursor: request.cursor,
        page_size: request.page_size,
    }
}

fn from_proto_fetch_live_page_response(
    response: proto_v2::FetchLivePageResponseV2,
) -> Result<FetchLivePageResponseV2, ConnectorErrorV2> {
    let page = response.page.ok_or_else(|| ConnectorErrorV2 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: "live page response missing page".to_string(),
        retryable: true,
        details: None,
    })?;
    let mut items = Vec::with_capacity(response.items_json.len());
    for item in response.items_json {
        items.push(parse_json_text_v2(&item, "live.items_json[]")?);
    }
    Ok(FetchLivePageResponseV2 {
        items,
        page: LivePageInfoV2 {
            handle_id: page.handle_id,
            page_no: page.page_no,
            has_more: page.has_more,
            mode: match proto_v2::LiveModeV2::try_from(page.mode).map_err(|_| {
                malformed_payload(
                    "live.page.mode",
                    format!("unknown enum value {}", page.mode),
                )
            })? {
                proto_v2::LiveModeV2::Live => LiveModeV2::Live,
                proto_v2::LiveModeV2::Unspecified => {
                    return Err(malformed_payload(
                        "live.page.mode",
                        "UNSPECIFIED enum value is not allowed",
                    ))
                }
            },
            expires_at: page.expires_at,
            next_cursor: parse_optional_cursor_v2(page.next_cursor, "live.page.next_cursor")?,
            retry_after_ms: page.retry_after_ms,
        },
    })
}

fn to_proto_submit_action_request(
    request: SubmitActionRequestV2,
) -> Result<proto_v2::SubmitActionRequestV2, ConnectorErrorV2> {
    Ok(proto_v2::SubmitActionRequestV2 {
        path: request.path,
        payload_json: serde_json::to_string(&request.payload).map_err(|err| ConnectorErrorV2 {
            code: connector_error_codes_v2::INVALID_PAYLOAD.to_string(),
            message: format!("invalid submit_action payload: {err}"),
            retryable: false,
            details: None,
        })?,
        execution_mode: match request.execution_mode {
            ActionExecutionModeV2::Inline => proto_v2::ActionExecutionModeV2::Inline as i32,
            ActionExecutionModeV2::Streaming => proto_v2::ActionExecutionModeV2::Streaming as i32,
        },
    })
}

fn from_proto_submit_action_response(
    response: proto_v2::SubmitActionResponseV2,
) -> Result<SubmitActionResponseV2, ConnectorErrorV2> {
    let outcome = response.outcome.ok_or_else(|| ConnectorErrorV2 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: "submit action response missing outcome".to_string(),
        retryable: true,
        details: None,
    })?;
    let mapped = match outcome.kind {
        Some(proto_v2::submit_action_outcome_v2::Kind::CompletedContentJson(content)) => {
            SubmitActionOutcomeV2::Completed {
                content: parse_json_text_v2(&content, "submit_action.completed_content_json")?,
            }
        }
        Some(proto_v2::submit_action_outcome_v2::Kind::StreamingPlan(plan)) => {
            if plan.terminal_content_json.trim().is_empty() {
                return Err(ConnectorErrorV2 {
                    code: connector_error_codes_v2::INTERNAL.to_string(),
                    message: "streaming plan missing terminal_content_json".to_string(),
                    retryable: true,
                    details: None,
                });
            }
            let terminal = parse_json_text_v2(
                &plan.terminal_content_json,
                "submit_action.streaming.terminal_content_json",
            )?;
            let accepted = if plan.has_accepted_content {
                Some(parse_json_text_v2(
                    &plan.accepted_content_json,
                    "submit_action.streaming.accepted_content_json",
                )?)
            } else {
                None
            };
            let progress = if plan.has_progress_content {
                Some(parse_json_text_v2(
                    &plan.progress_content_json,
                    "submit_action.streaming.progress_content_json",
                )?)
            } else {
                None
            };
            SubmitActionOutcomeV2::Streaming {
                plan: ActionStreamingPlanV2 {
                    accepted_content: accepted,
                    progress_content: progress,
                    terminal_content: terminal,
                },
            }
        }
        None => {
            return Err(ConnectorErrorV2 {
                code: connector_error_codes_v2::INTERNAL.to_string(),
                message: "submit action outcome kind is empty".to_string(),
                retryable: true,
                details: None,
            });
        }
    };

    Ok(SubmitActionResponseV2 {
        request_id: response.request_id,
        estimated_duration_ms: response.estimated_duration_ms,
        outcome: mapped,
    })
}

fn from_proto_connector_error(err: proto_v2::ConnectorErrorV2) -> ConnectorErrorV2 {
    ConnectorErrorV2 {
        code: err.code,
        message: err.message,
        retryable: err.retryable,
        details: err.details,
    }
}

fn from_proto_connector_error_v3(err: proto_v3::ConnectorErrorV3) -> ConnectorErrorV3 {
    ConnectorErrorV3 {
        code: err.code,
        message: err.message,
        retryable: err.retryable,
        details: err.details,
    }
}

fn from_proto_get_app_structure_response_v3(
    response: proto_v3::AppStructureSyncResultV3,
) -> Result<GetAppStructureResponseV3, ConnectorErrorV3> {
    Ok(GetAppStructureResponseV3 {
        result: from_proto_structure_sync_result_v3(response)?,
    })
}

fn from_proto_refresh_app_structure_response_v3(
    response: proto_v3::AppStructureSyncResultV3,
) -> Result<RefreshAppStructureResponseV3, ConnectorErrorV3> {
    Ok(RefreshAppStructureResponseV3 {
        result: from_proto_structure_sync_result_v3(response)?,
    })
}

fn from_proto_structure_sync_result_v3(
    response: proto_v3::AppStructureSyncResultV3,
) -> Result<AppStructureSyncResultV3, ConnectorErrorV3> {
    match response.kind {
        Some(proto_v3::app_structure_sync_result_v3::Kind::Unchanged(unchanged)) => {
            Ok(AppStructureSyncResultV3::Unchanged {
                app_id: unchanged.app_id,
                revision: unchanged.revision,
                active_scope: unchanged.active_scope,
            })
        }
        Some(proto_v3::app_structure_sync_result_v3::Kind::Snapshot(snapshot)) => {
            Ok(AppStructureSyncResultV3::Snapshot {
                snapshot: from_proto_structure_snapshot_v3(snapshot.snapshot.ok_or_else(
                    || {
                        malformed_payload_v3(
                            "structure.result.snapshot.snapshot",
                            "snapshot payload is required",
                        )
                    },
                )?)?,
            })
        }
        None => Err(malformed_payload_v3(
            "structure.result.kind",
            "result kind is empty",
        )),
    }
}

fn from_proto_structure_snapshot_v3(
    snapshot: proto_v3::AppStructureSnapshotV3,
) -> Result<AppStructureSnapshotV3, ConnectorErrorV3> {
    if snapshot.revision.trim().is_empty() {
        return Err(malformed_payload_v3(
            "structure.snapshot.revision",
            "revision must be non-empty",
        ));
    }
    let mut nodes = Vec::with_capacity(snapshot.nodes.len());
    for node in snapshot.nodes {
        nodes.push(from_proto_structure_node_v3(node)?);
    }
    Ok(AppStructureSnapshotV3 {
        app_id: snapshot.app_id,
        revision: snapshot.revision,
        active_scope: snapshot.active_scope,
        ownership_prefixes: snapshot.ownership_prefixes,
        nodes,
    })
}

fn from_proto_structure_node_v3(
    node: proto_v3::AppStructureNodeV3,
) -> Result<AppStructureNodeV3, ConnectorErrorV3> {
    if node.path.trim().is_empty() {
        return Err(malformed_payload_v3(
            "structure.snapshot.nodes[].path",
            "path must be non-empty",
        ));
    }
    let kind = match proto_v3::AppStructureNodeKindV3::try_from(node.kind).map_err(|_| {
        malformed_payload_v3(
            "structure.snapshot.nodes[].kind",
            format!("unknown enum value {}", node.kind),
        )
    })? {
        proto_v3::AppStructureNodeKindV3::Directory => AppStructureNodeKindV3::Directory,
        proto_v3::AppStructureNodeKindV3::ActionFile => AppStructureNodeKindV3::ActionFile,
        proto_v3::AppStructureNodeKindV3::SnapshotResource => {
            AppStructureNodeKindV3::SnapshotResource
        }
        proto_v3::AppStructureNodeKindV3::LiveResource => AppStructureNodeKindV3::LiveResource,
        proto_v3::AppStructureNodeKindV3::StaticJsonResource => {
            AppStructureNodeKindV3::StaticJsonResource
        }
        proto_v3::AppStructureNodeKindV3::Unspecified => {
            return Err(malformed_payload_v3(
                "structure.snapshot.nodes[].kind",
                "UNSPECIFIED enum value is not allowed",
            ))
        }
    };
    let manifest_entry = match node.manifest_entry_json {
        Some(json) => Some(parse_json_text_v3(
            &json,
            "structure.snapshot.nodes[].manifest_entry_json",
        )?),
        None => None,
    };
    let seed_content = match node.seed_content_json {
        Some(json) => Some(parse_json_text_v3(
            &json,
            "structure.snapshot.nodes[].seed_content_json",
        )?),
        None => None,
    };
    Ok(AppStructureNodeV3 {
        path: node.path,
        kind,
        manifest_entry,
        seed_content,
        mutable: node.r#mutable,
        scope: node.scope,
    })
}

fn parse_json_text_v2(text: &str, field: &str) -> Result<JsonValue, ConnectorErrorV2> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| ConnectorErrorV2 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: format!("bridge grpc invalid json in {field}: {err}"),
        retryable: true,
        details: None,
    })
}

fn parse_json_text_v3(text: &str, field: &str) -> Result<JsonValue, ConnectorErrorV3> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| ConnectorErrorV3 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: format!("bridge grpc invalid json in {field}: {err}"),
        retryable: true,
        details: None,
    })
}

fn parse_json_object_text_v2(text: &str, field: &str) -> Result<JsonValue, ConnectorErrorV2> {
    let value = parse_json_text_v2(text, field)?;
    if !value.is_object() {
        return Err(malformed_payload(
            field,
            "expected JSON object, got non-object JSON",
        ));
    }
    Ok(value)
}

fn parse_optional_cursor_v2(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, ConnectorErrorV2> {
    match value {
        Some(s) if s.is_empty() => Err(malformed_payload(
            field,
            "empty string is not allowed for optional cursor",
        )),
        Some(s) => Ok(Some(s)),
        None => Ok(None),
    }
}

fn malformed_payload(field: &str, reason: impl std::fmt::Display) -> ConnectorErrorV2 {
    ConnectorErrorV2 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: format!("bridge grpc malformed payload in {field}: {reason}"),
        retryable: false,
        details: None,
    }
}

fn malformed_payload_v3(field: &str, reason: impl std::fmt::Display) -> ConnectorErrorV3 {
    ConnectorErrorV3 {
        code: connector_error_codes_v2::INTERNAL.to_string(),
        message: format!("bridge grpc malformed payload in {field}: {reason}"),
        retryable: false,
        details: None,
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
        GrpcBridgeConnectorV2,
    };
    use agentfs_sdk::{
        ActionExecutionModeV2, AdapterControlActionV1, AdapterControlOutcomeV1,
        AdapterExecutionModeV1, AdapterInputModeV1, AdapterSubmitOutcomeV1, AppAdapterV1,
        AppConnectorV2, AppConnectorV3, AppStructureSyncReasonV3, ConnectorContextV2,
        FetchLivePageRequestV2, FetchSnapshotChunkRequestV2, GetAppStructureRequestV3,
        RefreshAppStructureRequestV3, RequestContextV1, SnapshotResumeV2, SubmitActionOutcomeV2,
        SubmitActionRequestV2,
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

    #[derive(Default)]
    struct TestConnectorV2Service;

    #[derive(Default)]
    struct TestConnectorV3Service;

    #[tonic::async_trait]
    impl super::proto_v2::appfs_connector_v2_server::AppfsConnectorV2 for TestConnectorV2Service {
        async fn get_connector_info(
            &self,
            _request: Request<super::proto_v2::GetConnectorInfoRequest>,
        ) -> Result<Response<super::proto_v2::GetConnectorInfoResponse>, Status> {
            Ok(Response::new(super::proto_v2::GetConnectorInfoResponse {
                result: Some(super::proto_v2::get_connector_info_response::Result::Info(
                    super::proto_v2::ConnectorInfoV2 {
                        connector_id: "mock-grpc-v2".to_string(),
                        version: "0.3.0-test".to_string(),
                        app_id: "aiim".to_string(),
                        transport: super::proto_v2::ConnectorTransportV2::GrpcBridge as i32,
                        supports_snapshot: true,
                        supports_live: true,
                        supports_action: true,
                        optional_features: vec!["demo_mode".to_string()],
                    },
                )),
            }))
        }

        async fn health(
            &self,
            _request: Request<super::proto_v2::HealthRequest>,
        ) -> Result<Response<super::proto_v2::HealthResponse>, Status> {
            Ok(Response::new(super::proto_v2::HealthResponse {
                result: Some(super::proto_v2::health_response::Result::Status(
                    super::proto_v2::HealthStatusV2 {
                        healthy: true,
                        auth_status: super::proto_v2::AuthStatusV2::Valid as i32,
                        message: Some("ok".to_string()),
                        checked_at: "2026-03-24T00:00:00Z".to_string(),
                    },
                )),
            }))
        }

        async fn prewarm_snapshot_meta(
            &self,
            _request: Request<super::proto_v2::PrewarmSnapshotMetaRequest>,
        ) -> Result<Response<super::proto_v2::PrewarmSnapshotMetaResponse>, Status> {
            Ok(Response::new(
                super::proto_v2::PrewarmSnapshotMetaResponse {
                    result: Some(
                        super::proto_v2::prewarm_snapshot_meta_response::Result::Meta(
                            super::proto_v2::SnapshotMetaV2 {
                                size_bytes: Some(1024),
                                revision: Some("rev-1".to_string()),
                                last_modified: Some("2026-03-24T00:00:00Z".to_string()),
                                item_count: Some(1),
                            },
                        ),
                    ),
                },
            ))
        }

        async fn fetch_snapshot_chunk(
            &self,
            request: Request<super::proto_v2::FetchSnapshotChunkRequest>,
        ) -> Result<Response<super::proto_v2::FetchSnapshotChunkResponse>, Status> {
            let inner = request.into_inner();
            let response = match inner.request {
                Some(req) => match req
                    .resume
                    .and_then(|resume| resume.kind)
                    .unwrap_or(super::proto_v2::snapshot_resume_v2::Kind::Start(true))
                {
                    super::proto_v2::snapshot_resume_v2::Kind::Start(_) => {
                        super::proto_v2::FetchSnapshotChunkResponse {
                            result: Some(
                                super::proto_v2::fetch_snapshot_chunk_response::Result::Response(
                                    super::proto_v2::FetchSnapshotChunkResponseV2 {
                                        records: vec![super::proto_v2::SnapshotRecordV2 {
                                            record_key: "rk-1".to_string(),
                                            ordering_key: "ok-1".to_string(),
                                            line_json: "{\"id\":\"m-1\"}".to_string(),
                                        }],
                                        emitted_bytes: 16,
                                        next_cursor: Some("cursor-1".to_string()),
                                        has_more: true,
                                        revision: Some("rev-1".to_string()),
                                    },
                                ),
                            ),
                        }
                    }
                    super::proto_v2::snapshot_resume_v2::Kind::Cursor(cursor) => {
                        if cursor != "cursor-1" {
                            super::proto_v2::FetchSnapshotChunkResponse {
                                result: Some(
                                    super::proto_v2::fetch_snapshot_chunk_response::Result::Error(
                                        super::proto_v2::ConnectorErrorV2 {
                                            code: "INVALID_ARGUMENT".to_string(),
                                            message: "unknown cursor".to_string(),
                                            retryable: false,
                                            details: None,
                                        },
                                    ),
                                ),
                            }
                        } else {
                            super::proto_v2::FetchSnapshotChunkResponse {
                                result: Some(super::proto_v2::fetch_snapshot_chunk_response::Result::Response(
                                    super::proto_v2::FetchSnapshotChunkResponseV2 {
                                        records: vec![super::proto_v2::SnapshotRecordV2 {
                                            record_key: "rk-2".to_string(),
                                            ordering_key: "ok-2".to_string(),
                                            line_json: "{\"id\":\"m-2\"}".to_string(),
                                        }],
                                        emitted_bytes: 16,
                                        next_cursor: None,
                                        has_more: false,
                                        revision: Some("rev-1".to_string()),
                                    },
                                )),
                            }
                        }
                    }
                    _ => super::proto_v2::FetchSnapshotChunkResponse {
                        result: Some(
                            super::proto_v2::fetch_snapshot_chunk_response::Result::Error(
                                super::proto_v2::ConnectorErrorV2 {
                                    code: "NOT_SUPPORTED".to_string(),
                                    message: "offset unsupported".to_string(),
                                    retryable: false,
                                    details: None,
                                },
                            ),
                        ),
                    },
                },
                None => super::proto_v2::FetchSnapshotChunkResponse {
                    result: Some(
                        super::proto_v2::fetch_snapshot_chunk_response::Result::Error(
                            super::proto_v2::ConnectorErrorV2 {
                                code: "INVALID_ARGUMENT".to_string(),
                                message: "missing request".to_string(),
                                retryable: false,
                                details: None,
                            },
                        ),
                    ),
                },
            };
            Ok(Response::new(response))
        }

        async fn fetch_live_page(
            &self,
            request: Request<super::proto_v2::FetchLivePageRequest>,
        ) -> Result<Response<super::proto_v2::FetchLivePageResponse>, Status> {
            let req = request.into_inner().request;
            let (cursor, handle) = match req {
                Some(r) => (r.cursor, r.handle_id.unwrap_or_else(|| "ph-1".to_string())),
                None => (None, "ph-1".to_string()),
            };
            if cursor.as_deref() == Some("invalid") {
                return Ok(Response::new(super::proto_v2::FetchLivePageResponse {
                    result: Some(super::proto_v2::fetch_live_page_response::Result::Error(
                        super::proto_v2::ConnectorErrorV2 {
                            code: "CURSOR_INVALID".to_string(),
                            message: "cursor invalid".to_string(),
                            retryable: false,
                            details: None,
                        },
                    )),
                }));
            }
            let page_no = if cursor.as_deref().is_some() { 2 } else { 1 };
            Ok(Response::new(super::proto_v2::FetchLivePageResponse {
                result: Some(super::proto_v2::fetch_live_page_response::Result::Response(
                    super::proto_v2::FetchLivePageResponseV2 {
                        items_json: vec![format!("{{\"id\":\"m-{page_no}\"}}")],
                        page: Some(super::proto_v2::LivePageInfoV2 {
                            handle_id: handle,
                            page_no,
                            has_more: page_no == 1,
                            mode: super::proto_v2::LiveModeV2::Live as i32,
                            expires_at: Some("2026-03-24T00:00:00Z".to_string()),
                            next_cursor: if page_no == 1 {
                                Some("cursor-live-1".to_string())
                            } else {
                                None
                            },
                            retry_after_ms: None,
                        }),
                    },
                )),
            }))
        }

        async fn submit_action(
            &self,
            request: Request<super::proto_v2::SubmitActionRequest>,
        ) -> Result<Response<super::proto_v2::SubmitActionResponse>, Status> {
            let req = request.into_inner().request;
            let Some(req) = req else {
                return Ok(Response::new(super::proto_v2::SubmitActionResponse {
                    result: Some(super::proto_v2::submit_action_response::Result::Error(
                        super::proto_v2::ConnectorErrorV2 {
                            code: "INVALID_ARGUMENT".to_string(),
                            message: "missing request".to_string(),
                            retryable: false,
                            details: None,
                        },
                    )),
                }));
            };
            if req.path.ends_with("/rate_limited.act") {
                return Ok(Response::new(super::proto_v2::SubmitActionResponse {
                    result: Some(super::proto_v2::submit_action_response::Result::Error(
                        super::proto_v2::ConnectorErrorV2 {
                            code: "RATE_LIMITED".to_string(),
                            message: "upstream rate limited".to_string(),
                            retryable: true,
                            details: None,
                        },
                    )),
                }));
            }
            let result = if req.execution_mode
                == super::proto_v2::ActionExecutionModeV2::Inline as i32
            {
                super::proto_v2::submit_action_response::Result::Response(
                    super::proto_v2::SubmitActionResponseV2 {
                        request_id: "req-1".to_string(),
                        estimated_duration_ms: Some(12),
                        outcome: Some(super::proto_v2::SubmitActionOutcomeV2 {
                            kind: Some(super::proto_v2::submit_action_outcome_v2::Kind::CompletedContentJson(
                                "{\"ok\":true}".to_string(),
                            )),
                        }),
                    },
                )
            } else {
                super::proto_v2::submit_action_response::Result::Response(
                    super::proto_v2::SubmitActionResponseV2 {
                        request_id: "req-2".to_string(),
                        estimated_duration_ms: Some(34),
                        outcome: Some(super::proto_v2::SubmitActionOutcomeV2 {
                            kind: Some(
                                super::proto_v2::submit_action_outcome_v2::Kind::StreamingPlan(
                                    super::proto_v2::ActionStreamingPlanV2 {
                                        accepted_content_json: "{\"state\":\"accepted\"}"
                                            .to_string(),
                                        progress_content_json: "{\"percent\":50}".to_string(),
                                        terminal_content_json: "{\"ok\":true}".to_string(),
                                        has_accepted_content: true,
                                        has_progress_content: true,
                                    },
                                ),
                            ),
                        }),
                    },
                )
            };
            Ok(Response::new(super::proto_v2::SubmitActionResponse {
                result: Some(result),
            }))
        }
    }

    #[tonic::async_trait]
    impl super::proto_v3::appfs_connector_v3_server::AppfsConnectorV3 for TestConnectorV3Service {
        async fn get_app_structure(
            &self,
            request: Request<super::proto_v3::GetAppStructureRequest>,
        ) -> Result<Response<super::proto_v3::GetAppStructureResponse>, Status> {
            let req = request
                .into_inner()
                .request
                .ok_or_else(|| Status::invalid_argument("missing request"))?;
            let response = if req.known_revision.as_deref() == Some("demo-structure-chat-001") {
                super::proto_v3::GetAppStructureResponse {
                    result: Some(
                        super::proto_v3::get_app_structure_response::Result::Response(
                            super::proto_v3::AppStructureSyncResultV3 {
                                kind: Some(
                                    super::proto_v3::app_structure_sync_result_v3::Kind::Unchanged(
                                        super::proto_v3::AppStructureSyncUnchangedV3 {
                                            app_id: req.app_id,
                                            revision: "demo-structure-chat-001".to_string(),
                                            active_scope: Some("chat-001".to_string()),
                                        },
                                    ),
                                ),
                            },
                        ),
                    ),
                }
            } else {
                super::proto_v3::GetAppStructureResponse {
                    result: Some(
                        super::proto_v3::get_app_structure_response::Result::Response(
                            super::proto_v3::AppStructureSyncResultV3 {
                                kind: Some(
                                    super::proto_v3::app_structure_sync_result_v3::Kind::Snapshot(
                                        super::proto_v3::AppStructureSyncSnapshotV3 {
                                            snapshot: Some(structure_snapshot_proto_v3("chat-001")),
                                        },
                                    ),
                                ),
                            },
                        ),
                    ),
                }
            };
            Ok(Response::new(response))
        }

        async fn refresh_app_structure(
            &self,
            request: Request<super::proto_v3::RefreshAppStructureRequest>,
        ) -> Result<Response<super::proto_v3::RefreshAppStructureResponse>, Status> {
            let req = request
                .into_inner()
                .request
                .ok_or_else(|| Status::invalid_argument("missing request"))?;
            if req.reason == super::proto_v3::AppStructureSyncReasonV3::EnterScope as i32
                && req.target_scope.is_none()
            {
                return Ok(Response::new(
                    super::proto_v3::RefreshAppStructureResponse {
                        result: Some(
                            super::proto_v3::refresh_app_structure_response::Result::Error(
                                super::proto_v3::ConnectorErrorV3 {
                                    code: "STRUCTURE_SCOPE_INVALID".to_string(),
                                    message: "target_scope is required for enter_scope refresh"
                                        .to_string(),
                                    retryable: false,
                                    details: None,
                                },
                            ),
                        ),
                    },
                ));
            }

            let target_scope = req.target_scope.unwrap_or_else(|| "chat-001".to_string());
            let snapshot = structure_snapshot_proto_v3(&target_scope);
            if req.known_revision.as_deref() == Some(snapshot.revision.as_str()) {
                return Ok(Response::new(super::proto_v3::RefreshAppStructureResponse {
                    result: Some(
                        super::proto_v3::refresh_app_structure_response::Result::Response(
                            super::proto_v3::AppStructureSyncResultV3 {
                                kind: Some(
                                    super::proto_v3::app_structure_sync_result_v3::Kind::Unchanged(
                                        super::proto_v3::AppStructureSyncUnchangedV3 {
                                            app_id: req.app_id,
                                            revision: snapshot.revision,
                                            active_scope: snapshot.active_scope,
                                        },
                                    ),
                                ),
                            },
                        ),
                    ),
                }));
            }

            Ok(Response::new(
                super::proto_v3::RefreshAppStructureResponse {
                    result: Some(
                        super::proto_v3::refresh_app_structure_response::Result::Response(
                            super::proto_v3::AppStructureSyncResultV3 {
                                kind: Some(
                                    super::proto_v3::app_structure_sync_result_v3::Kind::Snapshot(
                                        super::proto_v3::AppStructureSyncSnapshotV3 {
                                            snapshot: Some(snapshot),
                                        },
                                    ),
                                ),
                            },
                        ),
                    ),
                },
            ))
        }
    }

    fn test_ctx_v2() -> ConnectorContextV2 {
        ConnectorContextV2 {
            app_id: "aiim".to_string(),
            session_id: "sess-v2".to_string(),
            request_id: "req-v2".to_string(),
            client_token: Some("tok-v2".to_string()),
            trace_id: Some("trace-v2".to_string()),
        }
    }

    async fn spawn_test_grpc_v2_server() -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test grpc v2 listener");
        let addr = listener.local_addr().expect("read local addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    super::proto_v2::appfs_connector_v2_server::AppfsConnectorV2Server::new(
                        TestConnectorV2Service,
                    ),
                )
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("run test grpc v2 server");
        });

        (addr, shutdown_tx)
    }

    fn structure_snapshot_proto_v3(scope: &str) -> super::proto_v3::AppStructureSnapshotV3 {
        let mut nodes = vec![
            super::proto_v3::AppStructureNodeV3 {
                path: "contacts".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: None,
            },
            super::proto_v3::AppStructureNodeV3 {
                path: "contacts/zhangsan".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: None,
            },
            super::proto_v3::AppStructureNodeV3 {
                path: "contacts/zhangsan/send_message.act".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::ActionFile as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"contacts/{contact_id}/send_message.act\",\"kind\":\"action\",\"input_mode\":\"json\",\"execution_mode\":\"inline\"}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: true,
                scope: None,
            },
            super::proto_v3::AppStructureNodeV3 {
                path: "_app".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: None,
            },
            super::proto_v3::AppStructureNodeV3 {
                path: "_app/enter_scope.act".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::ActionFile as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"_app/enter_scope.act\",\"kind\":\"action\",\"input_mode\":\"json\",\"execution_mode\":\"inline\"}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: true,
                scope: None,
            },
            super::proto_v3::AppStructureNodeV3 {
                path: "_app/refresh_structure.act".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::ActionFile as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"_app/refresh_structure.act\",\"kind\":\"action\",\"input_mode\":\"json\",\"execution_mode\":\"inline\"}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: true,
                scope: None,
            },
        ];

        if scope == "chat-long" {
            nodes.push(super::proto_v3::AppStructureNodeV3 {
                path: "chats".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-long".to_string()),
            });
            nodes.push(super::proto_v3::AppStructureNodeV3 {
                path: "chats/chat-long".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-long".to_string()),
            });
            nodes.push(super::proto_v3::AppStructureNodeV3 {
                path: "chats/chat-long/messages.res.jsonl".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::SnapshotResource as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"chats/chat-long/messages.res.jsonl\",\"kind\":\"resource\",\"output_mode\":\"jsonl\",\"snapshot\":{\"max_materialized_bytes\":1024,\"prewarm\":true,\"prewarm_timeout_ms\":5000,\"read_through_timeout_ms\":10000,\"on_timeout\":\"return_stale\"}}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-long".to_string()),
            });
        } else {
            nodes.push(super::proto_v3::AppStructureNodeV3 {
                path: "chats".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-001".to_string()),
            });
            nodes.push(super::proto_v3::AppStructureNodeV3 {
                path: "chats/chat-001".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-001".to_string()),
            });
            nodes.push(super::proto_v3::AppStructureNodeV3 {
                path: "chats/chat-001/messages.res.jsonl".to_string(),
                kind: super::proto_v3::AppStructureNodeKindV3::SnapshotResource as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"chats/chat-001/messages.res.jsonl\",\"kind\":\"resource\",\"output_mode\":\"jsonl\",\"snapshot\":{\"max_materialized_bytes\":10485760,\"prewarm\":true,\"prewarm_timeout_ms\":5000,\"read_through_timeout_ms\":10000,\"on_timeout\":\"return_stale\"}}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-001".to_string()),
            });
        }

        super::proto_v3::AppStructureSnapshotV3 {
            app_id: "aiim".to_string(),
            revision: format!("demo-structure-{scope}"),
            active_scope: Some(scope.to_string()),
            ownership_prefixes: vec![
                "_meta".to_string(),
                "contacts".to_string(),
                "chats".to_string(),
                "_app".to_string(),
            ],
            nodes,
        }
    }

    async fn spawn_test_grpc_v2_v3_server() -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test grpc v2/v3 listener");
        let addr = listener.local_addr().expect("read local addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    super::proto_v2::appfs_connector_v2_server::AppfsConnectorV2Server::new(
                        TestConnectorV2Service,
                    ),
                )
                .add_service(
                    super::proto_v3::appfs_connector_v3_server::AppfsConnectorV3Server::new(
                        TestConnectorV3Service,
                    ),
                )
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("run test grpc v2/v3 server");
        });

        (addr, shutdown_tx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_connector_v2_roundtrip() {
        let (addr, shutdown) = spawn_test_grpc_v2_server().await;
        let mut connector = GrpcBridgeConnectorV2::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge v2 connector");

        let info = connector.connector_id().expect("connector info");
        assert_eq!(info.connector_id, "mock-grpc-v2");

        let ctx = test_ctx_v2();
        let health = connector.health(&ctx).expect("health");
        assert!(health.healthy);

        let meta = connector
            .prewarm_snapshot_meta("/messages", Duration::from_millis(200), &ctx)
            .expect("prewarm");
        assert_eq!(meta.revision, Some("rev-1".to_string()));

        let first = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequestV2 {
                    resource_path: "/messages".to_string(),
                    resume: SnapshotResumeV2::Start,
                    budget_bytes: 1024,
                },
                &ctx,
            )
            .expect("snapshot first");
        assert!(first.has_more);
        assert_eq!(first.records.len(), 1);

        let live = connector
            .fetch_live_page(
                FetchLivePageRequestV2 {
                    resource_path: "/messages".to_string(),
                    handle_id: None,
                    cursor: None,
                    page_size: 10,
                },
                &ctx,
            )
            .expect("live first");
        assert_eq!(live.page.page_no, 1);
        assert_eq!(live.items.len(), 1);

        let inline = connector
            .submit_action(
                SubmitActionRequestV2 {
                    path: "/send_message.act".to_string(),
                    payload: serde_json::json!({"text":"hi"}),
                    execution_mode: ActionExecutionModeV2::Inline,
                },
                &ctx,
            )
            .expect("submit inline");
        match inline.outcome {
            SubmitActionOutcomeV2::Completed { content } => {
                assert_eq!(content["ok"], true);
            }
            _ => panic!("expected completed outcome"),
        }

        let _ = shutdown.send(());
    }

    #[test]
    fn rejects_unspecified_and_unknown_enums() {
        let info_unspecified = super::from_proto_connector_info(super::proto_v2::ConnectorInfoV2 {
            connector_id: "c".to_string(),
            version: "v".to_string(),
            app_id: "aiim".to_string(),
            transport: super::proto_v2::ConnectorTransportV2::Unspecified as i32,
            supports_snapshot: true,
            supports_live: true,
            supports_action: true,
            optional_features: vec![],
        })
        .expect_err("unspecified transport should fail");
        assert!(info_unspecified
            .message
            .contains("connector_info.transport"));

        let info_unknown = super::from_proto_connector_info(super::proto_v2::ConnectorInfoV2 {
            connector_id: "c".to_string(),
            version: "v".to_string(),
            app_id: "aiim".to_string(),
            transport: 999,
            supports_snapshot: true,
            supports_live: true,
            supports_action: true,
            optional_features: vec![],
        })
        .expect_err("unknown transport should fail");
        assert!(info_unknown.message.contains("unknown enum value"));

        let health_unspecified = super::from_proto_health_status(super::proto_v2::HealthStatusV2 {
            healthy: true,
            auth_status: super::proto_v2::AuthStatusV2::Unspecified as i32,
            message: Some("ok".to_string()),
            checked_at: "2026-03-24T00:00:00Z".to_string(),
        })
        .expect_err("unspecified auth should fail");
        assert!(health_unspecified.message.contains("health.auth_status"));

        let live_unknown =
            super::from_proto_fetch_live_page_response(super::proto_v2::FetchLivePageResponseV2 {
                items_json: vec!["{\"id\":\"m-1\"}".to_string()],
                page: Some(super::proto_v2::LivePageInfoV2 {
                    handle_id: "ph-1".to_string(),
                    page_no: 1,
                    has_more: false,
                    mode: 999,
                    expires_at: None,
                    next_cursor: None,
                    retry_after_ms: None,
                }),
            })
            .expect_err("unknown live mode should fail");
        assert!(live_unknown.message.contains("live.page.mode"));
    }

    #[test]
    fn rejects_malformed_json_and_empty_cursor_payloads() {
        let invalid_json = super::from_proto_fetch_snapshot_chunk_response(
            super::proto_v2::FetchSnapshotChunkResponseV2 {
                records: vec![super::proto_v2::SnapshotRecordV2 {
                    record_key: "rk".to_string(),
                    ordering_key: "ok".to_string(),
                    line_json: "{".to_string(),
                }],
                emitted_bytes: 1,
                next_cursor: Some("".to_string()),
                has_more: false,
                revision: None,
            },
        )
        .expect_err("invalid line_json should fail");
        assert!(invalid_json.message.contains("invalid json"));

        let non_object = super::from_proto_fetch_snapshot_chunk_response(
            super::proto_v2::FetchSnapshotChunkResponseV2 {
                records: vec![super::proto_v2::SnapshotRecordV2 {
                    record_key: "rk".to_string(),
                    ordering_key: "ok".to_string(),
                    line_json: "[1,2,3]".to_string(),
                }],
                emitted_bytes: 9,
                next_cursor: Some("".to_string()),
                has_more: false,
                revision: None,
            },
        )
        .expect_err("non-object line_json should fail");
        assert!(non_object.message.contains("expected JSON object"));

        let live_empty_cursor =
            super::from_proto_fetch_live_page_response(super::proto_v2::FetchLivePageResponseV2 {
                items_json: vec!["{\"id\":\"m-1\"}".to_string()],
                page: Some(super::proto_v2::LivePageInfoV2 {
                    handle_id: "ph-1".to_string(),
                    page_no: 1,
                    has_more: true,
                    mode: super::proto_v2::LiveModeV2::Live as i32,
                    expires_at: None,
                    next_cursor: Some("".to_string()),
                    retry_after_ms: None,
                }),
            })
            .expect_err("empty live next_cursor should fail");
        assert!(live_empty_cursor.message.contains("live.page.next_cursor"));

        let snapshot_empty_cursor = super::from_proto_fetch_snapshot_chunk_response(
            super::proto_v2::FetchSnapshotChunkResponseV2 {
                records: vec![super::proto_v2::SnapshotRecordV2 {
                    record_key: "rk".to_string(),
                    ordering_key: "ok".to_string(),
                    line_json: "{\"id\":\"m-1\"}".to_string(),
                }],
                emitted_bytes: 12,
                next_cursor: Some("".to_string()),
                has_more: true,
                revision: None,
            },
        )
        .expect_err("empty snapshot next_cursor should fail");
        assert!(snapshot_empty_cursor
            .message
            .contains("snapshot.next_cursor"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_connector_v2_error_paths() {
        let (addr, shutdown) = spawn_test_grpc_v2_server().await;
        let mut connector = GrpcBridgeConnectorV2::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge v2 connector");
        let ctx = test_ctx_v2();

        let cursor_err = connector
            .fetch_live_page(
                FetchLivePageRequestV2 {
                    resource_path: "/messages".to_string(),
                    handle_id: None,
                    cursor: Some("invalid".to_string()),
                    page_size: 10,
                },
                &ctx,
            )
            .expect_err("invalid cursor should fail");
        assert_eq!(cursor_err.code, "CURSOR_INVALID");
        assert!(!cursor_err.retryable);

        let rate_limit_err = connector
            .submit_action(
                SubmitActionRequestV2 {
                    path: "/messages/rate_limited.act".to_string(),
                    payload: serde_json::json!({"text":"hi"}),
                    execution_mode: ActionExecutionModeV2::Inline,
                },
                &ctx,
            )
            .expect_err("rate limited should fail");
        assert_eq!(rate_limit_err.code, "RATE_LIMITED");
        assert!(rate_limit_err.retryable);

        let _ = shutdown.send(());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_connector_v3_structure_roundtrip() {
        let (addr, shutdown) = spawn_test_grpc_v2_v3_server().await;
        let mut connector = GrpcBridgeConnectorV2::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge v2/v3 connector");
        let ctx = test_ctx_v2();

        let initial = connector
            .get_app_structure(
                GetAppStructureRequestV3 {
                    app_id: "aiim".to_string(),
                    known_revision: None,
                },
                &ctx,
            )
            .expect("initial structure");
        match initial.result {
            agentfs_sdk::AppStructureSyncResultV3::Snapshot { snapshot } => {
                assert_eq!(snapshot.active_scope.as_deref(), Some("chat-001"));
                assert!(snapshot
                    .nodes
                    .iter()
                    .any(|node| node.path == "_app/enter_scope.act"));
            }
            _ => panic!("expected snapshot result"),
        }

        let refreshed = connector
            .refresh_app_structure(
                RefreshAppStructureRequestV3 {
                    app_id: "aiim".to_string(),
                    known_revision: Some("demo-structure-chat-001".to_string()),
                    reason: AppStructureSyncReasonV3::Refresh,
                    target_scope: None,
                    trigger_action_path: Some("/_app/refresh_structure.act".to_string()),
                },
                &ctx,
            )
            .expect("unchanged refresh");
        match refreshed.result {
            agentfs_sdk::AppStructureSyncResultV3::Unchanged {
                revision,
                active_scope,
                ..
            } => {
                assert_eq!(revision, "demo-structure-chat-001");
                assert_eq!(active_scope.as_deref(), Some("chat-001"));
            }
            _ => panic!("expected unchanged result"),
        }

        let enter_scope = connector
            .refresh_app_structure(
                RefreshAppStructureRequestV3 {
                    app_id: "aiim".to_string(),
                    known_revision: Some("demo-structure-chat-001".to_string()),
                    reason: AppStructureSyncReasonV3::EnterScope,
                    target_scope: Some("chat-long".to_string()),
                    trigger_action_path: Some("/_app/enter_scope.act".to_string()),
                },
                &ctx,
            )
            .expect("enter scope refresh");
        match enter_scope.result {
            agentfs_sdk::AppStructureSyncResultV3::Snapshot { snapshot } => {
                assert_eq!(snapshot.active_scope.as_deref(), Some("chat-long"));
                assert!(snapshot
                    .nodes
                    .iter()
                    .any(|node| node.path == "chats/chat-long/messages.res.jsonl"));
            }
            _ => panic!("expected snapshot result"),
        }

        let _ = shutdown.send(());
    }
}
