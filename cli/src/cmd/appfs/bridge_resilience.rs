use std::time::{Duration, Instant};
use tonic::Code;

#[derive(Debug, Clone, Copy)]
pub(super) struct BridgeRuntimeOptions {
    pub max_retries: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub circuit_breaker_failures: u32,
    pub circuit_breaker_cooldown: Duration,
}

impl BridgeRuntimeOptions {
    pub(super) fn from_cli(
        max_retries: u32,
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
        circuit_breaker_failures: u32,
        circuit_breaker_cooldown_ms: u64,
    ) -> Self {
        let mut options = Self {
            max_retries,
            initial_backoff: Duration::from_millis(initial_backoff_ms),
            max_backoff: Duration::from_millis(max_backoff_ms),
            circuit_breaker_failures,
            circuit_breaker_cooldown: Duration::from_millis(circuit_breaker_cooldown_ms),
        };
        options.normalize();
        options
    }

    fn normalize(&mut self) {
        if self.initial_backoff.is_zero() {
            self.initial_backoff = Duration::from_millis(1);
        }
        if self.max_backoff < self.initial_backoff {
            self.max_backoff = self.initial_backoff;
        }
        if self.circuit_breaker_cooldown.is_zero() {
            self.circuit_breaker_cooldown = Duration::from_millis(1);
        }
    }

    pub(super) fn retry_backoff_for_attempt(&self, attempt: u32) -> Duration {
        let retry_index = attempt.saturating_sub(1).min(16);
        let factor: u128 = 1u128 << retry_index;
        let raw = self.initial_backoff.as_millis().saturating_mul(factor);
        let capped = raw.min(self.max_backoff.as_millis());
        Duration::from_millis(capped as u64)
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct BridgeMetrics {
    pub requests_total: u64,
    pub attempts_total: u64,
    pub retries_total: u64,
    pub succeeded_total: u64,
    pub failed_total: u64,
    pub short_circuited_total: u64,
}

impl BridgeMetrics {
    pub(super) fn record_request(&mut self, attempts: u32, success: bool) {
        self.requests_total = self.requests_total.saturating_add(1);
        self.attempts_total = self.attempts_total.saturating_add(u64::from(attempts));
        self.retries_total = self
            .retries_total
            .saturating_add(u64::from(attempts.saturating_sub(1)));
        if success {
            self.succeeded_total = self.succeeded_total.saturating_add(1);
        } else {
            self.failed_total = self.failed_total.saturating_add(1);
        }
    }

    pub(super) fn record_short_circuit(&mut self) {
        self.short_circuited_total = self.short_circuited_total.saturating_add(1);
    }

    pub(super) fn snapshot(&self) -> String {
        format!(
            "requests={} attempts={} retries={} ok={} failed={} short_circuit={}",
            self.requests_total,
            self.attempts_total,
            self.retries_total,
            self.succeeded_total,
            self.failed_total,
            self.short_circuited_total
        )
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct BridgeCircuitBreaker {
    consecutive_failures: u32,
    open_until: Option<Instant>,
    pub opened_total: u64,
}

impl BridgeCircuitBreaker {
    pub(super) fn check_open(&mut self, now: Instant) -> Option<Duration> {
        match self.open_until {
            Some(until) if now < until => Some(until.saturating_duration_since(now)),
            Some(_) => {
                self.open_until = None;
                None
            }
            None => None,
        }
    }

    pub(super) fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.open_until = None;
    }

    pub(super) fn record_failure(&mut self, now: Instant, options: BridgeRuntimeOptions) -> bool {
        if options.circuit_breaker_failures == 0 {
            return false;
        }
        if self.check_open(now).is_some() {
            return true;
        }

        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures < options.circuit_breaker_failures {
            return false;
        }

        self.consecutive_failures = 0;
        self.open_until = Some(now + options.circuit_breaker_cooldown);
        self.opened_total = self.opened_total.saturating_add(1);
        true
    }
}

pub(super) fn is_retryable_http_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

pub(super) fn is_retryable_grpc_code(code: Code) -> bool {
    matches!(
        code,
        Code::DeadlineExceeded
            | Code::Unavailable
            | Code::ResourceExhausted
            | Code::Aborted
            | Code::Internal
            | Code::Unknown
    )
}

#[cfg(test)]
mod tests {
    use super::{
        is_retryable_grpc_code, is_retryable_http_status, BridgeCircuitBreaker,
        BridgeRuntimeOptions,
    };
    use std::time::{Duration, Instant};
    use tonic::Code;

    #[test]
    fn retry_backoff_is_exponential_and_capped() {
        let opts = BridgeRuntimeOptions::from_cli(2, 100, 500, 3, 1000);
        assert_eq!(
            opts.retry_backoff_for_attempt(1),
            Duration::from_millis(100)
        );
        assert_eq!(
            opts.retry_backoff_for_attempt(2),
            Duration::from_millis(200)
        );
        assert_eq!(
            opts.retry_backoff_for_attempt(3),
            Duration::from_millis(400)
        );
        assert_eq!(
            opts.retry_backoff_for_attempt(4),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn circuit_breaker_opens_after_threshold() {
        let now = Instant::now();
        let mut breaker = BridgeCircuitBreaker::default();
        let opts = BridgeRuntimeOptions::from_cli(1, 10, 100, 2, 200);

        assert!(!breaker.record_failure(now, opts));
        assert!(breaker.record_failure(now + Duration::from_millis(1), opts));
        assert!(breaker
            .check_open(now + Duration::from_millis(50))
            .is_some());
        assert!(breaker
            .check_open(now + Duration::from_millis(250))
            .is_none());
    }

    #[test]
    fn retryable_code_mapping_matches_policy() {
        assert!(is_retryable_http_status(503));
        assert!(!is_retryable_http_status(400));
        assert!(is_retryable_grpc_code(Code::Unavailable));
        assert!(!is_retryable_grpc_code(Code::InvalidArgument));
    }
}
