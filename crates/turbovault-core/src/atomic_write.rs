//! The one way TurboVault replaces a file's contents on disk.
//!
//! Every write that has to leave either the old file or the new one, never a
//! mix, goes through [`write_atomic`]: a note written by the direct backend, a
//! file materialized from a git commit, a rollback, a plugin's stored value.
//! There used to be one copy of this per caller, and each had lost something
//! different. None synced to disk, so a crash just after a reported success
//! could lose the write. None kept the replaced file's permissions. One named
//! its temp file `note.tmp` for every `note.md`, which overwrote and then
//! removed a real note of that name.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::Path;

/// Replace the contents of `path` with `contents`, atomically and durably.
///
/// The bytes go to a uniquely named sibling first, are synced to disk, and
/// are then renamed over `path`, so a reader sees the old contents or the new
/// ones and never a torn file. The directory is synced after the rename where
/// the platform allows it, so the rename itself survives a crash. An existing
/// file keeps its permissions; a new one gets the process default, the same
/// as any other file it creates. Missing parent directories are created.
///
/// On failure the temp file is removed and `path` is untouched.
///
/// This blocks. Call it from `spawn_blocking` in async code.
pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;

    let temp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
    let written = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(contents)?;
        if let Ok(existing) = fs::metadata(path) {
            file.set_permissions(existing.permissions())?;
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(error);
    }

    sync_dir(parent);
    Ok(())
}

/// Sync a directory so a rename inside it is durable. Best effort: the write
/// has already landed, and Windows cannot open a directory to sync it.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = fs::File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn leftovers(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".tmp."))
            .collect()
    }

    #[test]
    fn creates_and_replaces() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a/b/note.md");
        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"two");
        assert!(leftovers(path.parent().unwrap()).is_empty());
    }

    /// The old rollback wrote through `note.tmp` for `note.md`, which clobbered
    /// a real file of that name and then renamed it away.
    #[test]
    fn leaves_a_sibling_with_the_temp_name_alone() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("note.tmp"), b"a real note").unwrap();
        write_atomic(&dir.path().join("note.md"), b"new").unwrap();
        assert_eq!(
            fs::read(dir.path().join("note.tmp")).unwrap(),
            b"a real note"
        );
    }

    /// Renaming over a directory fails, and the temp file must not outlive
    /// the failure.
    #[test]
    fn cleans_up_after_a_failed_rename() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("taken");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("inside"), b"x").unwrap();
        assert!(write_atomic(&path, b"new").is_err());
        assert!(
            leftovers(dir.path()).is_empty(),
            "{:?}",
            leftovers(dir.path())
        );
    }

    #[cfg(unix)]
    #[test]
    fn keeps_the_permissions_of_the_file_it_replaces() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("private.md");
        fs::write(&path, b"old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
