//! Per-vault slots for the lazily built search and similarity engines.
//!
//! An engine is built off the request path's locks the first time a vault is
//! searched, which takes seconds on a large vault. Two things can go wrong in
//! that window, and the slots exist to rule both out:
//!
//! - **Two first callers build twice.** Each vault has a build lock, so the
//!   second caller waits for the first build and gets its engine.
//! - **A change lands mid-build and is lost.** The build reads each note once.
//!   A note that changes after it was read, but before the engine is
//!   published, used to find no engine to report to, and the sweep had
//!   already recorded it as seen, so the index kept the old text until the
//!   note's next edit. A change reported while a build runs is now kept, and
//!   handed to the builder when it publishes.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

/// What a change report was: `(vault-relative path, present)`.
pub(super) type Changed = Vec<(String, bool)>;

enum Slot<E> {
    /// A cold build is running; changes reported meanwhile are kept for it.
    Building {
        changed: Changed,
    },
    Ready(Arc<E>),
}

pub(super) struct EngineSlots<E> {
    slots: Mutex<HashMap<String, Slot<E>>>,
    builds: tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl<E> Default for EngineSlots<E> {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            builds: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl<E> EngineSlots<E> {
    /// The vault's engine, if one is built.
    pub(super) fn ready(&self, vault: &str) -> Option<Arc<E>> {
        match self.lock().get(vault) {
            Some(Slot::Ready(engine)) => Some(Arc::clone(engine)),
            _ => None,
        }
    }

    /// The vault's engine, building it with `build` if there is none yet.
    ///
    /// Alongside the engine come the changes reported while this call was
    /// building it, which the engine has not seen; empty when the engine was
    /// already built. A build that fails publishes nothing.
    pub(super) async fn get_or_build<F, Fut, Err>(
        &self,
        vault: &str,
        build: F,
    ) -> Result<(Arc<E>, Changed), Err>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<E, Err>>,
    {
        if let Some(engine) = self.ready(vault) {
            return Ok((engine, Vec::new()));
        }
        let build_lock = {
            let mut builds = self.builds.lock().await;
            Arc::clone(builds.entry(vault.to_string()).or_default())
        };
        let _building = build_lock.lock().await;
        // Whoever held the lock before us may have built it.
        if let Some(engine) = self.ready(vault) {
            return Ok((engine, Vec::new()));
        }

        self.lock().insert(
            vault.to_string(),
            Slot::Building {
                changed: Vec::new(),
            },
        );
        let engine = match build().await {
            Ok(engine) => Arc::new(engine),
            Err(e) => {
                self.end_build(vault);
                return Err(e);
            }
        };

        let mut slots = self.lock();
        // Removed while building (the vault was removed, or the engine
        // evicted): hand this caller its engine but publish nothing.
        let Some(Slot::Building { changed }) = slots.remove(vault) else {
            return Ok((engine, Vec::new()));
        };
        slots.insert(vault.to_string(), Slot::Ready(Arc::clone(&engine)));
        Ok((engine, changed))
    }

    /// Route a change report: the built engine to apply it to, if there is
    /// one. While a build runs the report is kept for it instead.
    pub(super) fn report(&self, vault: &str, changed: &[(String, bool)]) -> Option<Arc<E>> {
        match self.lock().get_mut(vault) {
            Some(Slot::Ready(engine)) => Some(Arc::clone(engine)),
            Some(Slot::Building { changed: kept }) => {
                kept.extend_from_slice(changed);
                None
            }
            None => None,
        }
    }

    /// Drop the vault's engine, or the result of a build in progress.
    pub(super) fn remove(&self, vault: &str) {
        self.lock().remove(vault);
    }

    fn end_build(&self, vault: &str) {
        let mut slots = self.lock();
        if matches!(slots.get(vault), Some(Slot::Building { .. })) {
            slots.remove(vault);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Slot<E>>> {
        // A panic while the map was held can't leave a slot half-written:
        // every mutation is a single insert or remove.
        self.slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::Notify;

    #[tokio::test]
    async fn concurrent_first_callers_build_once() {
        let slots = Arc::new(EngineSlots::<usize>::default());
        let builds = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(Notify::new());

        let first = {
            let (slots, builds, release) = (
                Arc::clone(&slots),
                Arc::clone(&builds),
                Arc::clone(&release),
            );
            tokio::spawn(async move {
                slots
                    .get_or_build("v", || async move {
                        builds.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Ok::<_, ()>(7)
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        let second = {
            let (slots, builds) = (Arc::clone(&slots), Arc::clone(&builds));
            tokio::spawn(async move {
                slots
                    .get_or_build("v", || async move {
                        builds.fetch_add(1, Ordering::SeqCst);
                        Ok::<_, ()>(8)
                    })
                    .await
            })
        };
        tokio::task::yield_now().await;
        release.notify_one();

        let (first, _) = first.await.unwrap().unwrap();
        let (second, _) = second.await.unwrap().unwrap();
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn a_change_reported_mid_build_is_handed_to_the_builder() {
        let slots = Arc::new(EngineSlots::<usize>::default());
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());

        let build = {
            let (slots, started, release) = (
                Arc::clone(&slots),
                Arc::clone(&started),
                Arc::clone(&release),
            );
            tokio::spawn(async move {
                slots
                    .get_or_build("v", || async move {
                        started.notify_one();
                        release.notified().await;
                        Ok::<_, ()>(1)
                    })
                    .await
            })
        };
        started.notified().await;
        assert!(slots.report("v", &[("a.md".to_string(), true)]).is_none());
        release.notify_one();

        let (_, changed) = build.await.unwrap().unwrap();
        assert_eq!(changed, [("a.md".to_string(), true)]);
        // Published: the next report goes straight to the engine.
        assert!(slots.report("v", &[("b.md".to_string(), true)]).is_some());
    }

    #[tokio::test]
    async fn a_build_removed_midway_is_not_published() {
        let slots = Arc::new(EngineSlots::<usize>::default());
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());

        let build = {
            let (slots, started, release) = (
                Arc::clone(&slots),
                Arc::clone(&started),
                Arc::clone(&release),
            );
            tokio::spawn(async move {
                slots
                    .get_or_build("v", || async move {
                        started.notify_one();
                        release.notified().await;
                        Ok::<_, ()>(1)
                    })
                    .await
            })
        };
        started.notified().await;
        slots.remove("v");
        release.notify_one();

        assert!(build.await.unwrap().is_ok());
        assert!(slots.ready("v").is_none());
    }

    #[tokio::test]
    async fn a_failed_build_publishes_nothing() {
        let slots = EngineSlots::<usize>::default();
        assert!(slots.get_or_build("v", || async { Err(()) }).await.is_err());
        assert!(slots.ready("v").is_none());
        assert!(slots.report("v", &[("a.md".to_string(), true)]).is_none());
    }
}
