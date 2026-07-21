use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// Provenance supplied by a writer for best-effort event correlation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteProvenance {
    /// Stable caller-selected source, such as `daily-review-agent`.
    pub source: String,
    /// Optional identifier connecting related operations.
    pub correlation_id: Option<String>,
    /// Optional human-readable reason.
    pub note: Option<String>,
}

/// Best-effort event attribution.
///
/// This is advisory loop-prevention metadata, never an authorization or
/// security boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "provenance")]
pub enum EventAttribution {
    /// The host correlated the observed content with a known write.
    Attributed(WriteProvenance),
    /// No trusted correlation was available; the change may be external.
    ExternalOrUnknown,
}

/// A vault mutation observed by a hook producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HookEvent {
    /// A note was created.
    FileCreated {
        /// Vault-relative path.
        path: String,
    },
    /// A note was modified.
    FileModified {
        /// Vault-relative path.
        path: String,
    },
    /// A note was deleted.
    FileDeleted {
        /// Vault-relative path.
        path: String,
    },
    /// A note moved between two paths.
    FileRenamed {
        /// Original vault-relative path.
        from: String,
        /// New vault-relative path.
        to: String,
    },
    /// The authoritative state must be re-read after a producer reset.
    ResyncRequired {
        /// Human-readable reason for the reset.
        reason: String,
    },
}

/// Sequenced event delivered to hook subscribers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultEventEnvelope {
    /// Process-local monotonic sequence number.
    pub sequence: u64,
    /// Observation time as Unix epoch milliseconds.
    pub observed_at_ms: u64,
    /// Vault name.
    pub vault: String,
    /// Observed mutation.
    pub event: HookEvent,
    /// Content identity when it was available at observation time.
    pub content_hash: Option<String>,
    /// Best-effort writer attribution.
    pub attribution: EventAttribution,
}

/// Current event-bus lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookLifecycle {
    /// New events may be published.
    Running,
    /// No further events will be accepted.
    Closed,
}

/// Publish failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    /// The bus was explicitly closed.
    #[error("hook bus is closed")]
    Closed,
}

/// Receive failure with explicit overflow semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HookRecvError {
    /// No event is currently available to a non-blocking receiver.
    #[error("no hook event is currently available")]
    Empty,
    /// This subscriber fell behind the bounded ring buffer.
    ///
    /// The consumer must re-read authoritative vault state before continuing.
    #[error("hook subscriber lagged by {skipped} event(s); resync required")]
    Lagged {
        /// Number of discarded envelopes.
        skipped: u64,
    },
    /// The bus closed and all buffered events were drained.
    #[error("hook bus is closed")]
    Closed,
}

struct HookBusInner {
    sender: Mutex<Option<broadcast::Sender<VaultEventEnvelope>>>,
    sequence: AtomicU64,
    closed: AtomicBool,
    capacity: usize,
}

/// Cloneable bounded event bus shared by a host and its plugins.
///
/// Delivery is best-effort. A slow subscriber receives
/// [`HookRecvError::Lagged`] and must resynchronize from [`crate::VaultApi`].
#[derive(Clone)]
pub struct HookBus {
    inner: Arc<HookBusInner>,
}

impl std::fmt::Debug for HookBus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookBus")
            .field("capacity", &self.capacity())
            .field("lifecycle", &self.lifecycle())
            .finish()
    }
}

impl HookBus {
    fn sender(&self) -> std::sync::MutexGuard<'_, Option<broadcast::Sender<VaultEventEnvelope>>> {
        self.inner
            .sender
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Create a bus with a fixed per-subscriber ring-buffer capacity.
    ///
    /// A zero capacity is promoted to one.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let (sender, _) = broadcast::channel(capacity);
        Self {
            inner: Arc::new(HookBusInner {
                sender: Mutex::new(Some(sender)),
                sequence: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                capacity,
            }),
        }
    }

    /// Return the fixed ring-buffer capacity.
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Return whether new events are accepted.
    pub fn lifecycle(&self) -> HookLifecycle {
        if self.inner.closed.load(Ordering::Acquire) {
            HookLifecycle::Closed
        } else {
            HookLifecycle::Running
        }
    }

    /// Subscribe to future events.
    pub fn subscribe(&self) -> Result<HookSubscription, PublishError> {
        let guard = self.sender();
        let sender = guard.as_ref().ok_or(PublishError::Closed)?;
        Ok(HookSubscription {
            receiver: sender.subscribe(),
        })
    }

    /// Sequence and publish an advisory event.
    ///
    /// Success does not imply that a subscriber exists.
    pub fn publish(
        &self,
        vault: impl Into<String>,
        event: HookEvent,
        content_hash: Option<String>,
        attribution: EventAttribution,
    ) -> Result<VaultEventEnvelope, PublishError> {
        let guard = self.sender();
        let sender = guard.as_ref().ok_or(PublishError::Closed)?;
        let envelope = VaultEventEnvelope {
            sequence: self.inner.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            observed_at_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            vault: vault.into(),
            event,
            content_hash,
            attribution,
        };
        let _subscriber_count = sender.send(envelope.clone());
        Ok(envelope)
    }

    /// Stop accepting events and close subscribers after buffered delivery.
    ///
    /// Returns `true` only for the call that changed the lifecycle.
    pub fn close(&self) -> bool {
        let mut guard = self.sender();
        let changed = guard.take().is_some();
        self.inner.closed.store(true, Ordering::Release);
        changed
    }
}

/// A single bounded subscription to [`HookBus`].
pub struct HookSubscription {
    receiver: broadcast::Receiver<VaultEventEnvelope>,
}

impl std::fmt::Debug for HookSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookSubscription")
            .finish_non_exhaustive()
    }
}

impl HookSubscription {
    /// Wait for the next event or an explicit lag/close condition.
    pub async fn recv(&mut self) -> Result<VaultEventEnvelope, HookRecvError> {
        self.receiver.recv().await.map_err(|error| match error {
            broadcast::error::RecvError::Closed => HookRecvError::Closed,
            broadcast::error::RecvError::Lagged(skipped) => HookRecvError::Lagged { skipped },
        })
    }

    /// Receive immediately without waiting.
    pub fn try_recv(&mut self) -> Result<VaultEventEnvelope, HookRecvError> {
        self.receiver.try_recv().map_err(|error| match error {
            broadcast::error::TryRecvError::Closed => HookRecvError::Closed,
            broadcast::error::TryRecvError::Lagged(skipped) => HookRecvError::Lagged { skipped },
            broadcast::error::TryRecvError::Empty => HookRecvError::Empty,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn publish(bus: &HookBus, path: &str) -> VaultEventEnvelope {
        bus.publish(
            "test",
            HookEvent::FileModified {
                path: path.to_string(),
            },
            None,
            EventAttribution::ExternalOrUnknown,
        )
        .expect("publish")
    }

    #[tokio::test]
    async fn sequences_events_and_closes_after_buffered_delivery() {
        // Capacity two retains both events until this subscriber reads them.
        const CAPACITY: usize = 2;
        let bus = HookBus::new(CAPACITY);
        let mut subscription = bus.subscribe().expect("subscribe");
        assert_eq!(publish(&bus, "a.md").sequence, 1);
        assert_eq!(publish(&bus, "b.md").sequence, 2);
        assert!(bus.close());
        assert!(!bus.close());

        assert_eq!(subscription.recv().await.expect("first").sequence, 1);
        assert_eq!(subscription.recv().await.expect("second").sequence, 2);
        assert_eq!(subscription.recv().await, Err(HookRecvError::Closed));
        assert_eq!(bus.lifecycle(), HookLifecycle::Closed);
    }

    #[tokio::test]
    async fn reports_lag_and_requires_resync() {
        // Capacity one intentionally evicts the first of two unread events.
        const CAPACITY: usize = 1;
        let bus = HookBus::new(CAPACITY);
        let mut subscription = bus.subscribe().expect("subscribe");
        publish(&bus, "a.md");
        publish(&bus, "b.md");

        assert_eq!(
            subscription.recv().await,
            Err(HookRecvError::Lagged { skipped: 1 })
        );
        assert_eq!(subscription.recv().await.expect("retained").sequence, 2);
    }

    #[test]
    fn rejects_new_work_after_close() {
        let bus = HookBus::new(0);
        assert_eq!(bus.capacity(), 1);
        assert!(bus.close());
        assert!(matches!(bus.subscribe(), Err(PublishError::Closed)));
        assert_eq!(
            bus.publish(
                "test",
                HookEvent::ResyncRequired {
                    reason: "closed".to_string()
                },
                None,
                EventAttribution::ExternalOrUnknown
            ),
            Err(PublishError::Closed)
        );
    }
}
