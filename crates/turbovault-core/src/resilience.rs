//! Resilience patterns: retry logic, circuit breakers, graceful degradation

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::time::sleep;

/// Retry configuration
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts
    pub max_attempts: u32,
    /// Initial backoff duration
    pub initial_backoff: Duration,
    /// Maximum backoff duration
    pub max_backoff: Duration,
    /// Backoff multiplier (exponential)
    pub backoff_multiplier: f64,
}

impl RetryConfig {
    /// Conservative defaults for vault operations
    pub fn conservative() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_secs(5),
            backoff_multiplier: 2.0,
        }
    }

    /// Aggressive retries for critical operations
    pub fn aggressive() -> Self {
        Self {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(50),
            max_backoff: Duration::from_secs(10),
            backoff_multiplier: 1.5,
        }
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Exponential backoff retry executor
pub async fn retry_with_backoff<F, T, E>(config: &RetryConfig, mut f: F) -> Result<T, E>
where
    F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send>>,
{
    let mut backoff = config.initial_backoff;
    let mut attempt = 0;

    loop {
        attempt += 1;
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt >= config.max_attempts {
                    return Err(e);
                }
                sleep(backoff).await;
                backoff = Duration::from_secs_f64(
                    (backoff.as_secs_f64() * config.backoff_multiplier)
                        .min(config.max_backoff.as_secs_f64()),
                );
            }
        }
    }
}

/// Circuit breaker states
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed (normal operation)
    Closed,
    /// Circuit is open (rejecting requests)
    Open,
    /// Circuit is half-open (testing recovery)
    HalfOpen,
}

/// Circuit breaker for preventing cascading failures
pub struct CircuitBreaker {
    state: Arc<std::sync::Mutex<CircuitState>>,
    failure_count: Arc<AtomicU32>,
    success_count: Arc<AtomicU32>,
    failure_threshold: u32,
    success_threshold: u32,
    timeout: Duration,
    last_state_change: Arc<std::sync::Mutex<Instant>>,
}

impl CircuitBreaker {
    /// Create new circuit breaker
    pub fn new(failure_threshold: u32, success_threshold: u32, timeout: Duration) -> Self {
        Self {
            state: Arc::new(std::sync::Mutex::new(CircuitState::Closed)),
            failure_count: Arc::new(AtomicU32::new(0)),
            success_count: Arc::new(AtomicU32::new(0)),
            failure_threshold,
            success_threshold,
            timeout,
            last_state_change: Arc::new(std::sync::Mutex::new(Instant::now())),
        }
    }

    /// Check if request is allowed
    pub fn is_request_allowed(&self) -> bool {
        let mut state = self.state.lock().unwrap();
        let last_change = self.last_state_change.lock().unwrap();

        match *state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Open -> Half-Open after timeout
                if last_change.elapsed() >= self.timeout {
                    *state = CircuitState::HalfOpen;
                    self.failure_count.store(0, Ordering::SeqCst);
                    self.success_count.store(0, Ordering::SeqCst);
                    true
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => {
                // In half-open, allow limited requests
                true
            }
        }
    }

    /// Record operation success
    pub fn record_success(&self) {
        let mut state = self.state.lock().unwrap();

        self.failure_count.store(0, Ordering::SeqCst);
        self.success_count.fetch_add(1, Ordering::SeqCst);

        if *state == CircuitState::HalfOpen
            && self.success_count.load(Ordering::SeqCst) >= self.success_threshold
        {
            *state = CircuitState::Closed;
            self.success_count.store(0, Ordering::SeqCst);
            if let Ok(mut last_change) = self.last_state_change.lock() {
                *last_change = Instant::now();
            }
        }
    }

    /// Record operation failure
    pub fn record_failure(&self) {
        let mut state = self.state.lock().unwrap();

        self.success_count.store(0, Ordering::SeqCst);
        self.failure_count.fetch_add(1, Ordering::SeqCst);

        if self.failure_count.load(Ordering::SeqCst) >= self.failure_threshold {
            *state = CircuitState::Open;
            if let Ok(mut last_change) = self.last_state_change.lock() {
                *last_change = Instant::now();
            }
        }
    }

    /// Get current state
    pub fn state(&self) -> CircuitState {
        *self.state.lock().unwrap()
    }
}

/// Graceful degradation: fallback strategies for failures
pub trait FallbackStrategy: Send + Sync {
    /// Execute fallback operation
    fn fallback(&self) -> Option<String>;
}

/// Simple string-based fallback
pub struct SimpleFallback(pub String);

impl FallbackStrategy for SimpleFallback {
    fn fallback(&self) -> Option<String> {
        Some(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU32;

    #[tokio::test]
    async fn test_retry_success() {
        let attempt = Arc::new(AtomicU32::new(0));
        let config = RetryConfig::conservative();

        let attempt_clone = attempt.clone();
        let result = retry_with_backoff(&config, || {
            let attempt_clone = attempt_clone.clone();
            Box::pin(async move {
                let curr = attempt_clone.fetch_add(1, Ordering::SeqCst) + 1;
                if curr < 3 {
                    Err("temporary error")
                } else {
                    Ok("success")
                }
            })
        })
        .await;

        assert_eq!(result, Ok("success"));
        assert_eq!(attempt.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_max_attempts() {
        let config = RetryConfig {
            max_attempts: 2,
            ..Default::default()
        };

        let result = retry_with_backoff(&config, || {
            Box::pin(async { Err::<&str, _>("always fails") })
        })
        .await;

        assert!(result.is_err());
    }

    #[test]
    fn test_circuit_breaker_closed() {
        let cb = CircuitBreaker::new(3, 2, Duration::from_secs(1));

        // Should allow requests when closed
        assert!(cb.is_request_allowed());
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_circuit_breaker_opens() {
        let cb = CircuitBreaker::new(3, 2, Duration::from_secs(1));

        // Record failures
        for _ in 0..3 {
            cb.record_failure();
        }

        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.is_request_allowed());
    }

    #[tokio::test]
    async fn test_circuit_breaker_half_open() {
        let cb = CircuitBreaker::new(2, 2, Duration::from_millis(100));

        // Open circuit
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout and transition to half-open
        sleep(Duration::from_millis(150)).await;
        assert!(cb.is_request_allowed());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Record success to close the circuit
        cb.record_success();
        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
    }
}
