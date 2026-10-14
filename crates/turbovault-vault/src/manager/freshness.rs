//! The freshness gate: draining the Git reindex queue and sweeping for
//! changes made outside TurboVault before any derived read.

use super::*;

impl VaultManager {
    /// M4c (design §6.6): drain the git reindex queue — apply every pending
    /// commit's diff to the link graph + note cache, then fire the
    /// change-listener with the collapsed `(path, present)` set. Holds the
    /// queue's flush lock across the whole pass so it can't interleave with a
    /// concurrent flush (turbovault-9zr). A no-op on Direct or an empty queue.
    ///
    /// This is what makes the manager's derived reads self-flushing and closes
    /// the M4b search-staleness gap for out-of-band commits (the ref-listener
    /// enqueues; this applies).
    pub async fn flush_reindex(&self) {
        let Some(queue) = self.reindex_queue.clone() else {
            return; // Direct backend
        };
        let _flush_guard = queue.lock_flush().await;
        if queue.pending_count() == 0 {
            return;
        }

        // Open the repo + collect per-commit diffs inside spawn_blocking:
        // `VaultRepo` is `!Sync`, so its libgit2 handle must never cross an
        // await (mirrors the server's `drain_pending_diffs`). The graph apply
        // runs back here, async, via `sync_index` — the SAME applier the
        // manager's own mutators use.
        let path = self.vault_path.clone();
        let drain_queue = Arc::clone(&queue);
        let drained = tokio::task::spawn_blocking(move || -> Vec<DrainedCommit> {
            let Ok(repo) = VaultRepo::open(&path) else {
                return Vec::new();
            };
            let mut batches = Vec::new();
            while let Some(pending) = drain_queue.pop_front() {
                let commit = pending.oid;
                // A commit the ref-watcher enqueued may no longer be reachable
                // (GC / non-ff move). A first-parent or diff failure SKIPS that
                // commit — advance the cursor, keep draining — rather than
                // bricking the whole pass (unify with drain_through's tlx.1).
                let parent = match repo.git_commit_first_parent(commit) {
                    Ok(p) => p,
                    Err(e) => {
                        tracing::warn!(
                            "flush_reindex: skipping commit {commit} after first-parent error: {e}"
                        );
                        drain_queue.advance_cursor(commit);
                        continue;
                    }
                };
                match repo.diff_path_statuses(parent, commit) {
                    Ok(changes) => batches.push((commit, pending.origin, changes)),
                    Err(e) => {
                        tracing::warn!(
                            "flush_reindex: skipping commit {commit} after diff error: {e}"
                        );
                        drain_queue.advance_cursor(commit);
                    }
                }
            }
            batches
        })
        .await
        .unwrap_or_default();

        if drained.is_empty() {
            return;
        }

        let mut collapsed: Vec<(String, bool, CommitOrigin)> = Vec::new();
        for (commit, origin, changes) in drained {
            self.sync_index(&changes).await;
            collapsed.extend(with_origin(changes, origin));
            queue.advance_cursor(commit);
        }
        // Fired while the flush guard is still held, deliberately. The guard is
        // what orders passes against each other; releasing it first would let a
        // later pass's fan-out overtake an earlier one's and leave the search
        // index holding an older revision of a path than the link graph does.
        self.fire_change_listener(collapsed).await;
    }

    /// Bring every derived view this manager owns into agreement with what is
    /// on disk right now, then return.
    ///
    /// This is the freshness gate, and it is the single thing a caller has to
    /// remember. Read paths that consult derived state (the link graph, the
    /// note cache, and — through the change-listener — the server's search and
    /// similarity indexes and any plugin's own index) call it first, so that
    /// every one of them answers from the same observation of the vault.
    ///
    /// # Why a gate and not a watcher
    ///
    /// A vault is a shared directory. Obsidian is usually open on it, an editor
    /// or a `git pull` or a sync client may touch it, and none of that goes
    /// through this process. Filesystem notifications look like the obvious
    /// answer and are not a correctness mechanism anywhere: inotify queues
    /// overflow and its watch budget is finite, FSEvents degrades to
    /// directory-granularity rescan hints under load, and network and
    /// cloud-synced vaults (iCloud, Dropbox, Syncthing — all ordinary for
    /// Obsidian) deliver nothing at all for a peer's changes. A watcher yields
    /// *mostly* fresh, and for an agent that is worse than plainly stale,
    /// because nothing marks the answers it should not have trusted.
    ///
    /// Comparing state cannot miss an event, because it is not listening for
    /// one. The comparison is a scan of `(size, mtime)` against what the note
    /// cache recorded when it parsed each note, debounced so a burst of calls
    /// pays for one pass. What it costs is bounded by
    /// `RECONCILE_DUTY_DIVISOR`; how stale an answer can be is bounded by
    /// `ReconcileState::interval`.
    ///
    /// The two halves run in this order on purpose. Draining first settles any
    /// commit this process has already been told about and records the
    /// fingerprints that go with it, so the sweep that follows sees those notes
    /// as settled instead of reporting them a second time.
    pub async fn ensure_fresh(&self) {
        self.flush_reindex().await;
        self.reconcile_external_changes().await;
    }

    /// [`Self::ensure_fresh`] without the debounce: a full pass now, unless
    /// the vault opted out of reconciling.
    ///
    /// For an operation that rewrites notes according to what links to what,
    /// a link-aware move or delete. A debounced answer can be up to
    /// `ReconcileState::interval` old, and a link added in Obsidian within
    /// that window would be left pointing at a note that is no longer there.
    /// Reads keep using [`Self::ensure_fresh`]: they cost one pass per burst,
    /// and an answer a moment old is not one that breaks anything.
    pub async fn ensure_fresh_now(&self) {
        self.flush_reindex().await;
        if !self.config.reconcile_external_changes {
            return;
        }
        let mut state = self.reconcile.lock().await;
        self.reconcile_holding(&mut state).await;
    }

    /// Reconcile right now, ignoring both the schedule and the configured
    /// opt-out.
    ///
    /// [`Self::ensure_fresh`] is what a read path should call: it is scheduled,
    /// so a burst of calls costs one pass and an idle server costs nothing. Use
    /// this when something out of band says the vault moved and waiting out the
    /// interval is not acceptable. Being explicit is also why it overrides
    /// `reconcile_external_changes`: someone asking for this has said what they
    /// want more recently than the config did.
    pub async fn reconcile_now(&self) {
        self.flush_reindex().await;
        let mut state = self.reconcile.lock().await;
        self.reconcile_holding(&mut state).await;
    }

    /// The sweep half of [`Self::ensure_fresh`], with its schedule.
    pub(super) async fn reconcile_external_changes(&self) {
        if !self.config.reconcile_external_changes {
            return;
        }
        // Taking the lock before testing the schedule is what collapses
        // concurrent gate calls onto one pass: the losers block here, and by
        // the time they hold the lock the winner has recorded a fresh
        // timestamp, so they find the sweep no longer due and return having
        // paid only the wait. Testing first and then locking would let them all
        // through.
        let mut state = self.reconcile.lock().await;
        if !state.is_due() {
            return;
        }
        self.reconcile_holding(&mut state).await;
    }

    /// One reconcile pass. The caller holds the reconcile lock and has already
    /// decided the pass should happen.
    ///
    /// The fan-out runs under that lock too, for the reason the drain pass keeps
    /// its flush guard: it orders one pass's fan-out against the next, so the
    /// search index can never be left holding an older revision of a path than
    /// the link graph.
    pub(super) async fn reconcile_holding(&self, state: &mut ReconcileState) {
        let started = Instant::now();
        let changed = self.sweep_for_external_changes().await;
        state.record(started.elapsed());
        self.fire_change_listener(with_origin(changed, CommitOrigin::External))
            .await;
    }

    /// Compare a fresh scan against what the note cache believes and bring the
    /// cache and link graph into line. Returns the vault-relative
    /// `(path, present)` set that moved, in the shape the change listener and
    /// the search index already consume.
    pub(super) async fn sweep_for_external_changes(&self) -> Vec<(String, bool)> {
        let spec = Arc::clone(&self.scan_spec);
        let scanned = match tokio::task::spawn_blocking(move || spec.walk()).await {
            Ok(Ok(scanned)) => scanned,
            // A scan that fails leaves derived state exactly as it was. Acting
            // on a partial walk would report every note it did not reach as
            // deleted and tear the link graph down.
            Ok(Err(e)) => {
                tracing::warn!("freshness sweep: vault scan failed, derived state unchanged: {e}");
                return Vec::new();
            }
            Err(e) => {
                tracing::warn!("freshness sweep: scan task failed, derived state unchanged: {e}");
                return Vec::new();
            }
        };

        // Notes only, on both sides of the diff. The scan admits more than the
        // cache models, and comparing across that gap would report every
        // non-note as newly created on every pass.
        let mut on_disk: HashMap<PathBuf, FileFingerprint> = HashMap::with_capacity(scanned.len());
        for note in scanned {
            if !is_note(&note.path) {
                continue;
            }
            let fingerprint = note.fingerprint();
            on_disk.insert(note.path, fingerprint);
        }

        let mut changed: Vec<(PathBuf, bool)> = Vec::new();
        // Matching fingerprints that can't vouch for the bytes on their own.
        let mut racy: Vec<(PathBuf, FileFingerprint, String)> = Vec::new();
        {
            let cache = self.file_cache.read().await;
            for (path, entry) in cache.iter() {
                match (on_disk.remove(path), entry.observed) {
                    // Same revision we parsed, read too close to its mtime
                    // for the fingerprint alone to say so.
                    (Some(found), Some(observed)) if observed.fingerprint == found => {
                        if observed.racy {
                            racy.push((path.clone(), found, entry.file.metadata.checksum.clone()));
                        }
                    }
                    // Still there, but not the bytes we parsed.
                    (Some(_), _) => changed.push((path.clone(), true)),
                    // Gone from the vault.
                    (None, _) => changed.push((path.clone(), false)),
                }
            }
        }
        for (path, found, checksum) in racy {
            if let Some(edited) = self.recheck_racy(&path, found, &checksum).await {
                changed.push((path, edited));
            }
        }
        // Whatever the scan found that the cache had never heard of was created
        // underneath us. This is the case a per-entry mtime check structurally
        // cannot see, because it can only re-check entries it already holds.
        changed.extend(on_disk.into_keys().map(|path| (path, true)));

        let changed = self.without_claimed_paths(
            changed
                .into_iter()
                .filter_map(|(path, present)| Some((self.relative_key(&path)?, present)))
                .collect(),
        );
        if changed.is_empty() {
            return Vec::new();
        }
        tracing::debug!(
            "freshness sweep: {} path(s) changed outside this process",
            changed.len()
        );
        self.sync_index(&changed).await;
        changed
    }

    /// Check a racy note (see [`Observed::racy`]) by content. `Some(present)`
    /// when it changed and belongs in the sweep's diff. When the bytes are
    /// the ones we parsed, the entry takes this read's observation instead,
    /// which stops being racy once the read is past the note's mtime tick.
    pub(super) async fn recheck_racy(
        &self,
        path: &Path,
        found: FileFingerprint,
        checksum: &str,
    ) -> Option<bool> {
        match read_note_with_fingerprint(path, Some(found)).await {
            Ok(read) if turbovault_core::compute_hash(&read.content) == checksum => {
                let mut cache = self.file_cache.write().await;
                // Unless a write recorded a newer revision meanwhile.
                if let Some(entry) = cache.get_mut(path)
                    && entry.observed.map(|o| o.fingerprint) == Some(found)
                {
                    entry.observed = read.observed;
                }
                None
            }
            Ok(_) => Some(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
            // Left racy, so the next pass looks again.
            Err(e) => {
                tracing::debug!("freshness sweep: re-reading {}: {e}", path.display());
                None
            }
        }
    }

    /// Vault-relative, `/`-separated — the spelling a git diff produces, and so
    /// the one the search index is already keyed by. Building it from path
    /// components rather than replacing separators keeps a sweep-reported path
    /// and a commit-reported path for the same note identical on every
    /// platform, without mangling a Unix filename that legitimately contains a
    /// backslash.
    pub(super) fn relative_key(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.vault_path).ok()?;
        let mut key = String::with_capacity(relative.as_os_str().len());
        for component in relative.components() {
            let std::path::Component::Normal(part) = component else {
                return None;
            };
            if !key.is_empty() {
                key.push('/');
            }
            key.push_str(&part.to_string_lossy());
        }
        (!key.is_empty()).then_some(key)
    }

    /// Drop the paths this process is actively writing. See [`WriteClaim`].
    pub(super) fn without_claimed_paths(
        &self,
        changed: Vec<(String, bool)>,
    ) -> Vec<(String, bool)> {
        let claims = self.write_claims.lock().unwrap_or_else(|e| e.into_inner());
        if claims.is_empty() {
            return changed;
        }
        changed
            .into_iter()
            .filter(|(path, _)| !claims.contains_key(path))
            .collect()
    }
}
