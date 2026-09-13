use std::sync::Arc;

use tokio::sync::watch;

/// Cooperative shutdown signal handed to a plugin's background work.
///
/// A plugin that maintains derived state generally wants a worker: something
/// that drains the hook bus, re-embeds changed notes, compacts an index. That
/// work outlives any single tool call, so it needs a way to learn the host is
/// going away — otherwise a detached task keeps running against state that is
/// being torn down, and the process cannot exit cleanly.
///
/// Cheap to clone; every clone observes the same signal.
#[derive(Clone)]
pub struct ShutdownSignal {
    receiver: watch::Receiver<bool>,
}

impl std::fmt::Debug for ShutdownSignal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShutdownSignal")
            .field("is_shutting_down", &self.is_shutting_down())
            .finish()
    }
}

impl ShutdownSignal {
    /// Whether shutdown has already been requested.
    ///
    /// Check this at the top of a work loop; use [`Self::cancelled`] to wait.
    pub fn is_shutting_down(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Resolve when shutdown is requested, immediately if it already was.
    ///
    /// Select on this alongside the work a loop would otherwise block on:
    ///
    /// ```ignore
    /// loop {
    ///     tokio::select! {
    ///         _ = shutdown.cancelled() => break,
    ///         event = events.recv() => { /* ... */ }
    ///     }
    /// }
    /// ```
    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        // `borrow_and_update` marks the current value seen, so the wait below
        // reacts to the next change rather than an already-observed one.
        if *receiver.borrow_and_update() {
            return;
        }
        // A send error means the host dropped the trigger without signalling,
        // which can only happen if the host is already gone — treat it as
        // shutdown rather than waiting forever.
        let _ = receiver.changed().await;
    }
}

/// Host-side handle that requests shutdown.
///
/// Held by the server, never handed across the plugin boundary.
pub struct ShutdownTrigger {
    sender: Arc<watch::Sender<bool>>,
    /// Keeps the channel alive when no plugin holds a signal.
    ///
    /// Without a live receiver the state would be lost, and a shutdown
    /// requested before any worker started — an early failure during startup,
    /// say — would leave a worker that starts afterwards waiting forever for a
    /// signal that already fired.
    _keepalive: Arc<watch::Receiver<bool>>,
}

impl std::fmt::Debug for ShutdownTrigger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ShutdownTrigger")
            .finish_non_exhaustive()
    }
}

impl Default for ShutdownTrigger {
    fn default() -> Self {
        Self::new()
    }
}

impl ShutdownTrigger {
    /// Create a trigger that has not yet fired.
    pub fn new() -> Self {
        let (sender, keepalive) = watch::channel(false);
        Self {
            sender: Arc::new(sender),
            _keepalive: Arc::new(keepalive),
        }
    }

    /// Hand out a signal observing this trigger.
    pub fn signal(&self) -> ShutdownSignal {
        ShutdownSignal {
            receiver: self.sender.subscribe(),
        }
    }

    /// Request shutdown. Idempotent.
    pub fn shutdown(&self) {
        // `send_replace` rather than `send`: the latter reports an error when
        // no receiver exists, and this state must be recorded whether or not
        // anyone is listening yet.
        self.sender.send_replace(true);
    }
}

impl Clone for ShutdownTrigger {
    fn clone(&self) -> Self {
        Self {
            sender: Arc::clone(&self.sender),
            _keepalive: Arc::clone(&self._keepalive),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn waiting_workers_are_released_on_shutdown() {
        let trigger = ShutdownTrigger::new();
        let signal = trigger.signal();
        assert!(!signal.is_shutting_down());

        let waiter = tokio::spawn({
            let signal = signal.clone();
            async move {
                signal.cancelled().await;
                true
            }
        });

        trigger.shutdown();
        assert!(waiter.await.expect("waiter task"));
        assert!(signal.is_shutting_down());
    }

    #[tokio::test]
    async fn a_signal_taken_after_shutdown_resolves_immediately() {
        let trigger = ShutdownTrigger::new();
        trigger.shutdown();
        let signal = trigger.signal();
        assert!(signal.is_shutting_down());
        // Must not hang: the worker started too late to see the transition.
        signal.cancelled().await;
    }
}
