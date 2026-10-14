//! Notes as MCP resources: what `resources/list` enumerates, the URIs they are
//! read and subscribed by, and the notifications a subscriber receives.
//!
//! A note is `obsidian://note/{vault}/{+path}`: the `obsidian://` scheme the
//! vault's other resources already publish under (so no plugin can mount over
//! it), then the vault's name, so the same path in two vaults is two
//! resources and a URI stays valid whichever vault is active, then the
//! vault-relative path with `/` kept and anything else that cannot appear in a
//! URI percent-escaped (RFC 6570 reserved expansion).
//!
//! The listing is a snapshot kept current by the vault change listener, the
//! one every write and every freshness sweep reports through, since
//! `resources/list` is answered synchronously. The same report drives the
//! notifications: `notifications/resources/updated` to each session that
//! subscribed to a note that changed, and `notifications/resources/list_changed`
//! to each session that has listed resources when a note appears or goes.
//! The SDK keeps no subscription registry, so this does; sessions are held
//! weakly and a closed one is dropped the first time it cannot be reached.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::Duration;

use percent_encoding::{AsciiSet, CONTROLS, percent_decode_str, utf8_percent_encode};
use turbomcp::prelude::*;
use turbomcp_core::session::McpSession;

/// The scheme and kind every note URI starts with.
pub(crate) const NOTE_URI_PREFIX: &str = "obsidian://note/";

/// The resource template a client expands to reach any note.
pub(crate) const NOTE_URI_TEMPLATE: &str = "obsidian://note/{vault}/{+path}";

/// What a vault-relative path cannot carry as it is in a URI. `/` stays, so a
/// path reads as a path.
const PATH_ESCAPES: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// A vault name is one URI segment, so its `/` is escaped too.
const SEGMENT_ESCAPES: &AsciiSet = &PATH_ESCAPES.add(b'/');

/// How long a burst of added or removed notes is gathered into one
/// `list_changed`, so a batch write or a `git pull` is one notification.
const LIST_CHANGED_DELAY: Duration = Duration::from_millis(250);

/// How often a vault someone subscribed to is checked for edits made outside
/// TurboVault. The freshness gate debounces on its own, so this only bounds
/// how late an external edit's notification can be.
pub(crate) const SUBSCRIPTION_SWEEP_INTERVAL: Duration = Duration::from_secs(2);

/// The URI of `path` (vault-relative, `/`-separated) in `vault`.
pub(crate) fn note_uri(vault: &str, path: &str) -> String {
    format!(
        "{NOTE_URI_PREFIX}{}/{}",
        utf8_percent_encode(vault, SEGMENT_ESCAPES),
        utf8_percent_encode(path, PATH_ESCAPES)
    )
}

/// The vault and vault-relative path a note URI names, or `None` when `uri`
/// is not a note URI.
pub(crate) fn parse_note_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix(NOTE_URI_PREFIX)?;
    let (vault, path) = rest.split_once('/')?;
    let vault = percent_decode_str(vault).decode_utf8().ok()?.into_owned();
    let path = percent_decode_str(path).decode_utf8().ok()?.into_owned();
    (!vault.is_empty() && !path.is_empty()).then_some((vault, path))
}

/// The resource template for notes.
pub(crate) fn note_template() -> ResourceTemplate {
    let mut template = ResourceTemplate::new(NOTE_URI_TEMPLATE, "note").with_description(
        "A note in a registered vault, as markdown: `vault` is the vault's name (list_vaults), \
         `path` the note's vault-relative path, such as `projects/Plan.md`. Subscribe to be told \
         when it changes, by TurboVault or by any other editor.",
    );
    template.mime_type = Some("text/markdown".to_string());
    template.title = Some("Vault note".to_string());
    template
}

type SessionKey = usize;

fn session_key(session: &Arc<dyn McpSession>) -> SessionKey {
    Arc::as_ptr(session).cast::<()>() as usize
}

/// Sessions by identity, held weakly so a closed one can go.
type Sessions = HashMap<SessionKey, Weak<dyn McpSession>>;

/// Every note published as a resource, and who wants to hear about them.
#[derive(Default)]
pub(crate) struct ResourceHub {
    /// Vault name to its notes' vault-relative paths.
    notes: RwLock<BTreeMap<String, BTreeSet<String>>>,
    /// Note URI to the sessions subscribed to it.
    subscriptions: Mutex<HashMap<String, Sessions>>,
    /// Sessions that listed resources, and so may want `list_changed`.
    listeners: Mutex<Sessions>,
    /// A `list_changed` is already on its way.
    list_changed_pending: AtomicBool,
}

impl ResourceHub {
    /// Every note, as resources, in vault then path order.
    pub(crate) fn list(&self) -> Vec<Resource> {
        let notes = self.notes.read().unwrap_or_else(PoisonError::into_inner);
        notes
            .iter()
            .flat_map(|(vault, paths)| {
                paths.iter().map(move |path| {
                    Resource::new(note_uri(vault, path), path.clone())
                        .with_mime_type("text/markdown")
                })
            })
            .collect()
    }

    /// The vault-relative note paths of `vault` that start with `prefix`,
    /// case-insensitively, at most `limit` of them in path order, and how
    /// many matched in all.
    pub(crate) fn complete_path(
        &self,
        vault: &str,
        prefix: &str,
        limit: usize,
    ) -> (Vec<String>, usize) {
        let notes = self.notes.read().unwrap_or_else(PoisonError::into_inner);
        let Some(paths) = notes.get(vault) else {
            return (Vec::new(), 0);
        };
        let prefix = prefix.to_lowercase();
        let mut matched = paths
            .iter()
            .filter(|path| path.to_lowercase().starts_with(&prefix));
        let values: Vec<String> = matched.by_ref().take(limit).cloned().collect();
        let total = values.len() + matched.count();
        (values, total)
    }

    /// The folders of `vault` that hold notes and start with `prefix`,
    /// case-insensitively, at most `limit` of them in order, and how many
    /// matched in all.
    pub(crate) fn complete_folder(
        &self,
        vault: &str,
        prefix: &str,
        limit: usize,
    ) -> (Vec<String>, usize) {
        let notes = self.notes.read().unwrap_or_else(PoisonError::into_inner);
        let Some(paths) = notes.get(vault) else {
            return (Vec::new(), 0);
        };
        let prefix = prefix.to_lowercase();
        let folders: BTreeSet<&str> = paths
            .iter()
            .filter_map(|path| path.rsplit_once('/').map(|(folder, _)| folder))
            .flat_map(|folder| {
                // Every ancestor is a folder too.
                folder
                    .match_indices('/')
                    .map(move |(at, _)| &folder[..at])
                    .chain(std::iter::once(folder))
            })
            .filter(|folder| folder.to_lowercase().starts_with(&prefix))
            .collect();
        let total = folders.len();
        (
            folders
                .into_iter()
                .take(limit)
                .map(str::to_string)
                .collect(),
            total,
        )
    }

    /// Publish `vault`'s notes, replacing whatever was listed for it.
    pub(crate) fn seed(self: &Arc<Self>, vault: &str, paths: impl IntoIterator<Item = String>) {
        let paths: BTreeSet<String> = paths.into_iter().collect();
        let changed = {
            let mut notes = self.notes.write().unwrap_or_else(PoisonError::into_inner);
            notes.insert(vault.to_string(), paths.clone()) != Some(paths)
        };
        if changed {
            self.announce_list_changed();
        }
    }

    /// Stop publishing `vault`'s notes, as when it is removed.
    pub(crate) fn forget(self: &Arc<Self>, vault: &str) {
        let removed = self
            .notes
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(vault)
            .is_some_and(|paths| !paths.is_empty());
        if removed {
            self.announce_list_changed();
        }
    }

    /// Apply a change report from `vault`'s change listener: each
    /// `(path, present)` is a note that changed and whether it exists now.
    /// Tells each subscriber of a changed note, and every session that listed
    /// resources when a note came or went.
    pub(crate) fn apply(self: &Arc<Self>, vault: &str, changed: &[(String, bool)]) {
        if changed.is_empty() {
            return;
        }
        let mut list_changed = false;
        {
            let mut notes = self.notes.write().unwrap_or_else(PoisonError::into_inner);
            let paths = notes.entry(vault.to_string()).or_default();
            for (path, present) in changed {
                if !turbovault_core::is_note(std::path::Path::new(path)) {
                    continue;
                }
                list_changed |= if *present {
                    paths.insert(path.clone())
                } else {
                    paths.remove(path)
                };
            }
        }

        let updated: Vec<(String, Vec<Arc<dyn McpSession>>)> = {
            let mut subscriptions = self
                .subscriptions
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            changed
                .iter()
                .filter_map(|(path, _)| {
                    let uri = note_uri(vault, path);
                    let sessions = subscriptions.get_mut(&uri)?;
                    let live = live_sessions(sessions);
                    (!live.is_empty()).then_some((uri, live))
                })
                .collect()
        };
        for (uri, sessions) in updated {
            for session in sessions {
                let uri = uri.clone();
                spawn(async move {
                    let _ = session
                        .notify(
                            "notifications/resources/updated",
                            serde_json::json!({ "uri": uri }),
                        )
                        .await;
                });
            }
        }

        if list_changed {
            self.announce_list_changed();
        }
    }

    /// Remember that `session` lists resources, so it hears when the list
    /// changes.
    pub(crate) fn remember(&self, session: &Arc<dyn McpSession>) {
        self.listeners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session_key(session), Arc::downgrade(session));
    }

    /// Subscribe `session` to `uri`.
    pub(crate) fn subscribe(&self, uri: &str, session: &Arc<dyn McpSession>) {
        self.remember(session);
        self.subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(uri.to_string())
            .or_default()
            .insert(session_key(session), Arc::downgrade(session));
    }

    /// Cancel `session`'s subscription to `uri`.
    pub(crate) fn unsubscribe(&self, uri: &str, session: &Arc<dyn McpSession>) {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(sessions) = subscriptions.get_mut(uri) {
            sessions.remove(&session_key(session));
            if sessions.is_empty() {
                subscriptions.remove(uri);
            }
        }
    }

    /// The vaults that have at least one note someone is subscribed to and
    /// still connected for.
    pub(crate) fn subscribed_vaults(&self) -> BTreeSet<String> {
        let mut subscriptions = self
            .subscriptions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscriptions.retain(|_, sessions| !live_sessions(sessions).is_empty());
        subscriptions
            .keys()
            .filter_map(|uri| parse_note_uri(uri).map(|(vault, _)| vault))
            .collect()
    }

    /// Tell every session that listed resources that the list changed, once
    /// for a burst of changes: the first change schedules the notification,
    /// and those that land before it goes out ride along with it.
    fn announce_list_changed(self: &Arc<Self>) {
        if self.list_changed_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let hub = Arc::clone(self);
        let scheduled = spawn(async move {
            tokio::time::sleep(LIST_CHANGED_DELAY).await;
            // Cleared before the sessions are read, so a change after this
            // point schedules a notification of its own.
            hub.list_changed_pending.store(false, Ordering::Release);
            let sessions =
                live_sessions(&mut hub.listeners.lock().unwrap_or_else(PoisonError::into_inner));
            for session in sessions {
                let _ = session
                    .notify(
                        "notifications/resources/list_changed",
                        serde_json::json!({}),
                    )
                    .await;
            }
        });
        if !scheduled {
            self.list_changed_pending.store(false, Ordering::Release);
        }
    }
}

/// The sessions in `sessions` that are still open, dropping the rest.
fn live_sessions(sessions: &mut Sessions) -> Vec<Arc<dyn McpSession>> {
    let mut live = Vec::new();
    sessions.retain(|_, weak| match weak.upgrade() {
        Some(session) => {
            live.push(session);
            true
        }
        None => false,
    });
    live
}

/// Run `future` on the current Tokio runtime, if there is one. Notifications
/// are fire-and-forget: a slow or gone client must not hold up the write or
/// the freshness pass that reported the change.
fn spawn(future: impl std::future::Future<Output = ()> + Send + 'static) -> bool {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn(future);
            true
        }
        Err(_) => false,
    }
}

impl super::CoreToolHandler {
    /// The note resources every registered vault has published.
    pub(super) fn note_resources(&self) -> Vec<Resource> {
        self.resource_hub.list()
    }

    /// Remember that the client behind `ctx` lists resources, so it is told
    /// when notes come and go.
    pub(super) fn remember_resource_listener(&self, ctx: &RequestContext) {
        if let Some(session) = ctx.session() {
            self.resource_hub.remember(session);
        }
    }

    /// The manager for `vault`, building it on first use like the active
    /// vault's.
    pub(super) async fn manager_for_vault(
        &self,
        vault: &str,
    ) -> McpResult<Arc<turbovault_vault::VaultManager>> {
        if let Some(manager) = self.cached_manager(vault).await {
            return Ok(manager);
        }
        if !self
            .multi_vault_mgr
            .list_vaults()
            .await
            .unwrap_or_default()
            .iter()
            .any(|registered| registered.name == vault)
        {
            return Err(McpError::resource_not_found(format!(
                "no vault named {vault:?} is registered (list_vaults shows those that are)"
            )));
        }
        self.activate_vault_manager(vault).await
    }

    /// Read the note `uri` names, as markdown, under the same rules as
    /// read_note: protected folders and `max_file_size` apply, and only notes
    /// are resources.
    pub(super) async fn read_note_resource(&self, uri: &str) -> McpResult<ResourceResult> {
        let (vault, path) = parse_note_uri(uri).ok_or_else(|| McpError::resource_not_found(uri))?;
        if !turbovault_core::is_note(std::path::Path::new(&path)) {
            return Err(McpError::resource_not_found(uri));
        }
        let manager = self.manager_for_vault(&vault).await?;
        let content = manager
            .read_file(std::path::Path::new(&path))
            .await
            .map_err(|error| match error.category() {
                turbovault_core::error::ErrorCategory::NotFound => {
                    McpError::resource_not_found(uri)
                }
                _ => super::to_mcp_error(error),
            })?;
        Ok(ResourceResult::text(uri, content).with_mime_type("text/markdown"))
    }

    /// Subscribe the client behind `ctx` to `uri`, a note or one of the
    /// vault's fixed resources (which never change, so it hears nothing).
    pub(super) async fn subscribe_resource(
        &self,
        uri: &str,
        ctx: &RequestContext,
    ) -> McpResult<()> {
        let session = ctx.session().ok_or_else(|| {
            McpError::capability_not_supported(
                "resources/subscribe needs a session to send notifications on",
            )
        })?;
        if let Some((vault, _)) = parse_note_uri(uri) {
            // Activating the vault is what starts its change listener.
            self.manager_for_vault(&vault).await?;
        }
        self.resource_hub.subscribe(uri, session);
        self.ensure_subscription_sweep();
        Ok(())
    }

    /// Cancel the subscription of the client behind `ctx` to `uri`.
    pub(super) fn unsubscribe_resource(&self, uri: &str, ctx: &RequestContext) {
        if let Some(session) = ctx.session() {
            self.resource_hub.unsubscribe(uri, session);
        }
    }

    /// Keep the vaults someone is subscribed to fresh, so an edit made in
    /// Obsidian is announced without waiting for a tool call to notice it.
    /// One task does it for every vault and stops when nobody is subscribed.
    fn ensure_subscription_sweep(&self) {
        use std::sync::atomic::Ordering;
        if self.subscription_sweep.swap(true, Ordering::AcqRel) {
            return;
        }
        let core = self.clone();
        let started = spawn(async move {
            loop {
                tokio::time::sleep(SUBSCRIPTION_SWEEP_INTERVAL).await;
                let vaults = core.resource_hub.subscribed_vaults();
                if vaults.is_empty() {
                    core.subscription_sweep.store(false, Ordering::Release);
                    // A subscription that arrived after the check above
                    // would otherwise go unswept.
                    if core.resource_hub.subscribed_vaults().is_empty()
                        || core.subscription_sweep.swap(true, Ordering::AcqRel)
                    {
                        return;
                    }
                    continue;
                }
                for vault in vaults {
                    let manager = core.cached_manager(&vault).await;
                    if let Some(manager) = manager {
                        manager.ensure_fresh().await;
                    }
                }
            }
        });
        if !started {
            self.subscription_sweep.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_uris_round_trip() {
        for (vault, path) in [
            ("main", "notes/Plan.md"),
            ("My Vault", "Daily/2026-10-13 Review.md"),
            ("a/b", "x#y?.md"),
            ("v", "Ünïcode/ノート.md"),
        ] {
            let uri = note_uri(vault, path);
            assert!(uri.starts_with(NOTE_URI_PREFIX), "{uri}");
            assert!(!uri[NOTE_URI_PREFIX.len()..].contains(' '), "{uri}");
            assert_eq!(
                parse_note_uri(&uri),
                Some((vault.to_string(), path.to_string()))
            );
        }
        assert_eq!(
            note_uri("main", "notes/My Plan.md"),
            "obsidian://note/main/notes/My%20Plan.md"
        );
        assert_eq!(parse_note_uri("obsidian://syntax/quick-ref"), None);
        assert_eq!(parse_note_uri("obsidian://note/main/"), None);
    }

    #[test]
    fn the_listing_follows_the_change_report() {
        let hub = Arc::new(ResourceHub::default());
        hub.seed("main", ["a.md".to_string(), "b.md".to_string()]);
        hub.apply(
            "main",
            &[
                ("c.md".to_string(), true),
                ("a.md".to_string(), false),
                ("image.png".to_string(), true),
            ],
        );
        let uris: Vec<String> = hub.list().into_iter().map(|r| r.uri).collect();
        assert_eq!(
            uris,
            ["obsidian://note/main/b.md", "obsidian://note/main/c.md"]
        );
        hub.forget("main");
        assert!(hub.list().is_empty());
    }

    #[test]
    fn path_completion_is_bounded_and_counts_every_match() {
        let hub = Arc::new(ResourceHub::default());
        hub.seed(
            "main",
            (0..30)
                .map(|i| format!("Projects/p{i:02}.md"))
                .chain(["Other.md".to_string()]),
        );
        let (values, total) = hub.complete_path("main", "proj", 10);
        assert_eq!(values.len(), 10);
        assert_eq!(total, 30);
        assert_eq!(values[0], "Projects/p00.md");
        assert_eq!(hub.complete_path("nope", "", 10), (Vec::new(), 0));
    }
}
