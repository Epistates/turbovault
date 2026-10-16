//! Resolving vault paths, and the protected-directory policy.

use super::*;

impl VaultManager {
    /// Resolve a path against the vault, with traversal protection.
    ///
    /// This is the note-API resolver: it enforces BOTH the vault boundary and
    /// the in-vault protected-directory policy (see
    /// [`Self::ensure_path_is_not_protected`]). Callers holding an explicit
    /// capability grant for protected state use
    /// [`Self::resolve_path_bypassing_policy`] instead.
    ///
    /// The protected-directory policy is applied to the path as written and
    /// again to where it really lands, since a symlink inside the vault can
    /// reach `.git` without the path ever naming it.
    pub fn resolve_path(&self, path: &Path) -> Result<PathBuf> {
        let resolved = self.resolve_in_vault(path)?;
        self.ensure_path_is_not_protected(&resolved.path)?;
        if let Some(component) = self.protected_component(&resolved.real_relative) {
            return Err(Error::protected_path(resolved.path, component));
        }
        Ok(resolved.path)
    }

    /// Resolve a path with vault-boundary (traversal) protection ONLY.
    ///
    /// The in-vault protected-directory policy is not applied. Every caller
    /// must have an explicit capability grant to reach protected state — today
    /// that is the plugin host's config-read capability. Prefer
    /// [`Self::resolve_path`] everywhere else.
    pub fn resolve_path_bypassing_policy(&self, path: &Path) -> Result<PathBuf> {
        Ok(self.resolve_in_vault(path)?.path)
    }

    /// [`PathValidator::resolve_in_vault`], with the vault root's real
    /// location worked out once rather than on every call, and the path
    /// spelled as the filesystem stores it (see [`Self::on_disk_spelling`]).
    pub(super) fn resolve_in_vault(&self, path: &Path) -> Result<turbovault_core::ResolvedPath> {
        let mut resolved =
            PathValidator::resolve_in_vault_at(&self.vault_path, self.real_root()?, path)?;
        resolved.path = self.on_disk_spelling(resolved.path);
        Ok(resolved)
    }

    /// `path`, inside the vault, with every part of it that exists spelled
    /// the way the filesystem stores it.
    ///
    /// The default macOS and Windows filesystems find a file whatever the
    /// case of its name, and macOS whichever Unicode form its accents are
    /// written in, so `projects/roadmap.md` opens `Projects/Roadmap.md`. The
    /// note cache, the link graph and search are keyed by the names the scan
    /// reads from disk, so a path spelled another way was indexed as a second
    /// note. A filesystem that tells names apart by case is left alone: there
    /// the two spellings are two files.
    pub(super) fn on_disk_spelling(&self, path: PathBuf) -> PathBuf {
        if !*self
            .folds_names
            .get_or_init(|| filesystem_folds_names(&self.vault_path))
        {
            return path;
        }
        // A note the cache holds is already spelled as stored.
        if self
            .file_cache
            .try_read()
            .is_ok_and(|cache| cache.contains_key(&path))
        {
            return path;
        }
        let Ok(relative) = path.strip_prefix(&self.vault_path) else {
            return path;
        };

        let mut spelled = self.vault_path.clone();
        let mut components = relative.components();
        while let Some(component) = components.next() {
            let std::path::Component::Normal(name) = component else {
                return path;
            };
            match stored_name(&spelled, name) {
                Some(stored) => spelled.push(stored),
                None => {
                    // Nothing by this name yet, so nothing below it either.
                    spelled.push(name);
                    spelled.extend(components);
                    break;
                }
            }
        }
        spelled
    }

    /// The vault root's real location (see the `real_root` field). Only a
    /// successful resolution is kept, so a failure is retried next time.
    pub(super) fn real_root(&self) -> Result<&PathBuf> {
        if let Some(real_root) = self.real_root.get() {
            return Ok(real_root);
        }
        let real_root = PathValidator::real_root(&self.vault_path)?;
        Ok(self.real_root.get_or_init(|| real_root))
    }

    /// Refuse a path that lies inside the vault but under a protected
    /// directory.
    ///
    /// Staying inside the vault root is not sufficient authorization. A vault
    /// also contains application and tool state whose contents are executed or
    /// trusted by something: `.obsidian/plugins/*/main.js` is code Obsidian
    /// runs, `.git/hooks/*` is code git runs, and `.turbovault/` holds the
    /// audit trail that is supposed to be a record of what the note APIs did.
    /// Exposing those through a note read/write turns "edit my notes" into code
    /// execution and makes the audit log self-editable.
    ///
    /// The configurable part is [`VaultConfig::excluded_paths`], which already
    /// defaults to `.obsidian`/`.git`/`node_modules`/`.DS_Store` — until now it
    /// only filtered directory scans, so the policy existed without being
    /// enforced at the point of access. [`PROTECTED_COMPONENTS`] is the part no
    /// configuration can open up.
    ///
    /// Note that `allowed_extensions` is deliberately NOT enforced here: it
    /// describes what counts as a note for discovery, and attachments beside
    /// notes are a normal thing to read and write.
    pub fn ensure_path_is_not_protected(&self, resolved: &Path) -> Result<()> {
        // A path outside the vault is the traversal check's business, not ours.
        let Ok(relative) = resolved.strip_prefix(&self.vault_path) else {
            return Ok(());
        };
        match self.protected_component(relative) {
            Some(component) => Err(Error::protected_path(resolved, component)),
            None => Ok(()),
        }
    }

    /// The first component of a vault-relative path that names a protected
    /// directory, if any.
    ///
    /// Names are compared the way the filesystems vaults live on compare them,
    /// not byte for byte. The default macOS and Windows filesystems ignore
    /// case, so `.Git` is `.git` there, and Windows drops trailing dots and
    /// spaces, so `.git.` is too. A byte comparison let every one of those
    /// through to the directory it was meant to keep out. Refusing them on a
    /// case-sensitive filesystem as well costs only the ability to name a
    /// note folder `.GIT`.
    pub(super) fn protected_component(&self, relative: &Path) -> Option<String> {
        relative.components().find_map(|component| {
            let std::path::Component::Normal(raw) = component else {
                return None;
            };
            let name = raw.to_string_lossy();
            let folded = name.trim_end_matches(['.', ' ']).to_lowercase();
            let protected = self.protected_names.contains(&folded);
            protected.then(|| name.into_owned())
        })
    }
}

/// The name `dir` stores for `name`: `name` itself when it's there as
/// written, otherwise the one entry it matches ignoring case and Unicode
/// form. `None` when nothing in `dir` matches, including when `dir` can't be
/// read.
fn stored_name(dir: &Path, name: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
    let entries: Vec<std::ffi::OsString> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect();
    if entries.iter().any(|entry| entry == name) {
        return Some(name.to_os_string());
    }
    let wanted = fold_name(name.to_str()?);
    entries.into_iter().find(|entry| {
        entry
            .to_str()
            .is_some_and(|entry| fold_name(entry) == wanted)
    })
}

/// A file name as a name-folding filesystem compares it: lower case, and on
/// macOS in NFC too, since APFS and HFS+ also find a name whichever Unicode
/// form its accents are in. NTFS doesn't, so there two forms are two files.
fn fold_name(name: &str) -> String {
    if cfg!(target_os = "macos") {
        use unicode_normalization::UnicodeNormalization;
        name.nfc().flat_map(char::to_lowercase).collect()
    } else {
        name.chars().flat_map(char::to_lowercase).collect()
    }
}

/// Whether the filesystem `root` is on finds a name whatever its case.
///
/// Looks up an existing name with its case flipped: the names in the root,
/// which are on the vault's own filesystem, then the root's name in its
/// parent. Finding the same file under the flipped name means the filesystem
/// folds case. Nothing with letters in it to try means nothing can be spelled
/// two ways, so the answer doesn't matter and is no.
pub(super) fn filesystem_folds_names(root: &Path) -> bool {
    let entries = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .take(64)
        .map(|name| (root.to_path_buf(), name));
    let root_entry = root
        .parent()
        .zip(root.file_name())
        .map(|(parent, name)| (parent.to_path_buf(), name.to_os_string()));
    entries
        .chain(root_entry)
        .find_map(|(dir, name)| {
            let name = name.to_str()?;
            let flipped: String = name
                .chars()
                .flat_map(|c| {
                    if c.is_lowercase() {
                        c.to_uppercase().collect::<Vec<_>>()
                    } else {
                        c.to_lowercase().collect()
                    }
                })
                .collect();
            (flipped != name).then(|| same_file(&dir.join(name), &dir.join(&flipped)))
        })
        .unwrap_or(false)
}

/// Whether two paths name the same existing file.
fn same_file(a: &Path, b: &Path) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev() && a.ino() == b.ino()
    }
    #[cfg(not(unix))]
    {
        // Windows has no stable file id in std. Two names that differ only
        // in case both existing, with the same kind, size and times, are one
        // file in practice.
        a.file_type() == b.file_type()
            && a.len() == b.len()
            && a.modified().ok() == b.modified().ok()
            && a.created().ok() == b.created().ok()
    }
}
