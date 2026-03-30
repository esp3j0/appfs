use super::bridge_resilience::{
    is_retryable_grpc_code, BridgeCircuitBreaker, BridgeMetrics, BridgeRuntimeOptions,
};
use agentfs_sdk::{
    connector_error_codes, ActionExecutionMode, ActionStreamingPlan, AdapterControlActionV1,
    AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1, AdapterInputModeV1,
    AdapterStreamingPlanV1, AdapterSubmitOutcomeV1, AppAdapterV1, AppConnector, AppStructureNode,
    AppStructureNodeKind, AppStructureSnapshot, AppStructureSyncReason, AppStructureSyncResult,
    AuthStatus, ConnectorContext, ConnectorError, ConnectorInfo, ConnectorTransport,
    FetchLivePageRequest, FetchLivePageResponse, FetchSnapshotChunkRequest,
    FetchSnapshotChunkResponse, GetAppStructureRequest, GetAppStructureResponse, HealthStatus,
    LiveMode, LivePageInfo, RefreshAppStructureRequest, RefreshAppStructureResponse,
    RequestContextV1, SnapshotMeta, SnapshotRecord, SnapshotResume, SubmitActionOutcome,
    SubmitActionRequest as ConnectorSubmitActionRequest,
    SubmitActionResponse as ConnectorSubmitActionResponse,
};
use serde_json::Value as JsonValue;
use std::future::Future;
use std::time::{Duration, Instant};
use tonic::transport::{Channel, Endpoint};

pub(super) mod proto {
    tonic::include_proto!("appfs.adapter.v1");
}
pub(super) mod connector_proto {
    tonic::include_proto!("appfs.connector");
}
pub(super) mod structure_proto {
    tonic::include_proto!("appfs.structure");
}

use connector_proto::appfs_connector_client::AppfsConnectorClient;
use proto::appfs_adapter_bridge_client::AppfsAdapterBridgeClient;
use proto::submit_action_response::Result as SubmitActionResult;
use proto::submit_control_action_request::Action as SubmitControlAction;
use proto::submit_control_action_response::Result as SubmitControlResult;
use proto::{
    ControlCompletedOutcome, ExecutionMode, InputMode, PagingCloseAction, PagingFetchNextAction,
    RequestContext, SubmitActionRequest, SubmitControlActionRequest,
};
use structure_proto::appfs_structure_connector_client::AppfsStructureConnectorClient;

#[cfg_attr(not(test), allow(dead_code))]
pub(super) struct GrpcBridgeAdapterV1 {
    app_id: String,
    client: AppfsAdapterBridgeClient<Channel>,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

pub(super) struct GrpcBridgeConnector {
    app_id: String,
    client: AppfsConnectorClient<Channel>,
    structure_client: AppfsStructureConnectorClient<Channel>,
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

impl GrpcBridgeConnector {
    pub(super) fn new(
        app_id: String,
        endpoint: String,
        timeout: Duration,
        runtime_options: BridgeRuntimeOptions,
    ) -> Result<Self, ConnectorError> {
        let endpoint = endpoint.trim().trim_end_matches('/').to_string();
        let endpoint = Endpoint::from_shared(endpoint.clone()).map_err(|err| ConnectorError {
            code: connector_error_codes::INVALID_ARGUMENT.to_string(),
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
            client: AppfsConnectorClient::new(channel.clone()),
            structure_client: AppfsStructureConnectorClient::new(channel),
            runtime_options,
            metrics: BridgeMetrics::default(),
            circuit_breaker: BridgeCircuitBreaker::default(),
        })
    }

    #[allow(clippy::result_large_err)]
    fn run_connector_rpc<Resp, F>(&mut self, method: &str, mut f: F) -> Result<Resp, ConnectorError>
    where
        F: FnMut(AppfsConnectorClient<Channel>) -> Result<tonic::Response<Resp>, tonic::Status>,
    {
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            return Err(ConnectorError {
                code: connector_error_codes::INTERNAL.to_string(),
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
                    return Err(map_connector_grpc_status(
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
    fn run_structure_rpc<Resp, F>(&mut self, method: &str, mut f: F) -> Result<Resp, ConnectorError>
    where
        F: FnMut(
            AppfsStructureConnectorClient<Channel>,
        ) -> Result<tonic::Response<Resp>, tonic::Status>,
    {
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            return Err(ConnectorError {
                code: connector_error_codes::INTERNAL.to_string(),
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
            let client = self.structure_client.clone();
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
                    return Err(map_structure_grpc_status(
                        method,
                        status,
                        attempt,
                        &self.metrics.snapshot(),
                    ));
                }
            }
        }
    }

    fn ensure_app_match(&self, app_id: &str) -> Result<(), ConnectorError> {
        if app_id != self.app_id {
            return Err(ConnectorError {
                code: connector_error_codes::INVALID_ARGUMENT.to_string(),
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

impl AppConnector for GrpcBridgeConnector {
    fn connector_id(&self) -> Result<ConnectorInfo, ConnectorError> {
        let mut client = self.client.clone();
        let response =
            run_async(client.get_connector_info(connector_proto::GetConnectorInfoRequest {}))
                .map_err(|status| map_connector_grpc_status("GetConnectorInfo", status, 1, "n/a"))?
                .into_inner();
        match response.result {
            Some(connector_proto::get_connector_info_response::Result::Info(info)) => {
                let app_id = info.app_id.clone();
                self.ensure_app_match(&app_id)?;
                from_proto_connector_info(info)
            }
            Some(connector_proto::get_connector_info_response::Result::Error(err)) => {
                Err(from_connector_proto_error(err))
            }
            None => Err(ConnectorError {
                code: connector_error_codes::INTERNAL.to_string(),
                message: "bridge grpc GetConnectorInfo returned empty result".to_string(),
                retryable: true,
                details: None,
            }),
        }
    }

    #[allow(clippy::result_large_err)]
    fn health(&mut self, ctx: &ConnectorContext) -> Result<HealthStatus, ConnectorError> {
        let req = connector_proto::HealthRequest {
            context: Some(to_connector_proto_context(ctx)),
        };
        let response =
            self.run_connector_rpc("Health", |mut client| run_async(client.health(req.clone())))?;
        match response.result {
            Some(connector_proto::health_response::Result::Status(status)) => {
                from_proto_health_status(status)
            }
            Some(connector_proto::health_response::Result::Error(err)) => {
                Err(from_connector_proto_error(err))
            }
            None => Err(empty_result_error("Health")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: Duration,
        ctx: &ConnectorContext,
    ) -> Result<SnapshotMeta, ConnectorError> {
        let timeout_ms = timeout.as_millis().max(1).min(u128::from(u64::MAX)) as u64;
        let req = connector_proto::PrewarmSnapshotMetaRequest {
            context: Some(to_connector_proto_context(ctx)),
            resource_path: resource_path.to_string(),
            timeout_ms,
        };
        let response = self.run_connector_rpc("PrewarmSnapshotMeta", |mut client| {
            run_async(client.prewarm_snapshot_meta(req.clone()))
        })?;
        match response.result {
            Some(connector_proto::prewarm_snapshot_meta_response::Result::Meta(meta)) => {
                Ok(from_proto_snapshot_meta(meta))
            }
            Some(connector_proto::prewarm_snapshot_meta_response::Result::Error(err)) => {
                Err(from_connector_proto_error(err))
            }
            None => Err(empty_result_error("PrewarmSnapshotMeta")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequest,
        ctx: &ConnectorContext,
    ) -> Result<FetchSnapshotChunkResponse, ConnectorError> {
        let req = connector_proto::FetchSnapshotChunkRequest {
            context: Some(to_connector_proto_context(ctx)),
            request: Some(to_proto_fetch_snapshot_chunk_request(request)),
        };
        let response = self.run_connector_rpc("FetchSnapshotChunk", |mut client| {
            run_async(client.fetch_snapshot_chunk(req.clone()))
        })?;
        match response.result {
            Some(connector_proto::fetch_snapshot_chunk_response::Result::Response(resp)) => {
                from_proto_fetch_snapshot_chunk_response(resp)
            }
            Some(connector_proto::fetch_snapshot_chunk_response::Result::Error(err)) => {
                Err(from_connector_proto_error(err))
            }
            None => Err(empty_result_error("FetchSnapshotChunk")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequest,
        ctx: &ConnectorContext,
    ) -> Result<FetchLivePageResponse, ConnectorError> {
        let req = connector_proto::FetchLivePageRequest {
            context: Some(to_connector_proto_context(ctx)),
            request: Some(to_proto_fetch_live_page_request(request)),
        };
        let response = self.run_connector_rpc("FetchLivePage", |mut client| {
            run_async(client.fetch_live_page(req.clone()))
        })?;
        match response.result {
            Some(connector_proto::fetch_live_page_response::Result::Response(resp)) => {
                from_proto_fetch_live_page_response(resp)
            }
            Some(connector_proto::fetch_live_page_response::Result::Error(err)) => {
                Err(from_connector_proto_error(err))
            }
            None => Err(empty_result_error("FetchLivePage")),
        }
    }

    #[allow(clippy::result_large_err)]
    fn submit_action(
        &mut self,
        request: ConnectorSubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> Result<ConnectorSubmitActionResponse, ConnectorError> {
        let req = connector_proto::SubmitActionRequest {
            context: Some(to_connector_proto_context(ctx)),
            request: Some(to_proto_submit_action_request(request)?),
        };
        let response = self.run_connector_rpc("SubmitAction", |mut client| {
            run_async(client.submit_action(req.clone()))
        })?;
        match response.result {
            Some(connector_proto::submit_action_response::Result::Response(resp)) => {
                from_proto_submit_action_response(resp)
            }
            Some(connector_proto::submit_action_response::Result::Error(err)) => {
                Err(from_connector_proto_error(err))
            }
            None => Err(empty_result_error("SubmitAction")),
        }
    }
    fn get_app_structure(
        &mut self,
        request: GetAppStructureRequest,
        ctx: &ConnectorContext,
    ) -> Result<GetAppStructureResponse, ConnectorError> {
        if request.app_id != self.app_id {
            return Err(ConnectorError {
                code: connector_error_codes::INVALID_ARGUMENT.to_string(),
                message: format!(
                    "grpc structure connector app_id mismatch expected={} got={}",
                    self.app_id, request.app_id
                ),
                retryable: false,
                details: None,
            });
        }
        let req = structure_proto::GetAppStructureRequest {
            context: Some(to_structure_proto_context(ctx)),
            request: Some(to_structure_get_app_structure_request(request)),
        };
        let response = self.run_structure_rpc("GetAppStructure", |mut client| {
            run_async(client.get_app_structure(req.clone()))
        })?;
        match response.result {
            Some(structure_proto::get_app_structure_response::Result::Response(resp)) => {
                from_structure_proto_get_app_structure_response(resp)
            }
            Some(structure_proto::get_app_structure_response::Result::Error(err)) => {
                Err(from_structure_proto_connector_error(err))
            }
            None => Err(empty_structure_result_error("GetAppStructure")),
        }
    }

    fn refresh_app_structure(
        &mut self,
        request: RefreshAppStructureRequest,
        ctx: &ConnectorContext,
    ) -> Result<RefreshAppStructureResponse, ConnectorError> {
        if request.app_id != self.app_id {
            return Err(ConnectorError {
                code: connector_error_codes::INVALID_ARGUMENT.to_string(),
                message: format!(
                    "grpc structure connector app_id mismatch expected={} got={}",
                    self.app_id, request.app_id
                ),
                retryable: false,
                details: None,
            });
        }
        let req = structure_proto::RefreshAppStructureRequest {
            context: Some(to_structure_proto_context(ctx)),
            request: Some(to_structure_refresh_app_structure_request(request)),
        };
        let response = self.run_structure_rpc("RefreshAppStructure", |mut client| {
            run_async(client.refresh_app_structure(req.clone()))
        })?;
        match response.result {
            Some(structure_proto::refresh_app_structure_response::Result::Response(resp)) => {
                from_structure_proto_refresh_app_structure_response(resp)
            }
            Some(structure_proto::refresh_app_structure_response::Result::Error(err)) => {
                Err(from_structure_proto_connector_error(err))
            }
            None => Err(empty_structure_result_error("RefreshAppStructure")),
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

fn map_connector_grpc_status(
    method: &str,
    status: tonic::Status,
    attempts: u32,
    metrics: &str,
) -> ConnectorError {
    ConnectorError {
        code: if is_retryable_grpc_code(status.code()) {
            connector_error_codes::UPSTREAM_UNAVAILABLE.to_string()
        } else {
            connector_error_codes::INTERNAL.to_string()
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

fn empty_result_error(method: &str) -> ConnectorError {
    ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: format!("bridge grpc {} returned empty result", method),
        retryable: true,
        details: None,
    }
}

fn empty_structure_result_error(method: &str) -> ConnectorError {
    ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: format!("bridge grpc {} returned empty result", method),
        retryable: true,
        details: None,
    }
}

fn map_structure_grpc_status(
    method: &str,
    status: tonic::Status,
    attempts: u32,
    metrics: &str,
) -> ConnectorError {
    ConnectorError {
        code: if is_retryable_grpc_code(status.code()) {
            connector_error_codes::UPSTREAM_UNAVAILABLE.to_string()
        } else {
            connector_error_codes::INTERNAL.to_string()
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

fn to_connector_proto_context(ctx: &ConnectorContext) -> connector_proto::ConnectorContext {
    connector_proto::ConnectorContext {
        app_id: ctx.app_id.clone(),
        session_id: ctx.session_id.clone(),
        request_id: ctx.request_id.clone(),
        client_token: ctx.client_token.clone(),
        trace_id: ctx.trace_id.clone(),
    }
}

fn to_structure_proto_context(ctx: &ConnectorContext) -> structure_proto::ConnectorContext {
    structure_proto::ConnectorContext {
        app_id: ctx.app_id.clone(),
        session_id: ctx.session_id.clone(),
        request_id: ctx.request_id.clone(),
        client_token: ctx.client_token.clone(),
        trace_id: ctx.trace_id.clone(),
    }
}

fn to_structure_get_app_structure_request(
    request: GetAppStructureRequest,
) -> structure_proto::GetAppStructureInput {
    structure_proto::GetAppStructureInput {
        app_id: request.app_id,
        known_revision: request.known_revision,
    }
}

fn to_structure_refresh_app_structure_request(
    request: RefreshAppStructureRequest,
) -> structure_proto::RefreshAppStructureInput {
    let reason = match request.reason {
        AppStructureSyncReason::Initialize => {
            structure_proto::AppStructureSyncReason::Initialize as i32
        }
        AppStructureSyncReason::EnterScope => {
            structure_proto::AppStructureSyncReason::EnterScope as i32
        }
        AppStructureSyncReason::Refresh => structure_proto::AppStructureSyncReason::Refresh as i32,
        AppStructureSyncReason::Recover => structure_proto::AppStructureSyncReason::Recover as i32,
    };
    structure_proto::RefreshAppStructureInput {
        app_id: request.app_id,
        known_revision: request.known_revision,
        reason,
        target_scope: request.target_scope,
        trigger_action_path: request.trigger_action_path,
    }
}

fn from_proto_connector_info(
    info: connector_proto::ConnectorInfo,
) -> Result<ConnectorInfo, ConnectorError> {
    let transport =
        connector_proto::ConnectorTransport::try_from(info.transport).map_err(|_| {
            malformed_payload(
                "connector_info.transport",
                format!("unknown enum value {}", info.transport),
            )
        })?;
    let transport = match transport {
        connector_proto::ConnectorTransport::InProcess => ConnectorTransport::InProcess,
        connector_proto::ConnectorTransport::HttpBridge => ConnectorTransport::HttpBridge,
        connector_proto::ConnectorTransport::GrpcBridge => ConnectorTransport::GrpcBridge,
        connector_proto::ConnectorTransport::Unspecified => {
            return Err(malformed_payload(
                "connector_info.transport",
                "UNSPECIFIED enum value is not allowed",
            ))
        }
    };
    Ok(ConnectorInfo {
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
    status: connector_proto::HealthStatus,
) -> Result<HealthStatus, ConnectorError> {
    let auth = connector_proto::AuthStatus::try_from(status.auth_status).map_err(|_| {
        malformed_payload(
            "health.auth_status",
            format!("unknown enum value {}", status.auth_status),
        )
    })?;
    let auth_status = match auth {
        connector_proto::AuthStatus::Valid => AuthStatus::Valid,
        connector_proto::AuthStatus::Expired => AuthStatus::Expired,
        connector_proto::AuthStatus::Refreshing => AuthStatus::Refreshing,
        connector_proto::AuthStatus::Invalid => AuthStatus::Invalid,
        connector_proto::AuthStatus::Unspecified => {
            return Err(malformed_payload(
                "health.auth_status",
                "UNSPECIFIED enum value is not allowed",
            ))
        }
    };
    Ok(HealthStatus {
        healthy: status.healthy,
        auth_status,
        message: status.message,
        checked_at: status.checked_at,
    })
}

fn from_proto_snapshot_meta(meta: connector_proto::SnapshotMeta) -> SnapshotMeta {
    SnapshotMeta {
        size_bytes: meta.size_bytes,
        revision: meta.revision,
        last_modified: meta.last_modified,
        item_count: meta.item_count,
    }
}

fn to_proto_fetch_snapshot_chunk_request(
    request: FetchSnapshotChunkRequest,
) -> connector_proto::SnapshotChunkRequest {
    let resume = match request.resume {
        SnapshotResume::Start => connector_proto::SnapshotResume {
            kind: Some(connector_proto::snapshot_resume::Kind::Start(true)),
        },
        SnapshotResume::Cursor(cursor) => connector_proto::SnapshotResume {
            kind: Some(connector_proto::snapshot_resume::Kind::Cursor(cursor)),
        },
        SnapshotResume::Offset(offset) => connector_proto::SnapshotResume {
            kind: Some(connector_proto::snapshot_resume::Kind::Offset(offset)),
        },
    };
    connector_proto::SnapshotChunkRequest {
        resource_path: request.resource_path,
        resume: Some(resume),
        budget_bytes: request.budget_bytes,
    }
}

fn from_proto_fetch_snapshot_chunk_response(
    response: connector_proto::SnapshotChunkResponse,
) -> Result<FetchSnapshotChunkResponse, ConnectorError> {
    let mut records = Vec::with_capacity(response.records.len());
    for record in response.records {
        if record.line_json.trim().is_empty() {
            return Err(ConnectorError {
                code: connector_error_codes::INTERNAL.to_string(),
                message: "snapshot record missing line".to_string(),
                retryable: true,
                details: None,
            });
        }
        records.push(SnapshotRecord {
            record_key: record.record_key,
            ordering_key: record.ordering_key,
            line: parse_connector_json_object_text(
                &record.line_json,
                "snapshot.records[].line_json",
            )?,
        });
    }
    Ok(FetchSnapshotChunkResponse {
        records,
        emitted_bytes: response.emitted_bytes,
        next_cursor: parse_optional_cursor(response.next_cursor, "snapshot.next_cursor")?,
        has_more: response.has_more,
        revision: response.revision,
    })
}

fn to_proto_fetch_live_page_request(
    request: FetchLivePageRequest,
) -> connector_proto::LivePageRequest {
    connector_proto::LivePageRequest {
        resource_path: request.resource_path,
        handle_id: request.handle_id,
        cursor: request.cursor,
        page_size: request.page_size,
    }
}

fn from_proto_fetch_live_page_response(
    response: connector_proto::LivePageResponse,
) -> Result<FetchLivePageResponse, ConnectorError> {
    let page = response.page.ok_or_else(|| ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: "live page response missing page".to_string(),
        retryable: true,
        details: None,
    })?;
    let mut items = Vec::with_capacity(response.items_json.len());
    for item in response.items_json {
        items.push(parse_connector_json_text(&item, "live.items_json[]")?);
    }
    Ok(FetchLivePageResponse {
        items,
        page: LivePageInfo {
            handle_id: page.handle_id,
            page_no: page.page_no,
            has_more: page.has_more,
            mode: match connector_proto::LiveMode::try_from(page.mode).map_err(|_| {
                malformed_payload(
                    "live.page.mode",
                    format!("unknown enum value {}", page.mode),
                )
            })? {
                connector_proto::LiveMode::Live => LiveMode::Live,
                connector_proto::LiveMode::Unspecified => {
                    return Err(malformed_payload(
                        "live.page.mode",
                        "UNSPECIFIED enum value is not allowed",
                    ))
                }
            },
            expires_at: page.expires_at,
            next_cursor: parse_optional_cursor(page.next_cursor, "live.page.next_cursor")?,
            retry_after_ms: page.retry_after_ms,
        },
    })
}

fn to_proto_submit_action_request(
    request: ConnectorSubmitActionRequest,
) -> Result<connector_proto::SubmitActionInput, ConnectorError> {
    Ok(connector_proto::SubmitActionInput {
        path: request.path,
        payload_json: serde_json::to_string(&request.payload).map_err(|err| ConnectorError {
            code: connector_error_codes::INVALID_PAYLOAD.to_string(),
            message: format!("invalid submit_action payload: {err}"),
            retryable: false,
            details: None,
        })?,
        execution_mode: match request.execution_mode {
            ActionExecutionMode::Inline => connector_proto::ActionExecutionMode::Inline as i32,
            ActionExecutionMode::Streaming => {
                connector_proto::ActionExecutionMode::Streaming as i32
            }
        },
    })
}

fn from_proto_submit_action_response(
    response: connector_proto::SubmitActionOutput,
) -> Result<ConnectorSubmitActionResponse, ConnectorError> {
    let outcome = response.outcome.ok_or_else(|| ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: "submit action response missing outcome".to_string(),
        retryable: true,
        details: None,
    })?;
    let mapped = match outcome.kind {
        Some(connector_proto::submit_action_outcome::Kind::CompletedContentJson(content)) => {
            SubmitActionOutcome::Completed {
                content: parse_connector_json_text(
                    &content,
                    "submit_action.completed_content_json",
                )?,
            }
        }
        Some(connector_proto::submit_action_outcome::Kind::StreamingPlan(plan)) => {
            if plan.terminal_content_json.trim().is_empty() {
                return Err(ConnectorError {
                    code: connector_error_codes::INTERNAL.to_string(),
                    message: "streaming plan missing terminal_content_json".to_string(),
                    retryable: true,
                    details: None,
                });
            }
            let terminal = parse_connector_json_text(
                &plan.terminal_content_json,
                "submit_action.streaming.terminal_content_json",
            )?;
            let accepted = if plan.has_accepted_content {
                Some(parse_connector_json_text(
                    &plan.accepted_content_json,
                    "submit_action.streaming.accepted_content_json",
                )?)
            } else {
                None
            };
            let progress = if plan.has_progress_content {
                Some(parse_connector_json_text(
                    &plan.progress_content_json,
                    "submit_action.streaming.progress_content_json",
                )?)
            } else {
                None
            };
            SubmitActionOutcome::Streaming {
                plan: ActionStreamingPlan {
                    accepted_content: accepted,
                    progress_content: progress,
                    terminal_content: terminal,
                },
            }
        }
        None => {
            return Err(ConnectorError {
                code: connector_error_codes::INTERNAL.to_string(),
                message: "submit action outcome kind is empty".to_string(),
                retryable: true,
                details: None,
            });
        }
    };

    Ok(ConnectorSubmitActionResponse {
        request_id: response.request_id,
        estimated_duration_ms: response.estimated_duration_ms,
        outcome: mapped,
    })
}

fn from_connector_proto_error(err: connector_proto::ConnectorError) -> ConnectorError {
    ConnectorError {
        code: err.code,
        message: err.message,
        retryable: err.retryable,
        details: err.details,
    }
}

fn from_structure_proto_connector_error(err: structure_proto::ConnectorError) -> ConnectorError {
    ConnectorError {
        code: err.code,
        message: err.message,
        retryable: err.retryable,
        details: err.details,
    }
}

fn from_structure_proto_get_app_structure_response(
    response: structure_proto::AppStructureSyncResult,
) -> Result<GetAppStructureResponse, ConnectorError> {
    Ok(GetAppStructureResponse {
        result: from_structure_proto_sync_result(response)?,
    })
}

fn from_structure_proto_refresh_app_structure_response(
    response: structure_proto::AppStructureSyncResult,
) -> Result<RefreshAppStructureResponse, ConnectorError> {
    Ok(RefreshAppStructureResponse {
        result: from_structure_proto_sync_result(response)?,
    })
}

fn from_structure_proto_sync_result(
    response: structure_proto::AppStructureSyncResult,
) -> Result<AppStructureSyncResult, ConnectorError> {
    match response.kind {
        Some(structure_proto::app_structure_sync_result::Kind::Unchanged(unchanged)) => {
            Ok(AppStructureSyncResult::Unchanged {
                app_id: unchanged.app_id,
                revision: unchanged.revision,
                active_scope: unchanged.active_scope,
            })
        }
        Some(structure_proto::app_structure_sync_result::Kind::Snapshot(snapshot)) => {
            Ok(AppStructureSyncResult::Snapshot {
                snapshot: from_structure_proto_snapshot(snapshot.snapshot.ok_or_else(|| {
                    malformed_structure_payload(
                        "structure.result.snapshot.snapshot",
                        "snapshot payload is required",
                    )
                })?)?,
            })
        }
        None => Err(malformed_structure_payload(
            "structure.result.kind",
            "result kind is empty",
        )),
    }
}

fn from_structure_proto_snapshot(
    snapshot: structure_proto::AppStructureSnapshot,
) -> Result<AppStructureSnapshot, ConnectorError> {
    if snapshot.revision.trim().is_empty() {
        return Err(malformed_structure_payload(
            "structure.snapshot.revision",
            "revision must be non-empty",
        ));
    }
    let mut nodes = Vec::with_capacity(snapshot.nodes.len());
    for node in snapshot.nodes {
        nodes.push(from_structure_proto_node(node)?);
    }
    Ok(AppStructureSnapshot {
        app_id: snapshot.app_id,
        revision: snapshot.revision,
        active_scope: snapshot.active_scope,
        ownership_prefixes: snapshot.ownership_prefixes,
        nodes,
    })
}

fn from_structure_proto_node(
    node: structure_proto::AppStructureNode,
) -> Result<AppStructureNode, ConnectorError> {
    if node.path.trim().is_empty() {
        return Err(malformed_structure_payload(
            "structure.snapshot.nodes[].path",
            "path must be non-empty",
        ));
    }
    let kind = match structure_proto::AppStructureNodeKind::try_from(node.kind).map_err(|_| {
        malformed_structure_payload(
            "structure.snapshot.nodes[].kind",
            format!("unknown enum value {}", node.kind),
        )
    })? {
        structure_proto::AppStructureNodeKind::Directory => AppStructureNodeKind::Directory,
        structure_proto::AppStructureNodeKind::ActionFile => AppStructureNodeKind::ActionFile,
        structure_proto::AppStructureNodeKind::SnapshotResource => {
            AppStructureNodeKind::SnapshotResource
        }
        structure_proto::AppStructureNodeKind::LiveResource => AppStructureNodeKind::LiveResource,
        structure_proto::AppStructureNodeKind::StaticJsonResource => {
            AppStructureNodeKind::StaticJsonResource
        }
        structure_proto::AppStructureNodeKind::Unspecified => {
            return Err(malformed_structure_payload(
                "structure.snapshot.nodes[].kind",
                "UNSPECIFIED enum value is not allowed",
            ))
        }
    };
    let manifest_entry = match node.manifest_entry_json {
        Some(json) => Some(parse_structure_json_text(
            &json,
            "structure.snapshot.nodes[].manifest_entry_json",
        )?),
        None => None,
    };
    let seed_content = match node.seed_content_json {
        Some(json) => Some(parse_structure_json_text(
            &json,
            "structure.snapshot.nodes[].seed_content_json",
        )?),
        None => None,
    };
    Ok(AppStructureNode {
        path: node.path,
        kind,
        manifest_entry,
        seed_content,
        mutable: node.r#mutable,
        scope: node.scope,
    })
}

fn parse_connector_json_text(text: &str, field: &str) -> Result<JsonValue, ConnectorError> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: format!("bridge grpc invalid json in {field}: {err}"),
        retryable: true,
        details: None,
    })
}

fn parse_structure_json_text(text: &str, field: &str) -> Result<JsonValue, ConnectorError> {
    serde_json::from_str::<JsonValue>(text).map_err(|err| ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: format!("bridge grpc invalid json in {field}: {err}"),
        retryable: true,
        details: None,
    })
}

fn parse_connector_json_object_text(text: &str, field: &str) -> Result<JsonValue, ConnectorError> {
    let value = parse_connector_json_text(text, field)?;
    if !value.is_object() {
        return Err(malformed_payload(
            field,
            "expected JSON object, got non-object JSON",
        ));
    }
    Ok(value)
}

fn parse_optional_cursor(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, ConnectorError> {
    match value {
        Some(s) if s.is_empty() => Err(malformed_payload(
            field,
            "empty string is not allowed for optional cursor",
        )),
        Some(s) => Ok(Some(s)),
        None => Ok(None),
    }
}

fn malformed_payload(field: &str, reason: impl std::fmt::Display) -> ConnectorError {
    ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
        message: format!("bridge grpc malformed payload in {field}: {reason}"),
        retryable: false,
        details: None,
    }
}

fn malformed_structure_payload(field: &str, reason: impl std::fmt::Display) -> ConnectorError {
    ConnectorError {
        code: connector_error_codes::INTERNAL.to_string(),
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
        parse_connector_json_text, to_proto_execution_mode, to_proto_input_mode,
        GrpcBridgeAdapterV1, GrpcBridgeConnector,
    };
    use agentfs_sdk::{
        ActionExecutionMode, AdapterControlActionV1, AdapterControlOutcomeV1,
        AdapterExecutionModeV1, AdapterInputModeV1, AdapterSubmitOutcomeV1, AppAdapterV1,
        AppConnector, AppStructureSyncReason, AppStructureSyncResult, ConnectorContext,
        FetchLivePageRequest, FetchSnapshotChunkRequest, GetAppStructureRequest,
        RefreshAppStructureRequest, RequestContextV1, SnapshotResume, SubmitActionOutcome,
        SubmitActionRequest,
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
        let err = parse_connector_json_text("not-json", "f").expect_err("should fail");
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
    struct TestConnectorService;

    #[derive(Default)]
    struct TestStructureConnectorService;

    #[tonic::async_trait]
    impl super::connector_proto::appfs_connector_server::AppfsConnector for TestConnectorService {
        async fn get_connector_info(
            &self,
            _request: Request<super::connector_proto::GetConnectorInfoRequest>,
        ) -> Result<Response<super::connector_proto::GetConnectorInfoResponse>, Status> {
            Ok(Response::new(
                super::connector_proto::GetConnectorInfoResponse {
                    result: Some(
                        super::connector_proto::get_connector_info_response::Result::Info(
                            super::connector_proto::ConnectorInfo {
                                connector_id: "mock-grpc".to_string(),
                                version: "0.3.0-test".to_string(),
                                app_id: "aiim".to_string(),
                                transport: super::connector_proto::ConnectorTransport::GrpcBridge
                                    as i32,
                                supports_snapshot: true,
                                supports_live: true,
                                supports_action: true,
                                optional_features: vec!["demo_mode".to_string()],
                            },
                        ),
                    ),
                },
            ))
        }

        async fn health(
            &self,
            _request: Request<super::connector_proto::HealthRequest>,
        ) -> Result<Response<super::connector_proto::HealthResponse>, Status> {
            Ok(Response::new(super::connector_proto::HealthResponse {
                result: Some(super::connector_proto::health_response::Result::Status(
                    super::connector_proto::HealthStatus {
                        healthy: true,
                        auth_status: super::connector_proto::AuthStatus::Valid as i32,
                        message: Some("ok".to_string()),
                        checked_at: "2026-03-24T00:00:00Z".to_string(),
                    },
                )),
            }))
        }

        async fn prewarm_snapshot_meta(
            &self,
            _request: Request<super::connector_proto::PrewarmSnapshotMetaRequest>,
        ) -> Result<Response<super::connector_proto::PrewarmSnapshotMetaResponse>, Status> {
            Ok(Response::new(
                super::connector_proto::PrewarmSnapshotMetaResponse {
                    result: Some(
                        super::connector_proto::prewarm_snapshot_meta_response::Result::Meta(
                            super::connector_proto::SnapshotMeta {
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
            request: Request<super::connector_proto::FetchSnapshotChunkRequest>,
        ) -> Result<Response<super::connector_proto::FetchSnapshotChunkResponse>, Status> {
            let inner = request.into_inner();
            let response = match inner.request {
                Some(req) => match req
                    .resume
                    .and_then(|resume| resume.kind)
                    .unwrap_or(super::connector_proto::snapshot_resume::Kind::Start(true))
                {
                    super::connector_proto::snapshot_resume::Kind::Start(_) => {
                        super::connector_proto::FetchSnapshotChunkResponse {
                            result: Some(
                                super::connector_proto::fetch_snapshot_chunk_response::Result::Response(
                                    super::connector_proto::SnapshotChunkResponse {
                                        records: vec![super::connector_proto::SnapshotRecord {
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
                    super::connector_proto::snapshot_resume::Kind::Cursor(cursor) => {
                        if cursor != "cursor-1" {
                            super::connector_proto::FetchSnapshotChunkResponse {
                                result: Some(
                                    super::connector_proto::fetch_snapshot_chunk_response::Result::Error(
                                        super::connector_proto::ConnectorError {
                                            code: "INVALID_ARGUMENT".to_string(),
                                            message: "unknown cursor".to_string(),
                                            retryable: false,
                                            details: None,
                                        },
                                    ),
                                ),
                            }
                        } else {
                            super::connector_proto::FetchSnapshotChunkResponse {
                                result: Some(super::connector_proto::fetch_snapshot_chunk_response::Result::Response(
                                    super::connector_proto::SnapshotChunkResponse {
                                        records: vec![super::connector_proto::SnapshotRecord {
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
                    _ => super::connector_proto::FetchSnapshotChunkResponse {
                        result: Some(
                            super::connector_proto::fetch_snapshot_chunk_response::Result::Error(
                                super::connector_proto::ConnectorError {
                                    code: "NOT_SUPPORTED".to_string(),
                                    message: "offset unsupported".to_string(),
                                    retryable: false,
                                    details: None,
                                },
                            ),
                        ),
                    },
                },
                None => super::connector_proto::FetchSnapshotChunkResponse {
                    result: Some(
                        super::connector_proto::fetch_snapshot_chunk_response::Result::Error(
                            super::connector_proto::ConnectorError {
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
            request: Request<super::connector_proto::FetchLivePageRequest>,
        ) -> Result<Response<super::connector_proto::FetchLivePageResponse>, Status> {
            let req = request.into_inner().request;
            let (cursor, handle) = match req {
                Some(r) => (r.cursor, r.handle_id.unwrap_or_else(|| "ph-1".to_string())),
                None => (None, "ph-1".to_string()),
            };
            if cursor.as_deref() == Some("invalid") {
                return Ok(Response::new(
                    super::connector_proto::FetchLivePageResponse {
                        result: Some(
                            super::connector_proto::fetch_live_page_response::Result::Error(
                                super::connector_proto::ConnectorError {
                                    code: "CURSOR_INVALID".to_string(),
                                    message: "cursor invalid".to_string(),
                                    retryable: false,
                                    details: None,
                                },
                            ),
                        ),
                    },
                ));
            }
            let page_no = if cursor.as_deref().is_some() { 2 } else { 1 };
            Ok(Response::new(
                super::connector_proto::FetchLivePageResponse {
                    result: Some(
                        super::connector_proto::fetch_live_page_response::Result::Response(
                            super::connector_proto::LivePageResponse {
                                items_json: vec![format!("{{\"id\":\"m-{page_no}\"}}")],
                                page: Some(super::connector_proto::LivePageInfo {
                                    handle_id: handle,
                                    page_no,
                                    has_more: page_no == 1,
                                    mode: super::connector_proto::LiveMode::Live as i32,
                                    expires_at: Some("2026-03-24T00:00:00Z".to_string()),
                                    next_cursor: if page_no == 1 {
                                        Some("cursor-live-1".to_string())
                                    } else {
                                        None
                                    },
                                    retry_after_ms: None,
                                }),
                            },
                        ),
                    ),
                },
            ))
        }

        async fn submit_action(
            &self,
            request: Request<super::connector_proto::SubmitActionRequest>,
        ) -> Result<Response<super::connector_proto::SubmitActionResponse>, Status> {
            let req = request.into_inner().request;
            let Some(req) = req else {
                return Ok(Response::new(
                    super::connector_proto::SubmitActionResponse {
                        result: Some(
                            super::connector_proto::submit_action_response::Result::Error(
                                super::connector_proto::ConnectorError {
                                    code: "INVALID_ARGUMENT".to_string(),
                                    message: "missing request".to_string(),
                                    retryable: false,
                                    details: None,
                                },
                            ),
                        ),
                    },
                ));
            };
            if req.path.ends_with("/rate_limited.act") {
                return Ok(Response::new(
                    super::connector_proto::SubmitActionResponse {
                        result: Some(
                            super::connector_proto::submit_action_response::Result::Error(
                                super::connector_proto::ConnectorError {
                                    code: "RATE_LIMITED".to_string(),
                                    message: "upstream rate limited".to_string(),
                                    retryable: true,
                                    details: None,
                                },
                            ),
                        ),
                    },
                ));
            }
            let result = if req.execution_mode
                == super::connector_proto::ActionExecutionMode::Inline as i32
            {
                super::connector_proto::submit_action_response::Result::Response(
                    super::connector_proto::SubmitActionOutput {
                        request_id: "req-1".to_string(),
                        estimated_duration_ms: Some(12),
                        outcome: Some(super::connector_proto::SubmitActionOutcome {
                            kind: Some(super::connector_proto::submit_action_outcome::Kind::CompletedContentJson(
                                "{\"ok\":true}".to_string(),
                            )),
                        }),
                    },
                )
            } else {
                super::connector_proto::submit_action_response::Result::Response(
                    super::connector_proto::SubmitActionOutput {
                        request_id: "req-2".to_string(),
                        estimated_duration_ms: Some(34),
                        outcome: Some(super::connector_proto::SubmitActionOutcome {
                            kind: Some(
                                super::connector_proto::submit_action_outcome::Kind::StreamingPlan(
                                    super::connector_proto::ActionStreamingPlan {
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
            Ok(Response::new(
                super::connector_proto::SubmitActionResponse {
                    result: Some(result),
                },
            ))
        }
    }

    #[tonic::async_trait]
    impl super::structure_proto::appfs_structure_connector_server::AppfsStructureConnector
        for TestStructureConnectorService
    {
        async fn get_app_structure(
            &self,
            request: Request<super::structure_proto::GetAppStructureRequest>,
        ) -> Result<Response<super::structure_proto::GetAppStructureResponse>, Status> {
            let req = request
                .into_inner()
                .request
                .ok_or_else(|| Status::invalid_argument("missing request"))?;
            let response = if req.known_revision.as_deref() == Some("demo-structure-chat-001") {
                super::structure_proto::GetAppStructureResponse {
                    result: Some(
                        super::structure_proto::get_app_structure_response::Result::Response(
                            super::structure_proto::AppStructureSyncResult {
                                kind: Some(
                                    super::structure_proto::app_structure_sync_result::Kind::Unchanged(
                                        super::structure_proto::AppStructureSyncUnchanged {
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
                super::structure_proto::GetAppStructureResponse {
                    result: Some(
                        super::structure_proto::get_app_structure_response::Result::Response(
                            super::structure_proto::AppStructureSyncResult {
                                kind: Some(
                                    super::structure_proto::app_structure_sync_result::Kind::Snapshot(
                                        super::structure_proto::AppStructureSyncSnapshot {
                                            snapshot: Some(structure_snapshot_proto("chat-001")),
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
            request: Request<super::structure_proto::RefreshAppStructureRequest>,
        ) -> Result<Response<super::structure_proto::RefreshAppStructureResponse>, Status> {
            let req = request
                .into_inner()
                .request
                .ok_or_else(|| Status::invalid_argument("missing request"))?;
            if req.reason == super::structure_proto::AppStructureSyncReason::EnterScope as i32
                && req.target_scope.is_none()
            {
                return Ok(Response::new(
                    super::structure_proto::RefreshAppStructureResponse {
                        result: Some(
                            super::structure_proto::refresh_app_structure_response::Result::Error(
                                super::structure_proto::ConnectorError {
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
            let snapshot = structure_snapshot_proto(&target_scope);
            if req.known_revision.as_deref() == Some(snapshot.revision.as_str()) {
                return Ok(Response::new(super::structure_proto::RefreshAppStructureResponse {
                    result: Some(
                        super::structure_proto::refresh_app_structure_response::Result::Response(
                            super::structure_proto::AppStructureSyncResult {
                                kind: Some(
                                    super::structure_proto::app_structure_sync_result::Kind::Unchanged(
                                        super::structure_proto::AppStructureSyncUnchanged {
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
                super::structure_proto::RefreshAppStructureResponse {
                    result: Some(
                        super::structure_proto::refresh_app_structure_response::Result::Response(
                            super::structure_proto::AppStructureSyncResult {
                                kind: Some(
                                    super::structure_proto::app_structure_sync_result::Kind::Snapshot(
                                        super::structure_proto::AppStructureSyncSnapshot {
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

    fn connector_test_ctx() -> ConnectorContext {
        ConnectorContext {
            app_id: "aiim".to_string(),
            session_id: "sess-grpc".to_string(),
            request_id: "req-grpc".to_string(),
            client_token: Some("tok-grpc".to_string()),
            trace_id: Some("trace-grpc".to_string()),
        }
    }

    async fn spawn_test_grpc_connector_server() -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test grpc connector listener");
        let addr = listener.local_addr().expect("read local addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    super::connector_proto::appfs_connector_server::AppfsConnectorServer::new(
                        TestConnectorService,
                    ),
                )
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("run test grpc connector server");
        });

        (addr, shutdown_tx)
    }

    fn structure_snapshot_proto(scope: &str) -> super::structure_proto::AppStructureSnapshot {
        let mut nodes = vec![
            super::structure_proto::AppStructureNode {
                path: "contacts".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: None,
            },
            super::structure_proto::AppStructureNode {
                path: "contacts/zhangsan".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: None,
            },
            super::structure_proto::AppStructureNode {
                path: "contacts/zhangsan/send_message.act".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::ActionFile as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"contacts/{contact_id}/send_message.act\",\"kind\":\"action\",\"input_mode\":\"json\",\"execution_mode\":\"inline\"}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: true,
                scope: None,
            },
            super::structure_proto::AppStructureNode {
                path: "_app".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: None,
            },
            super::structure_proto::AppStructureNode {
                path: "_app/enter_scope.act".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::ActionFile as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"_app/enter_scope.act\",\"kind\":\"action\",\"input_mode\":\"json\",\"execution_mode\":\"inline\"}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: true,
                scope: None,
            },
            super::structure_proto::AppStructureNode {
                path: "_app/refresh_structure.act".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::ActionFile as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"_app/refresh_structure.act\",\"kind\":\"action\",\"input_mode\":\"json\",\"execution_mode\":\"inline\"}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: true,
                scope: None,
            },
        ];

        if scope == "chat-long" {
            nodes.push(super::structure_proto::AppStructureNode {
                path: "chats".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-long".to_string()),
            });
            nodes.push(super::structure_proto::AppStructureNode {
                path: "chats/chat-long".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-long".to_string()),
            });
            nodes.push(super::structure_proto::AppStructureNode {
                path: "chats/chat-long/messages.res.jsonl".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::SnapshotResource as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"chats/chat-long/messages.res.jsonl\",\"kind\":\"resource\",\"output_mode\":\"jsonl\",\"snapshot\":{\"max_materialized_bytes\":1024,\"prewarm\":true,\"prewarm_timeout_ms\":5000,\"read_through_timeout_ms\":10000,\"on_timeout\":\"return_stale\"}}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-long".to_string()),
            });
        } else {
            nodes.push(super::structure_proto::AppStructureNode {
                path: "chats".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-001".to_string()),
            });
            nodes.push(super::structure_proto::AppStructureNode {
                path: "chats/chat-001".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::Directory as i32,
                manifest_entry_json: None,
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-001".to_string()),
            });
            nodes.push(super::structure_proto::AppStructureNode {
                path: "chats/chat-001/messages.res.jsonl".to_string(),
                kind: super::structure_proto::AppStructureNodeKind::SnapshotResource as i32,
                manifest_entry_json: Some(
                    "{\"template\":\"chats/chat-001/messages.res.jsonl\",\"kind\":\"resource\",\"output_mode\":\"jsonl\",\"snapshot\":{\"max_materialized_bytes\":10485760,\"prewarm\":true,\"prewarm_timeout_ms\":5000,\"read_through_timeout_ms\":10000,\"on_timeout\":\"return_stale\"}}".to_string(),
                ),
                seed_content_json: None,
                r#mutable: false,
                scope: Some("chat-001".to_string()),
            });
        }

        super::structure_proto::AppStructureSnapshot {
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

    async fn spawn_test_grpc_connector_and_structure_server(
    ) -> (SocketAddr, tokio::sync::oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test grpc connector/structure listener");
        let addr = listener.local_addr().expect("read local addr");
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        tokio::spawn(async move {
            tonic::transport::Server::builder()
                .add_service(
                    super::connector_proto::appfs_connector_server::AppfsConnectorServer::new(
                        TestConnectorService,
                    ),
                )
                .add_service(
                    super::structure_proto::appfs_structure_connector_server::AppfsStructureConnectorServer::new(
                        TestStructureConnectorService,
                    ),
                )
                .serve_with_incoming_shutdown(
                    tokio_stream::wrappers::TcpListenerStream::new(listener),
                    async move {
                        let _ = shutdown_rx.await;
                    },
                )
                .await
                .expect("run test grpc connector/structure server");
        });

        (addr, shutdown_tx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_connector_roundtrip() {
        let (addr, shutdown) = spawn_test_grpc_connector_server().await;
        let mut connector = GrpcBridgeConnector::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge connector");

        let info = connector.connector_id().expect("connector info");
        assert_eq!(info.connector_id, "mock-grpc");

        let ctx = connector_test_ctx();
        let health = connector.health(&ctx).expect("health");
        assert!(health.healthy);

        let meta = connector
            .prewarm_snapshot_meta("/messages", Duration::from_millis(200), &ctx)
            .expect("prewarm");
        assert_eq!(meta.revision, Some("rev-1".to_string()));

        let first = connector
            .fetch_snapshot_chunk(
                FetchSnapshotChunkRequest {
                    resource_path: "/messages".to_string(),
                    resume: SnapshotResume::Start,
                    budget_bytes: 1024,
                },
                &ctx,
            )
            .expect("snapshot first");
        assert!(first.has_more);
        assert_eq!(first.records.len(), 1);

        let live = connector
            .fetch_live_page(
                FetchLivePageRequest {
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
                SubmitActionRequest {
                    path: "/send_message.act".to_string(),
                    payload: serde_json::json!({"text":"hi"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx,
            )
            .expect("submit inline");
        match inline.outcome {
            SubmitActionOutcome::Completed { content } => {
                assert_eq!(content["ok"], true);
            }
            _ => panic!("expected completed outcome"),
        }

        let _ = shutdown.send(());
    }

    #[test]
    fn rejects_unspecified_and_unknown_enums() {
        let info_unspecified =
            super::from_proto_connector_info(super::connector_proto::ConnectorInfo {
                connector_id: "c".to_string(),
                version: "v".to_string(),
                app_id: "aiim".to_string(),
                transport: super::connector_proto::ConnectorTransport::Unspecified as i32,
                supports_snapshot: true,
                supports_live: true,
                supports_action: true,
                optional_features: vec![],
            })
            .expect_err("unspecified transport should fail");
        assert!(info_unspecified
            .message
            .contains("connector_info.transport"));

        let info_unknown =
            super::from_proto_connector_info(super::connector_proto::ConnectorInfo {
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

        let health_unspecified =
            super::from_proto_health_status(super::connector_proto::HealthStatus {
                healthy: true,
                auth_status: super::connector_proto::AuthStatus::Unspecified as i32,
                message: Some("ok".to_string()),
                checked_at: "2026-03-24T00:00:00Z".to_string(),
            })
            .expect_err("unspecified auth should fail");
        assert!(health_unspecified.message.contains("health.auth_status"));

        let live_unknown =
            super::from_proto_fetch_live_page_response(super::connector_proto::LivePageResponse {
                items_json: vec!["{\"id\":\"m-1\"}".to_string()],
                page: Some(super::connector_proto::LivePageInfo {
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
            super::connector_proto::SnapshotChunkResponse {
                records: vec![super::connector_proto::SnapshotRecord {
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
            super::connector_proto::SnapshotChunkResponse {
                records: vec![super::connector_proto::SnapshotRecord {
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
            super::from_proto_fetch_live_page_response(super::connector_proto::LivePageResponse {
                items_json: vec!["{\"id\":\"m-1\"}".to_string()],
                page: Some(super::connector_proto::LivePageInfo {
                    handle_id: "ph-1".to_string(),
                    page_no: 1,
                    has_more: true,
                    mode: super::connector_proto::LiveMode::Live as i32,
                    expires_at: None,
                    next_cursor: Some("".to_string()),
                    retry_after_ms: None,
                }),
            })
            .expect_err("empty live next_cursor should fail");
        assert!(live_empty_cursor.message.contains("live.page.next_cursor"));

        let snapshot_empty_cursor = super::from_proto_fetch_snapshot_chunk_response(
            super::connector_proto::SnapshotChunkResponse {
                records: vec![super::connector_proto::SnapshotRecord {
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
    async fn grpc_bridge_connector_error_paths() {
        let (addr, shutdown) = spawn_test_grpc_connector_server().await;
        let mut connector = GrpcBridgeConnector::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge connector");
        let ctx = connector_test_ctx();

        let cursor_err = connector
            .fetch_live_page(
                FetchLivePageRequest {
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
                SubmitActionRequest {
                    path: "/messages/rate_limited.act".to_string(),
                    payload: serde_json::json!({"text":"hi"}),
                    execution_mode: ActionExecutionMode::Inline,
                },
                &ctx,
            )
            .expect_err("rate limited should fail");
        assert_eq!(rate_limit_err.code, "RATE_LIMITED");
        assert!(rate_limit_err.retryable);

        let _ = shutdown.send(());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn grpc_bridge_connector_structure_roundtrip() {
        let (addr, shutdown) = spawn_test_grpc_connector_and_structure_server().await;
        let mut connector = GrpcBridgeConnector::new(
            "aiim".to_string(),
            format!("http://{}", addr),
            Duration::from_millis(1000),
            super::BridgeRuntimeOptions::from_cli(1, 10, 100, 3, 200),
        )
        .expect("create grpc bridge connector with structure");
        let ctx = connector_test_ctx();

        let initial = connector
            .get_app_structure(
                GetAppStructureRequest {
                    app_id: "aiim".to_string(),
                    known_revision: None,
                },
                &ctx,
            )
            .expect("initial structure");
        match initial.result {
            AppStructureSyncResult::Snapshot { snapshot } => {
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
                RefreshAppStructureRequest {
                    app_id: "aiim".to_string(),
                    known_revision: Some("demo-structure-chat-001".to_string()),
                    reason: AppStructureSyncReason::Refresh,
                    target_scope: None,
                    trigger_action_path: Some("/_app/refresh_structure.act".to_string()),
                },
                &ctx,
            )
            .expect("unchanged refresh");
        match refreshed.result {
            AppStructureSyncResult::Unchanged {
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
                RefreshAppStructureRequest {
                    app_id: "aiim".to_string(),
                    known_revision: Some("demo-structure-chat-001".to_string()),
                    reason: AppStructureSyncReason::EnterScope,
                    target_scope: Some("chat-long".to_string()),
                    trigger_action_path: Some("/_app/enter_scope.act".to_string()),
                },
                &ctx,
            )
            .expect("enter scope refresh");
        match enter_scope.result {
            AppStructureSyncResult::Snapshot { snapshot } => {
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
