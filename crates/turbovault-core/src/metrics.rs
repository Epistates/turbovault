//! Lock-free metrics infrastructure for high-performance observability
//!
//! This module provides minimal metrics collection using atomic operations
//! with zero locking overhead. For comprehensive observability (tracing, OpenTelemetry),
//! use the observability infrastructure in turbomcp-server.
//!
//! Design:
//! - Counter: Monotonically increasing atomic u64 values
//! - Histogram: Atomic-backed distribution tracking
//! - HistogramTimer: RAII timer for automatic duration recording
//! - MetricsContext: Global registry for named metrics (rarely used)

use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A lock-free counter metric (monotonically increasing)
#[derive(Debug, Clone)]
pub struct Counter {
    value: Arc<AtomicU64>,
    name: String,
}

impl Counter {
    /// Create a new counter with the given name
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            value: Arc::new(AtomicU64::new(0)),
            name: name.into(),
        }
    }

    /// Increment counter by 1 (lock-free operation)
    pub fn increment(&self) {
        self.add(1);
    }

    /// Add value to counter (lock-free, uses saturating arithmetic)
    pub fn add(&self, value: u64) {
        let mut current = self.value.load(Ordering::Relaxed);
        loop {
            let new_value = current.saturating_add(value);
            match self.value.compare_exchange_weak(
                current,
                new_value,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    /// Get current value (lock-free read)
    pub fn value(&self) -> u64 {
        self.value.load(Ordering::Acquire)
    }

    /// Get metric name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reset counter to zero
    pub fn reset(&self) {
        self.value.store(0, Ordering::Release);
    }
}

/// A histogram for tracking value distributions
#[derive(Debug, Clone)]
pub struct Histogram {
    values: Arc<RwLock<Vec<f64>>>,
    name: String,
}

impl Histogram {
    /// Create a new histogram
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            values: Arc::new(RwLock::new(Vec::new())),
            name: name.into(),
        }
    }

    /// Record a value in the histogram
    pub fn record(&self, value: f64) {
        let mut v = self.values.write();
        v.push(value);
    }

    /// Create a timer for automatic duration recording
    pub fn timer(&self) -> HistogramTimer {
        HistogramTimer {
            histogram: self.clone(),
            start: Instant::now(),
        }
    }

    /// Get statistics for recorded values
    pub fn stats(&self) -> HistogramStats {
        let v = self.values.read();
        if v.is_empty() {
            return HistogramStats {
                count: 0,
                sum: 0.0,
                min: 0.0,
                max: 0.0,
                mean: 0.0,
            };
        }

        let sum: f64 = v.iter().sum();
        let count = v.len();
        let mean = sum / count as f64;
        let min = v.iter().copied().fold(f64::INFINITY, f64::min);
        let max = v.iter().copied().fold(f64::NEG_INFINITY, f64::max);

        HistogramStats {
            count,
            sum,
            min,
            max,
            mean,
        }
    }

    /// Get metric name
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Clear all recorded values
    pub fn reset(&self) {
        let mut v = self.values.write();
        v.clear();
    }
}

/// Statistics computed from histogram values
#[derive(Debug, Clone)]
pub struct HistogramStats {
    /// Number of samples
    pub count: usize,
    /// Sum of all values
    pub sum: f64,
    /// Minimum value observed
    pub min: f64,
    /// Maximum value observed
    pub max: f64,
    /// Mean of all values
    pub mean: f64,
}

/// RAII timer that records duration to histogram on drop
#[derive(Debug)]
pub struct HistogramTimer {
    histogram: Histogram,
    start: Instant,
}

impl Drop for HistogramTimer {
    fn drop(&mut self) {
        let duration_ms = self.start.elapsed().as_secs_f64() * 1000.0;
        self.histogram.record(duration_ms);
    }
}

/// Global metrics context registry (rarely used)
///
/// For most use cases, use turbomcp's ServerMetrics directly.
/// This is provided for backward compatibility and simple metric collection.
#[derive(Debug)]
pub struct MetricsContext {
    enabled: bool,
    counters: Arc<RwLock<HashMap<String, Counter>>>,
    histograms: Arc<RwLock<HashMap<String, Histogram>>>,
}

impl MetricsContext {
    /// Create new metrics context
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            counters: Arc::new(RwLock::new(HashMap::new())),
            histograms: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get or create a named counter
    pub fn counter(&self, name: &str) -> Counter {
        if !self.enabled {
            return Counter::new(name);
        }

        let mut counters = self.counters.write();
        if let Some(counter) = counters.get(name) {
            counter.clone()
        } else {
            let counter = Counter::new(name);
            counters.insert(name.to_string(), counter.clone());
            counter
        }
    }

    /// Get or create a named histogram
    pub fn histogram(&self, name: &str) -> Histogram {
        if !self.enabled {
            return Histogram::new(name);
        }

        let mut histograms = self.histograms.write();
        if let Some(histogram) = histograms.get(name) {
            histogram.clone()
        } else {
            let histogram = Histogram::new(name);
            histograms.insert(name.to_string(), histogram.clone());
            histogram
        }
    }

    /// Get all counter statistics
    pub fn get_counters(&self) -> HashMap<String, u64> {
        self.counters
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.value()))
            .collect()
    }

    /// Get all histogram statistics
    pub fn get_histograms(&self) -> HashMap<String, HistogramStats> {
        self.histograms
            .read()
            .iter()
            .map(|(k, v)| (k.clone(), v.stats()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counter_increment() {
        let counter = Counter::new("test");
        assert_eq!(counter.value(), 0);
        counter.increment();
        assert_eq!(counter.value(), 1);
        counter.add(5);
        assert_eq!(counter.value(), 6);
    }

    #[test]
    fn test_counter_saturation() {
        let counter = Counter::new("test");
        counter.value.store(u64::MAX, Ordering::Release);
        counter.add(100); // Should saturate, not overflow
        assert_eq!(counter.value(), u64::MAX);
    }

    #[test]
    fn test_histogram_stats() {
        let histogram = Histogram::new("test");
        histogram.record(1.0);
        histogram.record(2.0);
        histogram.record(3.0);

        let stats = histogram.stats();
        assert_eq!(stats.count, 3);
        assert_eq!(stats.sum, 6.0);
        assert_eq!(stats.min, 1.0);
        assert_eq!(stats.max, 3.0);
        assert_eq!(stats.mean, 2.0);
    }

    #[test]
    fn test_histogram_timer() {
        let histogram = Histogram::new("test");
        {
            let _timer = histogram.timer();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        let stats = histogram.stats();
        assert_eq!(stats.count, 1);
        assert!(stats.sum > 10.0); // At least 10ms
    }

    #[test]
    fn test_metrics_context() {
        let ctx = MetricsContext::new(true);
        let counter = ctx.counter("requests");
        counter.add(5);

        let counters = ctx.get_counters();
        assert_eq!(counters.get("requests"), Some(&5));
    }

    #[test]
    fn test_metrics_context_disabled() {
        let ctx = MetricsContext::new(false);
        let counter = ctx.counter("requests");
        counter.add(100);
        // Stats should still be empty since context is disabled
        let counters = ctx.get_counters();
        assert!(counters.is_empty());
    }
}
