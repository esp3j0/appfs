use super::bridge_resilience::{
    is_retryable_http_status, BridgeCircuitBreaker, BridgeMetrics, BridgeRuntimeOptions,
};
use agentfs_sdk::{
    connector_error_codes, AppConnector, ConnectorContext, ConnectorError, ConnectorInfo,
    FetchLivePageRequest, FetchLivePageResponse, FetchSnapshotChunkRequest,
    FetchSnapshotChunkResponse, GetAppStructureRequest, GetAppStructureResponse, HealthStatus,
    RefreshAppStructureRequest, RefreshAppStructureResponse, SnapshotMeta,
    SubmitActionRequest as ConnectorSubmitActionRequest,
    SubmitActionResponse as ConnectorSubmitActionResponse,
};
use agentfs_sdk::{
    AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
    AdapterInputModeV1, AdapterSubmitOutcomeV1, AppAdapterV1, RequestContextV1,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::{Duration, Instant};

#[allow(dead_code)]
pub(super) struct HttpBridgeAdapterV1 {
    app_id: String,
    endpoint: String,
    timeout: Duration,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

pub(super) struct HttpBridgeConnector {
    endpoint: String,
    timeout: Duration,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct SubmitActionRequest {
    app_id: String,
    path: String,
    payload: String,
    input_mode: AdapterInputModeV1,
    execution_mode: AdapterExecutionModeV1,
    context: RequestContextV1,
}

#[derive(Debug, Serialize)]
#[allow(dead_code)]
struct SubmitControlRequest {
    app_id: String,
    path: String,
    action: AdapterControlActionV1,
    context: RequestContextV1,
}

#[derive(Debug, Serialize)]
struct ContextOnlyRequest {
    context: ConnectorContext,
}

#[derive(Debug, Serialize)]
struct WrappedRequest<T> {
    context: ConnectorContext,
    request: T,
}

#[derive(Debug, Serialize)]
struct PrewarmRequest {
    resource_path: String,
    timeout_ms: u64,
}

#[derive(Debug, Deserialize)]
struct BridgeErrorPayload {
    code: String,
    message: String,
    #[serde(default)]
    retryable: bool,
    #[serde(default)]
    details: Option<String>,
}

#[allow(dead_code)]
impl HttpBridgeAdapterV1 {
    pub(super) fn new(
        app_id: String,
        endpoint: String,
        timeout: Duration,
        runtime_options: BridgeRuntimeOptions,
    ) -> Self {
        Self {
            app_id,
            endpoint: endpoint.trim_end_matches('/').to_string(),
            timeout,
            runtime_options,
            metrics: BridgeMetrics::default(),
            circuit_breaker: BridgeCircuitBreaker::default(),
        }
    }

    fn post_json<Req, Resp>(
        &mut self,
        route: &str,
        req: &Req,
    ) -> std::result::Result<Resp, AdapterErrorV1>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        post_json_v1(
            &self.endpoint,
            self.timeout,
            self.runtime_options,
            &mut self.metrics,
            &mut self.circuit_breaker,
            route,
            req,
            map_status_error_v1,
        )
    }
}

impl HttpBridgeConnector {
    pub(super) fn new(
        _app_id: String,
        endpoint: String,
        timeout: Duration,
        runtime_options: BridgeRuntimeOptions,
    ) -> Self {
        Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            timeout,
            runtime_options,
            metrics: BridgeMetrics::default(),
            circuit_breaker: BridgeCircuitBreaker::default(),
        }
    }

    fn post_json<Req, Resp>(
        &mut self,
        route: &str,
        req: &Req,
    ) -> std::result::Result<Resp, ConnectorError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        post_json_connector(
            &self.endpoint,
            self.timeout,
            self.runtime_options,
            &mut self.metrics,
            &mut self.circuit_breaker,
            route,
            req,
            map_status_error_connector,
        )
    }
}

impl AppAdapterV1 for HttpBridgeAdapterV1 {
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
    ) -> std::result::Result<AdapterSubmitOutcomeV1, AdapterErrorV1> {
        let request = SubmitActionRequest {
            app_id: self.app_id.clone(),
            path: path.to_string(),
            payload: payload.to_string(),
            input_mode,
            execution_mode,
            context: ctx.clone(),
        };
        self.post_json("v1/submit-action", &request)
    }

    fn submit_control_action(
        &mut self,
        path: &str,
        action: AdapterControlActionV1,
        ctx: &RequestContextV1,
    ) -> std::result::Result<AdapterControlOutcomeV1, AdapterErrorV1> {
        let request = SubmitControlRequest {
            app_id: self.app_id.clone(),
            path: path.to_string(),
            action,
            context: ctx.clone(),
        };
        self.post_json("v1/submit-control-action", &request)
    }
}

impl AppConnector for HttpBridgeConnector {
    fn connector_id(&self) -> std::result::Result<ConnectorInfo, ConnectorError> {
        let url = format!("{}/{}", self.endpoint, "connector/info");
        let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
        match agent.post(&url).send_json(serde_json::json!({})) {
            Ok(response) => response
                .into_json::<ConnectorInfo>()
                .map_err(|err| ConnectorError {
                    code: connector_error_codes::INTERNAL.to_string(),
                    message: format!("bridge decode error for {url}: {err}"),
                    retryable: true,
                    details: None,
                }),
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                Err(map_status_error_connector(status, &body))
            }
            Err(ureq::Error::Transport(err)) => Err(ConnectorError {
                code: connector_error_codes::INTERNAL.to_string(),
                message: format!("bridge transport error for {url}: {err}"),
                retryable: true,
                details: None,
            }),
        }
    }

    fn health(
        &mut self,
        ctx: &ConnectorContext,
    ) -> std::result::Result<HealthStatus, ConnectorError> {
        let request = ContextOnlyRequest {
            context: ctx.clone(),
        };
        self.post_json("connector/health", &request)
    }

    fn prewarm_snapshot_meta(
        &mut self,
        resource_path: &str,
        timeout: Duration,
        ctx: &ConnectorContext,
    ) -> std::result::Result<SnapshotMeta, ConnectorError> {
        let timeout_ms = timeout.as_millis().max(1).min(u128::from(u64::MAX)) as u64;
        let request = WrappedRequest {
            context: ctx.clone(),
            request: PrewarmRequest {
                resource_path: resource_path.to_string(),
                timeout_ms,
            },
        };
        self.post_json("connector/snapshot/prewarm", &request)
    }

    fn fetch_snapshot_chunk(
        &mut self,
        request: FetchSnapshotChunkRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchSnapshotChunkResponse, ConnectorError> {
        let wrapped = WrappedRequest {
            context: ctx.clone(),
            request,
        };
        self.post_json("connector/snapshot/fetch-chunk", &wrapped)
    }

    fn fetch_live_page(
        &mut self,
        request: FetchLivePageRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<FetchLivePageResponse, ConnectorError> {
        let wrapped = WrappedRequest {
            context: ctx.clone(),
            request,
        };
        self.post_json("connector/live/fetch-page", &wrapped)
    }

    fn submit_action(
        &mut self,
        request: ConnectorSubmitActionRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<ConnectorSubmitActionResponse, ConnectorError> {
        let wrapped = WrappedRequest {
            context: ctx.clone(),
            request,
        };
        self.post_json("connector/action/submit", &wrapped)
    }

    fn get_app_structure(
        &mut self,
        request: GetAppStructureRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<GetAppStructureResponse, ConnectorError> {
        let wrapped = WrappedRequest {
            context: ctx.clone(),
            request,
        };
        self.post_json("connector/structure/get", &wrapped)
    }

    fn refresh_app_structure(
        &mut self,
        request: RefreshAppStructureRequest,
        ctx: &ConnectorContext,
    ) -> std::result::Result<RefreshAppStructureResponse, ConnectorError> {
        let wrapped = WrappedRequest {
            context: ctx.clone(),
            request,
        };
        self.post_json("connector/structure/refresh", &wrapped)
    }
}

#[allow(clippy::too_many_arguments, dead_code)]
fn post_json_v1<Req, Resp>(
    endpoint: &str,
    timeout: Duration,
    runtime_options: BridgeRuntimeOptions,
    metrics: &mut BridgeMetrics,
    circuit_breaker: &mut BridgeCircuitBreaker,
    route: &str,
    req: &Req,
    map_status_error: fn(u16, &str) -> AdapterErrorV1,
) -> std::result::Result<Resp, AdapterErrorV1>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    if let Some(remaining) = circuit_breaker.check_open(Instant::now()) {
        metrics.record_short_circuit();
        return Err(AdapterErrorV1::Internal {
            message: format!(
                "bridge circuit open for route={route}; retry_in_ms={} metrics={}",
                remaining.as_millis(),
                metrics.snapshot()
            ),
        });
    }

    let url = format!("{}/{}", endpoint, route.trim_start_matches('/'));
    let max_attempts = runtime_options.max_retries.saturating_add(1).max(1);
    let started = Instant::now();
    let mut attempt = 0u32;

    loop {
        attempt = attempt.saturating_add(1);
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        let request = agent.post(&url);

        match request.send_json(req) {
            Ok(response) => {
                let parsed = match response.into_json::<Resp>() {
                    Ok(value) => value,
                    Err(err) => {
                        let opened =
                            circuit_breaker.record_failure(Instant::now(), runtime_options);
                        metrics.record_request(attempt, false);
                        log_observation(metrics, route, attempt, started.elapsed(), "failed");
                        return Err(AdapterErrorV1::Internal {
                            message: format!(
                                "bridge decode error for {url}: {err} (attempts={} circuit_opened={} metrics={})",
                                attempt,
                                opened,
                                metrics.snapshot()
                            ),
                        });
                    }
                };

                circuit_breaker.record_success();
                metrics.record_request(attempt, true);
                log_observation(metrics, route, attempt, started.elapsed(), "ok");
                return Ok(parsed);
            }
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let retryable = is_retryable_http_status(status);
                if retryable && attempt < max_attempts {
                    std::thread::sleep(runtime_options.retry_backoff_for_attempt(attempt));
                    continue;
                }

                if retryable {
                    circuit_breaker.record_failure(Instant::now(), runtime_options);
                } else {
                    circuit_breaker.record_success();
                }
                metrics.record_request(attempt, false);
                log_observation(metrics, route, attempt, started.elapsed(), "failed");
                return Err(map_status_error(status, &body));
            }
            Err(ureq::Error::Transport(err)) => {
                if attempt < max_attempts {
                    std::thread::sleep(runtime_options.retry_backoff_for_attempt(attempt));
                    continue;
                }

                let opened = circuit_breaker.record_failure(Instant::now(), runtime_options);
                metrics.record_request(attempt, false);
                log_observation(metrics, route, attempt, started.elapsed(), "failed");
                return Err(AdapterErrorV1::Internal {
                    message: format!(
                        "bridge transport error for {url}: {err} (attempts={} circuit_opened={} metrics={})",
                        attempt,
                        opened,
                        metrics.snapshot()
                    ),
                });
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn post_json_connector<Req, Resp>(
    endpoint: &str,
    timeout: Duration,
    runtime_options: BridgeRuntimeOptions,
    metrics: &mut BridgeMetrics,
    circuit_breaker: &mut BridgeCircuitBreaker,
    route: &str,
    req: &Req,
    map_status_error: fn(u16, &str) -> ConnectorError,
) -> std::result::Result<Resp, ConnectorError>
where
    Req: Serialize,
    Resp: DeserializeOwned,
{
    if let Some(remaining) = circuit_breaker.check_open(Instant::now()) {
        metrics.record_short_circuit();
        return Err(ConnectorError {
            code: connector_error_codes::INTERNAL.to_string(),
            message: format!(
                "bridge circuit open for route={route}; retry_in_ms={} metrics={}",
                remaining.as_millis(),
                metrics.snapshot()
            ),
            retryable: true,
            details: None,
        });
    }

    let url = format!("{}/{}", endpoint, route.trim_start_matches('/'));
    let max_attempts = runtime_options.max_retries.saturating_add(1).max(1);
    let started = Instant::now();
    let mut attempt = 0u32;

    loop {
        attempt = attempt.saturating_add(1);
        let agent = ureq::AgentBuilder::new().timeout(timeout).build();
        let request = agent.post(&url);

        match request.send_json(req) {
            Ok(response) => {
                let parsed = match response.into_json::<Resp>() {
                    Ok(value) => value,
                    Err(err) => {
                        let opened =
                            circuit_breaker.record_failure(Instant::now(), runtime_options);
                        if opened {
                            eprintln!(
                                "AppFS bridge http circuit opened after {} decode failure route={} {}",
                                connector_error_codes::INTERNAL,
                                route,
                                metrics.snapshot()
                            );
                        }
                        metrics.record_request(attempt, false);
                        log_observation(metrics, route, attempt, started.elapsed(), "failed");
                        return Err(ConnectorError {
                            code: connector_error_codes::INTERNAL.to_string(),
                            message: format!(
                                "bridge decode error for {url}: {err} (attempts={} circuit_opened={} metrics={})",
                                attempt,
                                opened,
                                metrics.snapshot()
                            ),
                            retryable: true,
                            details: None,
                        });
                    }
                };

                circuit_breaker.record_success();
                metrics.record_request(attempt, true);
                log_observation(metrics, route, attempt, started.elapsed(), "ok");
                return Ok(parsed);
            }
            Err(ureq::Error::Status(status, response)) => {
                let body = response.into_string().unwrap_or_default();
                let retryable = is_retryable_http_status(status);
                if retryable && attempt < max_attempts {
                    std::thread::sleep(runtime_options.retry_backoff_for_attempt(attempt));
                    continue;
                }

                if retryable {
                    let opened = circuit_breaker.record_failure(Instant::now(), runtime_options);
                    if opened {
                        eprintln!(
                            "AppFS bridge http circuit opened after retryable status failure code={} route={} {}",
                            status,
                            route,
                            metrics.snapshot()
                        );
                    }
                } else {
                    circuit_breaker.record_success();
                }
                metrics.record_request(attempt, false);
                log_observation(metrics, route, attempt, started.elapsed(), "failed");
                return Err(map_status_error(status, &body));
            }
            Err(ureq::Error::Transport(err)) => {
                if attempt < max_attempts {
                    std::thread::sleep(runtime_options.retry_backoff_for_attempt(attempt));
                    continue;
                }

                let opened = circuit_breaker.record_failure(Instant::now(), runtime_options);
                if opened {
                    eprintln!(
                        "AppFS bridge http circuit opened after transport failure route={} {}",
                        route,
                        metrics.snapshot()
                    );
                }
                metrics.record_request(attempt, false);
                log_observation(metrics, route, attempt, started.elapsed(), "failed");
                return Err(ConnectorError {
                    code: connector_error_codes::INTERNAL.to_string(),
                    message: format!(
                        "bridge transport error for {url}: {err} (attempts={} circuit_opened={} metrics={})",
                        attempt,
                        opened,
                        metrics.snapshot()
                    ),
                    retryable: true,
                    details: None,
                });
            }
        }
    }
}

fn log_observation(
    metrics: &BridgeMetrics,
    route: &str,
    attempts: u32,
    elapsed: Duration,
    outcome: &str,
) {
    if attempts > 1 || outcome != "ok" || metrics.requests_total.is_multiple_of(50) {
        eprintln!(
            "AppFS bridge http metrics route={} outcome={} attempts={} latency_ms={} {}",
            route,
            outcome,
            attempts,
            elapsed.as_millis(),
            metrics.snapshot()
        );
    }
}

#[allow(dead_code)]
fn map_status_error_v1(status: u16, body: &str) -> AdapterErrorV1 {
    if let Ok(adapter_error) = serde_json::from_str::<AdapterErrorV1>(body) {
        return adapter_error;
    }
    if let Ok(payload) = serde_json::from_str::<BridgeErrorPayload>(body) {
        return AdapterErrorV1::Rejected {
            code: payload.code,
            message: payload.message,
            retryable: payload.retryable,
        };
    }
    AdapterErrorV1::Internal {
        message: if body.trim().is_empty() {
            format!("bridge http status {status}")
        } else {
            format!("bridge http status {status}: {body}")
        },
    }
}

fn map_status_error_connector(status: u16, body: &str) -> ConnectorError {
    if let Ok(err) = serde_json::from_str::<ConnectorError>(body) {
        return err;
    }
    if let Ok(payload) = serde_json::from_str::<BridgeErrorPayload>(body) {
        return ConnectorError {
            code: payload.code,
            message: payload.message,
            retryable: payload.retryable,
            details: payload.details,
        };
    }
    if let Ok(adapter_error) = serde_json::from_str::<AdapterErrorV1>(body) {
        match adapter_error {
            AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            } => {
                return ConnectorError {
                    code,
                    message,
                    retryable,
                    details: None,
                };
            }
            AdapterErrorV1::Internal { message } => {
                return ConnectorError {
                    code: connector_error_codes::INTERNAL.to_string(),
                    message,
                    retryable: true,
                    details: None,
                };
            }
        }
    }

    ConnectorError {
        code: if is_retryable_http_status(status) {
            connector_error_codes::UPSTREAM_UNAVAILABLE.to_string()
        } else {
            connector_error_codes::INTERNAL.to_string()
        },
        message: if body.trim().is_empty() {
            format!("bridge http status {status}")
        } else {
            format!("bridge http status {status}: {body}")
        },
        retryable: is_retryable_http_status(status),
        details: None,
    }
}

#[cfg(test)]
mod tests {
    use super::{map_status_error_connector, map_status_error_v1};
    use agentfs_sdk::{connector_error_codes, AdapterErrorV1};

    #[test]
    fn map_status_error_v1_accepts_adapter_error_shape() {
        let err = map_status_error_v1(
            400,
            r#"{"kind":"rejected","code":"INVALID_ARGUMENT","message":"bad payload","retryable":false}"#,
        );
        match err {
            AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            } => {
                assert_eq!(code, "INVALID_ARGUMENT");
                assert_eq!(message, "bad payload");
                assert!(!retryable);
            }
            _ => panic!("expected rejected error"),
        }
    }

    #[test]
    fn map_status_error_connector_accepts_connector_shape() {
        let err = map_status_error_connector(
            429,
            r#"{"code":"RATE_LIMITED","message":"limited","retryable":true,"details":"x"}"#,
        );
        assert_eq!(err.code, "RATE_LIMITED");
        assert_eq!(err.message, "limited");
        assert!(err.retryable);
        assert_eq!(err.details.as_deref(), Some("x"));
    }

    #[test]
    fn map_status_error_connector_fallback_sets_retryable_for_503() {
        let err = map_status_error_connector(503, "upstream down");
        assert_eq!(err.code, connector_error_codes::UPSTREAM_UNAVAILABLE);
        assert!(err.retryable);
    }
}
