//! The manager's bookkeeping: what each cached note was last seen as, the
//! freshness sweep's schedule, and the paths a write has claimed.

use super::*;

/// What the manager last observed about a note on disk.
///
/// [`VaultManager::ensure_fresh`] compares this against a fresh scan to work out
/// what changed underneath the process. It is deliberately an *identity* test
/// ("is this the same revision we parsed?") rather than a *happens-before* test
/// ("was it touched after we looked?"). Happens-before needs the file's clock
/// and ours to agree, and they do not: a sync tool that restores the original
/// mtime, a checkout that rewinds it, or a machine whose clock steps all defeat
/// it, each time by declaring a changed file fresh.
///
/// On a platform that reports no modification time at all this degrades to a
/// size comparison. Every platform TurboVault targets reports one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FileFingerprint {
    pub(super) size_bytes: u64,
    pub(super) modified: Option<SystemTime>,
}

impl FileFingerprint {
    pub(super) async fn observe(path: &Path) -> Option<Self> {
        Self::observe_with_times(path)
            .await
            .map(|(fingerprint, _)| fingerprint)
    }

    /// The fingerprint, and the file's times from the same stat.
    pub(super) async fn observe_with_times(path: &Path) -> Option<(Self, FileTimes)> {
        let metadata = tokio::fs::metadata(path).await.ok()?;
        let fingerprint = Self {
            size_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        };
        Some((fingerprint, FileTimes::of(&metadata)))
    }
}

/// When a note was created and last modified, for its [`FileMetadata`].
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FileTimes {
    pub(super) created: Option<SystemTime>,
    pub(super) modified: Option<SystemTime>,
}

impl FileTimes {
    pub(super) fn of(metadata: &std::fs::Metadata) -> Self {
        let modified = metadata.modified().ok();
        Self {
            // Not every filesystem keeps a birth time; the last change is the
            // nearest thing it does keep.
            created: metadata.created().ok().or(modified),
            modified,
        }
    }

    /// Record these times on `file`, as the seconds since the Unix epoch
    /// [`FileMetadata`] keeps. The parser has the content but not the file,
    /// so the times are the caller's to fill in.
    pub(super) fn stamp(self, file: &mut VaultFile) {
        let seconds = |time: SystemTime| {
            time.duration_since(UNIX_EPOCH)
                .ok()
                .map(|since| since.as_secs_f64())
        };
        if let Some(created) = self.created.and_then(seconds) {
            file.metadata.created_at = created;
        }
        if let Some(modified) = self.modified.and_then(seconds) {
            file.metadata.modified_at = modified;
        }
    }
}

/// A note's fingerprint, and whether it is enough to tell a later edit apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Observed {
    pub(super) fingerprint: FileFingerprint,
    /// Read before its mtime tick was over. A filesystem keeps mtimes to some
    /// granularity (2 s on FAT and exFAT, 1 s on HFS+ and many network
    /// mounts), so an edit that keeps the size and lands in the same tick
    /// leaves the fingerprint as it was: a checkbox toggle, `[ ]` to `[x]`, is
    /// exactly that. The sweep checks a racy note by content instead, until a
    /// read after the tick clears it. Git does the same for the index entries
    /// it calls "racily clean".
    pub(super) racy: bool,
}

/// How long after its mtime a note's fingerprint is racy: the coarsest tick
/// in common use, plus slack for the clocks to disagree.
pub(super) const RACY_WINDOW: Duration = Duration::from_millis(2_100);

impl Observed {
    /// The observation of a read that finished at `read_at`.
    pub(super) fn at(fingerprint: FileFingerprint, read_at: SystemTime) -> Self {
        let racy = fingerprint.modified.is_some_and(|modified| {
            // An mtime ahead of our clock is racy until our clock passes it.
            read_at
                .duration_since(modified)
                .map_or(true, |age| age < RACY_WINDOW)
        });
        Self { fingerprint, racy }
    }
}

/// One note in the cache: what was parsed, and the revision it was parsed from.
#[derive(Debug, Clone)]
pub(super) struct CacheEntry {
    /// Shared, so handing the cache's notes to a reader copies pointers,
    /// not the vault.
    pub(super) file: Arc<VaultFile>,
    /// The fingerprint of the bytes `file` was parsed from, when it could be
    /// established. `None` means "unknown", which no scan result can equal, so
    /// the next sweep re-parses the note. Unknown resolves to "assume stale".
    pub(super) observed: Option<Observed>,
}

/// Never reconcile more often than this, however cheap the sweep turns out to
/// be. Agent tool calls arrive a model round-trip apart and human edits are
/// seconds apart, so a bound in this range is imperceptible while still
/// collapsing a burst of calls onto a single scan.
pub(super) const RECONCILE_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// ...and never less often than this, however expensive it turns out to be.
pub(super) const RECONCILE_MAX_INTERVAL: Duration = Duration::from_secs(30);

/// Reconciliation may consume at most `1/RECONCILE_DUTY_DIVISOR` of wall clock.
/// Scaling the interval to the measured cost is what lets one default serve a
/// 200-note vault and a 200k-note vault: the small vault sweeps at the floor,
/// the large one backs off on its own, and nobody has to tune a number per
/// vault to avoid either staleness or a stall.
///
/// Measured on an M-series Mac, release build, warm page cache, no changes to
/// apply: 0.8ms at 100 notes, 1.7ms at 1k, 19ms at 10k. So every vault up to
/// roughly 13k notes sits at the floor and the duty cycle never binds; past
/// that it stretches the interval rather than the pass.
pub(super) const RECONCILE_DUTY_DIVISOR: u32 = 20;

/// How many note reads `initialize` keeps in flight. Enough to hide per-file
/// latency on a slow or network disk, few enough to stay well inside the open
/// file limit.
pub(super) const INITIAL_READ_CONCURRENCY: usize = 64;

/// How many notes `initialize` reads before parsing them, so at most this
/// many notes' raw text is held at once.
pub(super) const INITIAL_PARSE_CHUNK: usize = 2048;

/// How many notes `sync_index` applies per hold of the graph and the cache,
/// so a reader waits on a slice of a large batch, never all of it.
pub(super) const SYNC_CHUNK: usize = 512;

/// What `sync_index` does to the index for one changed note.
pub(super) enum IndexUpdate {
    /// Parse result to put in the graph and the cache.
    Upsert(Arc<VaultFile>, Option<Observed>),
    /// Gone, or out of the scan's reach: drop it.
    Remove,
    /// Leave the index as it is.
    Skip,
}

/// The reconcile schedule.
#[derive(Debug)]
pub(super) struct ReconcileState {
    /// When the last completed sweep finished. `None` until the first one runs,
    /// so the first gated call after startup always sweeps.
    pub(super) last_finished: Option<Instant>,
    /// What that sweep cost, which sets how long until the next one is due.
    pub(super) last_cost: Duration,
}

impl ReconcileState {
    pub(super) fn new() -> Self {
        Self {
            last_finished: None,
            last_cost: Duration::ZERO,
        }
    }

    pub(super) fn interval(&self) -> Duration {
        (self.last_cost * RECONCILE_DUTY_DIVISOR)
            .clamp(RECONCILE_MIN_INTERVAL, RECONCILE_MAX_INTERVAL)
    }

    pub(super) fn is_due(&self) -> bool {
        match self.last_finished {
            None => true,
            Some(finished) => finished.elapsed() >= self.interval(),
        }
    }

    pub(super) fn record(&mut self, cost: Duration) {
        self.last_cost = cost;
        self.last_finished = Some(Instant::now());
    }
}

/// A live claim on the paths one write plan touches, released on drop.
///
/// Between the substrate landing bytes and `sync_index` recording the new
/// fingerprint there is a window in which a freshness sweep would see a change
/// this process is in the middle of making. Publishing it would attribute one
/// of TurboVault's own mutations to an external actor, which is a lie to
/// anything consuming the change feed for provenance, and would re-parse a file
/// the write path is about to re-parse anyway. Claimed paths are skipped; the
/// write that claimed them reports them.
pub(super) struct WriteClaim<'a> {
    pub(super) claims: &'a StdMutex<HashMap<String, usize>>,
    pub(super) paths: Vec<String>,
}

impl<'a> WriteClaim<'a> {
    /// Counted rather than a plain set: two plans may legitimately have
    /// overlapping paths in flight, and the first to finish must not release
    /// the second's claim.
    pub(super) fn new(claims: &'a StdMutex<HashMap<String, usize>>, paths: Vec<String>) -> Self {
        {
            let mut held = claims.lock().unwrap_or_else(|e| e.into_inner());
            for path in &paths {
                *held.entry(path.clone()).or_insert(0) += 1;
            }
        }
        Self { claims, paths }
    }
}

impl Drop for WriteClaim<'_> {
    fn drop(&mut self) {
        let mut held = self.claims.lock().unwrap_or_else(|e| e.into_inner());
        for path in &self.paths {
            match held.get_mut(path) {
                Some(count) if *count > 1 => *count -= 1,
                _ => {
                    held.remove(path);
                }
            }
        }
    }
}
