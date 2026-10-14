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
    /// location worked out once rather than on every call.
    pub(super) fn resolve_in_vault(&self, path: &Path) -> Result<turbovault_core::ResolvedPath> {
        PathValidator::resolve_in_vault_at(&self.vault_path, self.real_root()?, path)
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
