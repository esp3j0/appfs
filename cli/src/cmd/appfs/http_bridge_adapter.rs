use super::bridge_resilience::{
    is_retryable_http_status, BridgeCircuitBreaker, BridgeMetrics, BridgeRuntimeOptions,
};
use agentfs_sdk::{
    AdapterControlActionV1, AdapterControlOutcomeV1, AdapterErrorV1, AdapterExecutionModeV1,
    AdapterInputModeV1, AdapterSubmitOutcomeV1, AppAdapterV1, RequestContextV1,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::{Duration, Instant};

pub(super) struct HttpBridgeAdapterV1 {
    app_id: String,
    endpoint: String,
    timeout: Duration,
    runtime_options: BridgeRuntimeOptions,
    metrics: BridgeMetrics,
    circuit_breaker: BridgeCircuitBreaker,
}

#[derive(Debug, Serialize)]
struct SubmitActionRequest {
    app_id: String,
    path: String,
    payload: String,
    input_mode: AdapterInputModeV1,
    execution_mode: AdapterExecutionModeV1,
    context: RequestContextV1,
}

#[derive(Debug, Serialize)]
struct SubmitControlRequest {
    app_id: String,
    path: String,
    action: AdapterControlActionV1,
    context: RequestContextV1,
}

#[derive(Debug, Deserialize)]
struct BridgeErrorPayload {
    code: String,
    message: String,
    #[serde(default)]
    retryable: bool,
}

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
        if let Some(remaining) = self.circuit_breaker.check_open(Instant::now()) {
            self.metrics.record_short_circuit();
            let message = format!(
                "bridge circuit open for route={route}; retry_in_ms={} metrics={}",
                remaining.as_millis(),
                self.metrics.snapshot()
            );
            if self.metrics.short_circuited_total <= 3
                || self.metrics.short_circuited_total % 10 == 0
            {
                eprintln!("AppFS bridge http short-circuit: {message}");
            }
            return Err(AdapterErrorV1::Internal { message });
        }

        let url = format!("{}/{}", self.endpoint, route.trim_start_matches('/'));
        let max_attempts = self.runtime_options.max_retries.saturating_add(1).max(1);
        let started = Instant::now();
        let mut attempt = 0u32;

        loop {
            attempt = attempt.saturating_add(1);
            let agent = ureq::AgentBuilder::new().timeout(self.timeout).build();
            let request = agent.post(&url);

            match request.send_json(req) {
                Ok(response) => {
                    let parsed = match response.into_json::<Resp>() {
                        Ok(value) => value,
                        Err(err) => {
                            let opened = self
                                .circuit_breaker
                                .record_failure(Instant::now(), self.runtime_options);
                            self.metrics.record_request(attempt, false);
                            self.log_observation(route, attempt, started.elapsed(), "failed");
                            return Err(AdapterErrorV1::Internal {
                                message: format!(
                                    "bridge decode error for {url}: {err} (attempts={} circuit_opened={} metrics={})",
                                    attempt,
                                    opened,
                                    self.metrics.snapshot()
                                ),
                            });
                        }
                    };

                    self.circuit_breaker.record_success();
                    self.metrics.record_request(attempt, true);
                    self.log_observation(route, attempt, started.elapsed(), "ok");
                    return Ok(parsed);
                }
                Err(ureq::Error::Status(status, response)) => {
                    let body = response.into_string().unwrap_or_default();
                    let retryable = is_retryable_http_status(status);
                    if retryable && attempt < max_attempts {
                        let backoff = self.runtime_options.retry_backoff_for_attempt(attempt);
                        eprintln!(
                            "AppFS bridge http retry route={} attempt={}/{} status={} backoff_ms={}",
                            route,
                            attempt,
                            max_attempts,
                            status,
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
                                "AppFS bridge http circuit opened after status={} route={} metrics={}",
                                status,
                                route,
                                self.metrics.snapshot()
                            );
                        }
                    } else {
                        self.circuit_breaker.record_success();
                    }
                    self.metrics.record_request(attempt, false);
                    self.log_observation(route, attempt, started.elapsed(), "failed");
                    return Err(map_status_error(status, &body));
                }
                Err(ureq::Error::Transport(err)) => {
                    if attempt < max_attempts {
                        let backoff = self.runtime_options.retry_backoff_for_attempt(attempt);
                        eprintln!(
                            "AppFS bridge http retry route={} attempt={}/{} transport_error={} backoff_ms={}",
                            route,
                            attempt,
                            max_attempts,
                            err,
                            backoff.as_millis()
                        );
                        std::thread::sleep(backoff);
                        continue;
                    }

                    let opened = self
                        .circuit_breaker
                        .record_failure(Instant::now(), self.runtime_options);
                    self.metrics.record_request(attempt, false);
                    self.log_observation(route, attempt, started.elapsed(), "failed");
                    return Err(AdapterErrorV1::Internal {
                        message: format!(
                            "bridge transport error for {url}: {err} (attempts={} circuit_opened={} metrics={})",
                            attempt,
                            opened,
                            self.metrics.snapshot()
                        ),
                    });
                }
            }
        }
    }

    fn log_observation(&self, route: &str, attempts: u32, elapsed: Duration, outcome: &str) {
        if attempts > 1 || outcome != "ok" || self.metrics.requests_total % 50 == 0 {
            eprintln!(
                "AppFS bridge http metrics route={} outcome={} attempts={} latency_ms={} {}",
                route,
                outcome,
                attempts,
                elapsed.as_millis(),
                self.metrics.snapshot()
            );
        }
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

fn map_status_error(status: u16, body: &str) -> AdapterErrorV1 {
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

#[cfg(test)]
mod tests {
    use super::map_status_error;
    use agentfs_sdk::AdapterErrorV1;

    #[test]
    fn map_status_error_accepts_adapter_error_shape() {
        let err = map_status_error(
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
    fn map_status_error_accepts_simple_error_shape() {
        let err = map_status_error(
            403,
            r#"{"code":"PERMISSION_DENIED","message":"forbidden","retryable":false}"#,
        );
        match err {
            AdapterErrorV1::Rejected {
                code,
                message,
                retryable,
            } => {
                assert_eq!(code, "PERMISSION_DENIED");
                assert_eq!(message, "forbidden");
                assert!(!retryable);
            }
            _ => panic!("expected rejected error"),
        }
    }
}
