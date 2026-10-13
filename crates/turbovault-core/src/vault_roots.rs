//! Which directories may be registered as a vault while the server runs.
//!
//! Registering a directory as a vault gives every tool caller read and write
//! access to it, and a tool caller is a language model reading untrusted note
//! text. So a path that reaches `add_vault` or `create_vault` is checked
//! against a [`VaultRootPolicy`] before anything else happens to it:
//!
//! - Some directories are never registrable at runtime, whatever the policy:
//!   a filesystem root, the home directory and anything containing it, the
//!   hidden directories under home (`~/.ssh`, `~/.aws`, `~/.config`), the
//!   application data folders (`~/Library` on macOS, apart from the iCloud and
//!   cloud-storage folders vaults sync through, and `AppData` on Windows), and
//!   system directories such as `/etc` and `/usr`. The temporary directory is
//!   exempt, since it holds nothing the server could not already read.
//! - [`VaultRootPolicy::anywhere`] allows any other directory.
//! - [`VaultRootPolicy::within`] allows only directories inside the operator's
//!   allowed roots.
//! - [`VaultRootPolicy::closed`] refuses runtime registration altogether.
//!
//! Vaults the operator names (`--vault`, the config file) are not checked:
//! the operator already has the access the check guards.

use std::path::{Component, Path, PathBuf};

use crate::error::{Error, Result};

/// Where runtime vault registration may point. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct VaultRootPolicy {
    mode: Mode,
}

#[derive(Debug, Clone, Default)]
enum Mode {
    #[default]
    Anywhere,
    Within(Vec<PathBuf>),
    Closed,
}

impl VaultRootPolicy {
    /// Allow any directory that is not always refused.
    pub fn anywhere() -> Self {
        Self {
            mode: Mode::Anywhere,
        }
    }

    /// Allow only directories inside one of `roots`. Each root must exist; it
    /// is resolved through symlinks so a vault path is compared with where
    /// the root really is.
    pub fn within<I, P>(roots: I) -> Result<Self>
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut resolved = Vec::new();
        for root in roots {
            let root = root.as_ref();
            let real = root.canonicalize().map_err(|error| {
                Error::config_error(format!(
                    "allowed vault root {} cannot be used: {error}",
                    root.display()
                ))
            })?;
            if !real.is_dir() {
                return Err(Error::config_error(format!(
                    "allowed vault root {} is not a directory",
                    root.display()
                )));
            }
            resolved.push(real);
        }
        if resolved.is_empty() {
            return Err(Error::config_error(
                "an allowed vault root list must name at least one directory".to_string(),
            ));
        }
        Ok(Self {
            mode: Mode::Within(resolved),
        })
    }

    /// Refuse every runtime registration.
    pub fn closed() -> Self {
        Self { mode: Mode::Closed }
    }

    /// Whether any runtime registration can succeed.
    pub fn allows_registration(&self) -> bool {
        !matches!(self.mode, Mode::Closed)
    }

    /// The allowed roots, resolved, or `None` when the policy is not
    /// restricted to roots.
    pub fn roots(&self) -> Option<&[PathBuf]> {
        match &self.mode {
            Mode::Within(roots) => Some(roots),
            _ => None,
        }
    }

    /// Check `path` (absolute; it need not exist yet) and return where it
    /// resolves to, the path to register. Refused paths are an error that
    /// says why without naming anything but the path itself.
    pub fn check(&self, path: &Path) -> Result<PathBuf> {
        if let Mode::Closed = self.mode {
            return Err(Error::invalid_path(
                "registering vaults at runtime is turned off on this server. The operator \
                 can allow it inside chosen directories with allowed_vault_roots \
                 (--allowed-vault-roots)"
                    .to_string(),
            ));
        }
        if !path.is_absolute() {
            return Err(Error::invalid_path(format!(
                "vault path {} is not absolute",
                path.display()
            )));
        }
        let resolved = soft_canonicalize::soft_canonicalize(path).map_err(|error| {
            Error::invalid_path(format!("cannot resolve {}: {error}", path.display()))
        })?;
        if let Some(reason) = refusal(&resolved, &Places::current()) {
            return Err(Error::invalid_path(format!(
                "{} cannot be registered as a vault: {reason}",
                resolved.display()
            )));
        }
        if let Mode::Within(roots) = &self.mode
            && !roots.iter().any(|root| resolved.starts_with(root))
        {
            return Err(Error::invalid_path(format!(
                "{} is outside the directories this server allows vaults in \
                 (allowed_vault_roots)",
                resolved.display()
            )));
        }
        Ok(resolved)
    }
}

/// The places [`refusal`] judges a path against, resolved once per check.
struct Places {
    home: Option<PathBuf>,
    temp: Option<PathBuf>,
    system: Vec<PathBuf>,
}

impl Places {
    fn current() -> Self {
        let home = home_dir().and_then(|home| home.canonicalize().ok());
        let temp = std::env::temp_dir().canonicalize().ok();
        let system = system_dirs()
            .into_iter()
            .filter_map(|dir| dir.canonicalize().ok())
            .collect();
        Self { home, temp, system }
    }
}

/// Why `path` (resolved) is never registrable at runtime, or `None`.
fn refusal(path: &Path, places: &Places) -> Option<&'static str> {
    if path.parent().is_none() {
        return Some("it is a filesystem root");
    }
    if let Some(temp) = &places.temp
        && path.starts_with(temp)
        && path != temp
    {
        return None;
    }
    if let Some(home) = &places.home {
        if path == home {
            return Some("it is the home directory");
        }
        if home.starts_with(path) {
            return Some("it contains the home directory");
        }
        if let Ok(rest) = path.strip_prefix(home)
            && let Some(reason) = refusal_under_home(rest)
        {
            return Some(reason);
        }
    }
    for dir in &places.system {
        if path.starts_with(dir) || dir.starts_with(path) {
            return Some("it is or contains a system directory");
        }
    }
    None
}

/// Why `rest`, a path relative to the home directory, is refused.
fn refusal_under_home(rest: &Path) -> Option<&'static str> {
    let names: Vec<&str> = rest
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => name.to_str(),
            _ => None,
        })
        .collect();
    if names.iter().any(|name| name.starts_with('.')) {
        return Some("it is inside a hidden directory in the home directory");
    }
    match names.as_slice() {
        // Vaults sync through iCloud and the cloud storage providers here;
        // the rest of ~/Library is application data and keychains.
        ["Library", "Mobile Documents" | "CloudStorage", ..] => None,
        ["Library", ..] => Some("it is inside ~/Library, which holds application data"),
        [first, ..] if first.eq_ignore_ascii_case("AppData") => {
            Some("it is inside AppData, which holds application data")
        }
        _ => None,
    }
}

fn home_dir() -> Option<PathBuf> {
    let home = shellexpand::tilde("~");
    (home != "~").then(|| PathBuf::from(home.as_ref()))
}

/// Directories holding the operating system, its configuration and its
/// programs. Those that don't exist on this machine are skipped.
fn system_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/lib",
        "/lib32",
        "/lib64",
        "/proc",
        "/root",
        "/run",
        "/sbin",
        "/sys",
        "/usr",
        "/var/db",
        "/var/lib",
        "/var/log",
        "/var/root",
        "/var/run",
        "/var/spool",
        "/Applications",
        "/Library",
        "/System",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    for var in [
        "SystemRoot",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramData",
    ] {
        if let Some(dir) = std::env::var_os(var) {
            dirs.push(PathBuf::from(dir));
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn places(home: &Path) -> Places {
        Places {
            home: Some(home.to_path_buf()),
            temp: Some(PathBuf::from("/tmp")),
            system: vec![PathBuf::from("/etc"), PathBuf::from("/usr")],
        }
    }

    #[test]
    fn refuses_roots_home_and_what_holds_secrets() {
        let home = Path::new("/home/me");
        let places = places(home);
        for path in [
            "/",
            "/home",
            "/home/me",
            "/home/me/.ssh",
            "/home/me/.config/app",
            "/home/me/Library",
            "/home/me/Library/Keychains",
            "/home/me/AppData/Roaming",
            "/etc",
            "/etc/ssl",
            "/usr/local",
        ] {
            assert!(
                refusal(Path::new(path), &places).is_some(),
                "{path} should be refused"
            );
        }
    }

    #[test]
    fn allows_ordinary_vault_locations() {
        let home = Path::new("/home/me");
        let places = places(home);
        for path in [
            "/home/me/Documents/Vault",
            "/home/me/Notes",
            "/home/me/Library/Mobile Documents/iCloud~md~obsidian/Documents/Vault",
            "/home/me/Library/CloudStorage/Dropbox/Vault",
            "/srv/vaults/team",
            "/tmp/vault",
        ] {
            assert_eq!(refusal(Path::new(path), &places), None, "{path}");
        }
    }

    #[test]
    fn closed_refuses_everything() {
        let temp = tempfile::TempDir::new().unwrap();
        let error = VaultRootPolicy::closed()
            .check(temp.path())
            .expect_err("closed policy");
        assert!(error.to_string().contains("allowed_vault_roots"), "{error}");
    }

    #[test]
    fn within_allows_only_inside_its_roots() {
        let allowed = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        let policy = VaultRootPolicy::within([allowed.path()]).unwrap();

        let inside = allowed.path().join("Vault");
        assert_eq!(
            policy.check(&inside).unwrap(),
            allowed.path().canonicalize().unwrap().join("Vault")
        );
        assert!(policy.check(&other.path().join("Vault")).is_err());
        // `..` cannot walk out of a root.
        assert!(
            policy
                .check(&allowed.path().join("..").join("escape"))
                .is_err()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_a_root_is_judged_by_its_target() {
        let allowed = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(other.path(), allowed.path().join("link")).unwrap();
        let policy = VaultRootPolicy::within([allowed.path()]).unwrap();

        assert!(policy.check(&allowed.path().join("link")).is_err());
    }

    #[test]
    fn anywhere_still_refuses_the_home_directory() {
        let Some(home) = home_dir() else { return };
        assert!(VaultRootPolicy::anywhere().check(&home).is_err());
        assert!(
            VaultRootPolicy::anywhere()
                .check(&home.join(".ssh"))
                .is_err()
        );
    }

    #[test]
    fn within_needs_existing_roots() {
        assert!(VaultRootPolicy::within(["/turbovault-no-such-root"]).is_err());
        assert!(VaultRootPolicy::within(Vec::<PathBuf>::new()).is_err());
    }
}
