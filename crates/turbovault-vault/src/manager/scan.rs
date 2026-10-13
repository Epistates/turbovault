//! Reading the vault: the scan, parsing notes, building and syncing the
//! note cache and link graph, and the cached views of them.

use super::*;

/// Everything a vault scan needs, detached from the manager once at
/// construction so the walk can move to the blocking pool without borrowing it.
#[derive(Debug)]
pub(super) struct ScanSpec {
    pub(super) root: PathBuf,
    pub(super) excluded: HashSet<String>,
    /// Admitted extensions, lower-cased and *without* the leading dot, so the
    /// per-entry test is a hash lookup on the raw `Path::extension` slice in
    /// the overwhelmingly common already-lower-case case.
    pub(super) allowed_extensions: HashSet<String>,
    pub(super) max_file_size: u64,
}

impl ScanSpec {
    pub(super) fn new(config: &ServerConfig, root: PathBuf) -> Self {
        Self {
            root,
            excluded: config.excluded_paths.clone(),
            allowed_extensions: config
                .allowed_extensions
                .iter()
                .map(|ext| ext.trim_start_matches('.').to_lowercase())
                .collect(),
            max_file_size: config.max_file_size,
        }
    }

    /// Case-insensitive, matching `sync_index`'s markdown test.
    ///
    /// The two have to agree. A note the write path caches but the scan cannot
    /// find would be reported deleted by the very next sweep, so `Note.MD`
    /// would quietly fall out of the link graph after being written.
    pub(super) fn admits_extension(&self, ext: &str) -> bool {
        self.allowed_extensions.contains(ext)
            || (ext.bytes().any(|b| b.is_ascii_uppercase())
                && self.allowed_extensions.contains(&ext.to_lowercase()))
    }

    /// Whether `path` is a file [`Self::walk`] would report: the same test,
    /// for one path.
    ///
    /// The cache must hold exactly what a walk can find. A note it holds that
    /// the walk can't (one that grew past `max_file_size`, or was written
    /// through a symlinked folder) would be reported deleted by every sweep
    /// and never settle.
    pub(super) fn admits(&self, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut current = self.root.clone();
        let mut components = relative.components().peekable();
        while let Some(component) = components.next() {
            let std::path::Component::Normal(part) = component else {
                return false;
            };
            let Some(name) = part.to_str() else {
                return false;
            };
            if PROTECTED_COMPONENTS.contains(&name) || self.excluded.contains(name) {
                return false;
            }
            current.push(part);
            // Not followed, as in the walk.
            let Ok(metadata) = std::fs::symlink_metadata(&current) else {
                return false;
            };
            if components.peek().is_some() {
                if !metadata.is_dir() {
                    return false;
                }
                continue;
            }
            let admitted = current
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| self.admits_extension(ext));
            return metadata.is_file() && admitted && metadata.len() <= self.max_file_size;
        }
        false
    }

    /// Walk the vault and return every note the configuration admits, carrying
    /// the `(size, mtime)` the walk had to read anyway.
    ///
    /// Symlinks are never followed, as directories or as notes. A vault is a
    /// directory of files the operator owns, and a link inside it can point
    /// anywhere: at an ancestor, which makes the walk recurse until it exhausts
    /// memory, or outside the vault root, which would pull content into the
    /// index that `resolve_path` refuses to hand out. Skipping them keeps the
    /// walk terminating and keeps discovery inside the boundary the read and
    /// write paths already enforce.
    ///
    /// [`PROTECTED_COMPONENTS`] is skipped alongside the configured exclusions:
    /// `.turbovault/` is TurboVault's own audit trail and snapshot store, not
    /// vault content, and walking it is pure cost on every pass.
    pub(super) fn walk(&self) -> Result<Vec<ScannedNote>> {
        let mut notes = Vec::new();
        let mut stack = vec![self.root.clone()];

        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                // A subdirectory that vanished between being pushed and being
                // read is the vault changing underneath us, not a scan failure.
                // A missing vault root still is one.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && dir != self.root => continue,
                Err(e) => return Err(Error::io(e)),
            };

            for entry in entries.flatten() {
                let path = entry.path();
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if PROTECTED_COMPONENTS.contains(&name) || self.excluded.contains(name) {
                    continue;
                }

                // `file_type` reads `d_type` straight off the dirent where the
                // platform supplies it, so the test costs no extra syscall, and
                // it describes the link itself rather than its target.
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    continue; // symlinks, fifos, sockets, devices
                }

                let admitted = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| self.admits_extension(ext));
                if !admitted {
                    continue;
                }

                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                if metadata.len() > self.max_file_size {
                    continue;
                }
                notes.push(ScannedNote {
                    path,
                    size_bytes: metadata.len(),
                    modified: metadata.modified().ok(),
                });
            }
        }

        Ok(notes)
    }
}

/// Read a note and fingerprint the exact revision that was read.
///
/// The fingerprint is taken before and after the read and kept only if it did
/// not move. A write landing inside that window would otherwise be recorded as
/// "already seen", and because the sweep compares against precisely this value
/// the note would then stay stale forever rather than for one interval. When
/// the two disagree the content still stands (it is a coherent snapshot of one
/// of the two revisions) but the fingerprint is dropped, which makes the next
/// sweep look again.
///
/// `expected` is a fingerprint the caller already holds, from the scan that
/// found the file, and passing it saves the leading stat.
pub(super) async fn read_note_with_fingerprint(
    path: &Path,
    expected: Option<FileFingerprint>,
) -> std::io::Result<NoteRead> {
    let before = match expected {
        Some(fingerprint) => Some(fingerprint),
        None => FileFingerprint::observe(path).await,
    };
    let content = tokio::fs::read_to_string(path).await?;
    let (after, times) = match FileFingerprint::observe_with_times(path).await {
        Some((fingerprint, times)) => (Some(fingerprint), times),
        None => (None, FileTimes::default()),
    };
    let read_at = SystemTime::now();
    let observed = if before == after { after } else { None };
    Ok(NoteRead {
        content,
        observed: observed.map(|fingerprint| Observed::at(fingerprint, read_at)),
        times,
    })
}

/// Parse what [`read_note_with_fingerprint`] read from `path`, with the
/// file's own created and modified times, which the parser can't know.
pub(super) fn parse_note(parser: &Parser, path: &Path, read: &NoteRead) -> Result<VaultFile> {
    let mut file = parser.parse_file(path, &read.content)?;
    read.times.stamp(&mut file);
    Ok(file)
}

/// A parsed note and what its read observed, ready for the index.
pub(super) type ParsedNote = (PathBuf, VaultFile, Option<Observed>);

/// Parse a chunk of initialization's reads, in parallel and in order,
/// dropping (with a warning) any note that couldn't be read or parsed.
pub(super) fn parse_reads(
    parser: &Parser,
    reads: Vec<(PathBuf, std::io::Result<NoteRead>)>,
) -> Vec<ParsedNote> {
    reads
        .into_par_iter()
        .filter_map(|(file_path, read)| {
            let read = match read {
                Ok(read) => read,
                Err(e) => {
                    tracing::warn!("Failed to read file {}: {}", file_path.display(), e);
                    return None;
                }
            };
            match parse_note(parser, &file_path, &read) {
                Ok(vault_file) => Some((file_path, vault_file, read.observed)),
                Err(e) => {
                    tracing::warn!("Failed to parse {}: {}", file_path.display(), e);
                    None
                }
            }
        })
        .collect()
}

/// The note cache and link graph for a vault, from its parsed notes.
///
/// Two passes: every note is added to the graph before any link is resolved,
/// so a link finds its target regardless of scan order.
pub(super) fn build_index(parsed: Vec<ParsedNote>) -> (HashMap<PathBuf, CacheEntry>, LinkGraph) {
    let mut cache = HashMap::with_capacity(parsed.len());
    let mut graph = LinkGraph::new();
    let mut in_order = Vec::with_capacity(parsed.len());

    for (file_path, vault_file, observed) in parsed {
        tracing::debug!(
            "Parsed {}: {} links extracted",
            file_path.display(),
            vault_file.links.len()
        );
        if let Err(e) = graph.add_file(&vault_file) {
            tracing::warn!("Graph add_file failed for {}: {}", file_path.display(), e);
        }
        let file = Arc::new(vault_file);
        in_order.push(Arc::clone(&file));
        cache.insert(file_path, CacheEntry { file, observed });
    }

    for vault_file in &in_order {
        if let Err(e) = graph.update_links(vault_file) {
            tracing::warn!("Graph update_links failed: {}", e);
        }
    }
    (cache, graph)
}

/// What [`read_note_with_fingerprint`] read.
pub(super) struct NoteRead {
    pub(super) content: String,
    /// `None` when the note changed during the read, so the next sweep
    /// looks again.
    pub(super) observed: Option<Observed>,
    pub(super) times: FileTimes,
}

impl VaultManager {
    /// Initialize vault by scanning all files
    #[instrument(skip(self), name = "vault_initialize")]
    pub async fn initialize(&self) -> Result<()> {
        tracing::info!("Starting vault initialization for: {:?}", self.vault_path);

        // Scan for markdown files, keeping the metadata the scan already read:
        // it becomes each cache entry's fingerprint, so the first freshness
        // sweep after startup compares against the same observation the graph
        // was built from and reports nothing. The walk is blocking I/O, so it
        // runs on the blocking pool.
        let spec = Arc::clone(&self.scan_spec);
        let scanned = tokio::task::spawn_blocking(move || spec.walk())
            .await
            .map_err(|e| Error::other(format!("vault scan task failed: {e}")))??;
        tracing::info!("Found {} markdown files", scanned.len());

        // Notes only, matching `sync_index`. Caching an admitted-but-not-a-
        // note file here (a `.txt`, a `.canvas`) would put it somewhere no
        // applier ever updates it, so every freshness sweep would report it
        // changed again.
        //
        // The reads overlap, since each one is independent and waiting on them
        // one at a time is most of what startup costs on a large vault or a
        // slow disk. `buffered` hands them back in scan order, so the cache and
        // graph are built in the same order as before.
        //
        // Parsing is CPU work, seconds of it on a large vault, so each chunk
        // of reads is parsed in parallel on the blocking pool, and only its
        // parsed notes are kept: the whole vault's raw text is never held at
        // once.
        let notes = scanned.into_iter().filter(|note| is_note(&note.path));
        let mut reads = futures::stream::iter(notes)
            .map(|note| async move {
                let fingerprint = note.fingerprint();
                let read = read_note_with_fingerprint(&note.path, Some(fingerprint)).await;
                (note.path, read)
            })
            .buffered(INITIAL_READ_CONCURRENCY);
        let parser = Arc::new(Parser::new(self.vault_path.clone()));
        let mut parsed = Vec::new();
        loop {
            let chunk: Vec<_> = reads.by_ref().take(INITIAL_PARSE_CHUNK).collect().await;
            if chunk.is_empty() {
                break;
            }
            let parser = Arc::clone(&parser);
            let notes = tokio::task::spawn_blocking(move || parse_reads(&parser, chunk))
                .await
                .map_err(|e| Error::other(format!("vault parse task failed: {e}")))?;
            parsed.extend(notes);
        }

        // Linking builds the cache and graph off to the side, on the blocking
        // pool too. Neither lock is held until they are swapped in.
        let (cache, graph) = tokio::task::spawn_blocking(move || build_index(parsed))
            .await
            .map_err(|e| Error::other(format!("vault index task failed: {e}")))?;

        tracing::info!(
            "Vault initialization complete. Graph now has {} files, {} links",
            graph.node_count(),
            graph.edge_count()
        );
        let mut cache_guard = self.file_cache.write().await;
        let mut graph_guard = self.link_graph.write().await;
        *cache_guard = cache;
        *graph_guard = graph;

        Ok(())
    }

    /// Scan for markdown files in vault
    pub(super) fn scan_files(&self) -> Result<Vec<PathBuf>> {
        Ok(self
            .scan_files_with_metadata()?
            .into_iter()
            .map(|entry| entry.path)
            .collect())
    }

    /// Scan for markdown files, keeping the metadata the scan already reads.
    ///
    /// The size filter stats every candidate anyway, so returning that stat
    /// costs nothing. Consumers that maintain their own derived state — a
    /// search or vector index — can diff `(size_bytes, modified)` against what
    /// they stored and re-read only what actually changed, instead of reading
    /// every note to discover that most of them did not. It is the same
    /// comparison [`Self::ensure_fresh`] makes for the manager's own state.
    pub fn scan_vault_with_metadata(&self) -> Result<Vec<ScannedNote>> {
        self.scan_files_with_metadata()
    }

    pub(super) fn scan_files_with_metadata(&self) -> Result<Vec<ScannedNote>> {
        self.scan_spec.walk()
    }

    /// Insert a parsed `VaultFile` into the note cache alongside the
    /// fingerprint of the revision it was parsed from.
    ///
    /// Recording the fingerprint here is what keeps the freshness sweep quiet
    /// about this process's own writes: the next scan finds exactly what this
    /// entry says it saw, so the note never enters the diff.
    pub(super) async fn insert_cache_entry(
        &self,
        path: PathBuf,
        file: VaultFile,
        observed: Option<Observed>,
    ) {
        let mut cache = self.file_cache.write().await;
        let file = Arc::new(file);
        cache.insert(path, CacheEntry { file, observed });
    }

    /// [`parse_note`] with this manager's parser.
    pub(super) fn parse_read(&self, path: &Path, read: &NoteRead) -> Result<VaultFile> {
        parse_note(&self.parser, path, read)
    }

    /// Bring the link graph + note cache into agreement with a substrate
    /// apply's verdict on which paths are now present (design R7 — one
    /// post-apply sync shared by `write_file`/`delete_file`/`move_file`,
    /// replacing their three near-duplicate pre-M3a graph/cache blocks).
    ///
    /// Markdown-only, mirroring pre-M3a `write_file`'s `is_markdown` guard:
    /// the link graph and note cache model *notes*, so attachments and other
    /// non-note artifacts are never graph nodes. (Pre-M3a `move_file` did
    /// not apply this guard on its insert side — any UTF-8-decodable file
    /// was cached regardless of extension; this unifies on the stricter,
    /// already-established `write_file` behavior. No existing caller relies
    /// on a non-markdown file appearing in the cache after a move.)
    pub(super) async fn sync_index(&self, changed: &[(String, bool)]) {
        let notes: Vec<(PathBuf, bool)> = changed
            .iter()
            .map(|(rel_path, present)| (self.vault_path.join(rel_path), *present))
            .filter(|(full_path, _)| is_note(full_path))
            .collect();
        if notes.is_empty() {
            return;
        }

        // Where each note stands on disk, read with overlapping I/O: a pull
        // or a sync that changes thousands of notes would otherwise wait on
        // each read in turn. `buffered` keeps them in the order reported.
        let spec = Arc::clone(&self.scan_spec);
        let observed: Vec<(PathBuf, bool, Option<std::io::Result<NoteRead>>)> =
            futures::stream::iter(notes)
                .map(|(full_path, present)| {
                    let spec = Arc::clone(&spec);
                    async move {
                        let path = full_path.clone();
                        let in_scope = tokio::task::spawn_blocking(move || spec.admits(&path))
                            .await
                            .unwrap_or(false);
                        let read = if present && in_scope {
                            Some(read_note_with_fingerprint(&full_path, None).await)
                        } else {
                            None
                        };
                        (full_path, in_scope, read)
                    }
                })
                .buffered(INITIAL_READ_CONCURRENCY)
                .collect()
                .await;

        // Parsing is CPU work; it runs on the blocking pool, notes in
        // parallel, and comes back in the same order.
        let parser = Parser::new(self.vault_path.clone());
        let updates = tokio::task::spawn_blocking(move || {
            observed
                .into_par_iter()
                .map(|(full_path, in_scope, read)| {
                    let update = match read {
                        Some(Ok(read)) => match parse_note(&parser, &full_path, &read) {
                            Ok(vault_file) => {
                                IndexUpdate::Upsert(Arc::new(vault_file), read.observed)
                            }
                            Err(e) => {
                                tracing::warn!(
                                    "Failed to parse {} after apply (graph not updated): {}",
                                    full_path.display(),
                                    e
                                );
                                IndexUpdate::Skip
                            }
                        },
                        Some(Err(e)) => {
                            tracing::warn!(
                                "Failed to re-read {} after apply: {}",
                                full_path.display(),
                                e
                            );
                            IndexUpdate::Skip
                        }
                        // Re-check disk state before evicting: `sync_index`
                        // runs after `substrate.apply()` has already released
                        // its write-lock, so a concurrent blind write that
                        // recreated this path can land between this apply's
                        // completion and this eviction. If the path exists
                        // again, this "removed" notification is stale, so skip it
                        // and let the write that recreated the path reconcile
                        // the cache via its own present=true branch, instead of
                        // clobbering a legitimately present file.
                        // ponytail: narrows the race (check-then-evict is still
                        // not atomic with the recreating write) rather than
                        // closing it outright; a full fix serializes sync_index
                        // with apply() per path, add if this ever proves
                        // observable in practice.
                        //
                        // A note that is there but out of the scan's reach
                        // (too large, or behind a symlinked folder) is evicted
                        // however it was reported, so the cache never holds
                        // what a sweep can't find.
                        None if in_scope => IndexUpdate::Skip,
                        None => IndexUpdate::Remove,
                    };
                    (full_path, update)
                })
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();

        // Applied in the order reported, a chunk per hold of the graph and of
        // the cache, so readers wait on a bounded slice of a large batch.
        let mut updates = updates.into_iter();
        loop {
            let chunk: Vec<(PathBuf, IndexUpdate)> = updates.by_ref().take(SYNC_CHUNK).collect();
            if chunk.is_empty() {
                break;
            }
            {
                let mut graph = self.link_graph.write().await;
                for (full_path, update) in &chunk {
                    match update {
                        IndexUpdate::Upsert(vault_file, _) => {
                            if let Err(e) = graph.add_file(vault_file) {
                                tracing::warn!(
                                    "Graph add_file failed for {}: {}",
                                    full_path.display(),
                                    e
                                );
                            }
                            if let Err(e) = graph.update_links(vault_file) {
                                tracing::warn!(
                                    "Graph update_links failed for {}: {}",
                                    full_path.display(),
                                    e
                                );
                            }
                        }
                        IndexUpdate::Remove => {
                            let _ = graph.remove_file(full_path);
                        }
                        IndexUpdate::Skip => {}
                    }
                }
            }
            let mut cache = self.file_cache.write().await;
            for (full_path, update) in chunk {
                match update {
                    IndexUpdate::Upsert(file, observed) => {
                        cache.insert(full_path, CacheEntry { file, observed });
                    }
                    IndexUpdate::Remove => {
                        cache.remove(&full_path);
                    }
                    IndexUpdate::Skip => {}
                }
            }
        }
    }

    /// The graph handle, NOT self-flushing. The reindex/drain machinery
    /// (`flush_reindex`/`apply_commit_diff`) needs it raw: `flush_reindex`
    /// already holds the queue's flush lock while draining, so routing it
    /// through [`Self::link_graph_flushed`] would re-enter that lock and
    /// deadlock.
    pub(crate) fn graph_handle(&self) -> Arc<RwLock<LinkGraph>> {
        Arc::clone(&self.link_graph)
    }

    /// Self-flushing link-graph accessor (see `get_backlinks`): passes the
    /// freshness gate before handing back the graph handle, so every reader
    /// gets the same coherence guarantee as `get_backlinks`/`get_stats`/etc.
    /// without each call site having to remember to flush itself.
    pub async fn link_graph_flushed(&self) -> Arc<RwLock<LinkGraph>> {
        self.ensure_fresh().await;
        self.graph_handle()
    }

    /// Parse a single file and return VaultFile
    #[instrument(skip(self), fields(file = ?path), name = "vault_parse_file")]
    pub async fn parse_file(&self, path: &Path) -> Result<VaultFile> {
        let full_path = self.resolve_path(path)?;
        let read = read_note_with_fingerprint(&full_path, None)
            .await
            .map_err(|e| Error::io_at(self.relative_path(&full_path), e))?;
        self.parse_read(&full_path, &read)
            .map_err(|e| Error::parse_error(e.to_string()))
    }

    /// Synchronize one note's on-disk state into the parsed cache and link graph.
    ///
    /// This is used after an external transactional operation, such as rollback,
    /// that creates, rewrites, or removes a note without going through the normal
    /// `write_file`/`delete_file` paths.
    pub async fn refresh_file_state(&self, path: &Path) -> Result<()> {
        let full_path = self.resolve_path(path)?;
        if !is_note(&full_path) {
            return Ok(()); // Not something the cache or the graph models.
        }
        match read_note_with_fingerprint(&full_path, None).await {
            Ok(read) => {
                let vault_file = self
                    .parse_read(&full_path, &read)
                    .map_err(|error| Error::parse_error(error.to_string()))?;
                {
                    let mut graph = self.link_graph.write().await;
                    graph.add_file(&vault_file)?;
                    graph.update_links(&vault_file)?;
                }
                self.insert_cache_entry(full_path, vault_file, read.observed)
                    .await;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                {
                    let mut graph = self.link_graph.write().await;
                    let _ = graph.remove_file(&full_path);
                }
                self.file_cache.write().await.remove(&full_path);
                Ok(())
            }
            Err(error) => Err(Error::io(error)),
        }
    }

    /// Scan vault and return list of all markdown files
    #[instrument(skip(self), name = "vault_scan")]
    pub async fn scan_vault(&self) -> Result<Vec<PathBuf>> {
        self.scan_files()
    }

    /// Return clones of all `VaultFile` objects currently in the in-memory cache.
    ///
    /// The cache is populated during `initialize()` and kept up-to-date on every
    /// write/delete/move. Callers that only need parsed metadata (frontmatter, links)
    /// and can tolerate up-to-millisecond staleness should prefer this over
    /// `scan_vault()` + `parse_file()`, which issues a filesystem scan and N file
    /// reads on every call.
    ///
    /// Each note is copied in full; [`Self::cached_notes`] shares them instead.
    pub async fn all_cached_vault_files(&self) -> Vec<VaultFile> {
        let cache = self.file_cache.read().await;
        cache.values().map(|e| VaultFile::clone(&e.file)).collect()
    }

    /// Every note in the cache, shared rather than copied, without passing
    /// the freshness gate. Most readers want [`Self::notes_validated`].
    pub async fn cached_notes(&self) -> Vec<Arc<VaultFile>> {
        let cache = self.file_cache.read().await;
        cache.values().map(|e| Arc::clone(&e.file)).collect()
    }

    /// The cached note at `path` (absolute, as the cache keys it), shared,
    /// without passing the freshness gate.
    pub async fn cached_note(&self, path: &Path) -> Option<Arc<VaultFile>> {
        let cache = self.file_cache.read().await;
        cache.get(path).map(|e| Arc::clone(&e.file))
    }

    /// Every note, after the freshness gate, shared rather than copied: the
    /// same notes as [`Self::vault_files_validated`] without copying the
    /// vault on every call.
    pub async fn notes_validated(&self) -> Vec<Arc<VaultFile>> {
        self.ensure_fresh().await;
        self.cached_notes().await
    }

    /// Return every cached vault file, after bringing the cache into agreement
    /// with disk.
    ///
    /// Thin wrapper over [`Self::ensure_fresh`] plus
    /// [`Self::all_cached_vault_files`], and that is the point: it used to run
    /// its own per-call mtime sweep over the entries it already held, which
    /// meant it could never discover a note created outside this process, and
    /// meant a burst of tool calls paid for one scan each. Sharing the gate
    /// fixes both, and lines this view up with the link graph and the search
    /// index rather than letting each drift on its own schedule.
    pub async fn vault_files_validated(&self) -> Vec<VaultFile> {
        self.ensure_fresh().await;
        self.all_cached_vault_files().await
    }
}
