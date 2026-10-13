//! A tantivy directory for a search index that is a cache.
//!
//! Tantivy makes every commit durable: each new file is synced, `meta.json`
//! is synced before it replaces the old one, and the directory is synced.
//! On macOS that is tens of milliseconds per commit, and the search index
//! commits on every write to the vault, so it was most of what a write cost.
//!
//! The persisted search index is rebuilt from the vault whenever it can't be
//! trusted, so it needs no durability: a crashed process loses nothing (its
//! writes are in the page cache), and what a power loss could leave half
//! written is caught by tantivy's own checksums when the index is next
//! opened, and rebuilt. This directory is [`MmapDirectory`] with the syncs
//! left out.

use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tantivy::directory::error::{DeleteError, LockError, OpenReadError, OpenWriteError};
use tantivy::directory::{
    AntiCallToken, Directory, DirectoryLock, FileHandle, Lock, MmapDirectory, TerminatingWrite,
    WatchCallback, WatchHandle, WritePtr,
};

/// [`MmapDirectory`] without fsync. See the module docs.
#[derive(Clone, Debug)]
pub(crate) struct CacheDirectory {
    inner: MmapDirectory,
    root: PathBuf,
}

impl CacheDirectory {
    pub(crate) fn open(root: &Path) -> tantivy::Result<Self> {
        Ok(Self {
            inner: MmapDirectory::open(root)?,
            root: root.to_path_buf(),
        })
    }
}

/// A file written without being synced when it is finished.
struct UnsyncedFile(File);

impl Write for UnsyncedFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TerminatingWrite for UnsyncedFile {
    fn terminate_ref(&mut self, _: AntiCallToken) -> io::Result<()> {
        self.0.flush()
    }
}

impl Directory for CacheDirectory {
    fn get_file_handle(&self, path: &Path) -> Result<Arc<dyn FileHandle>, OpenReadError> {
        self.inner.get_file_handle(path)
    }

    fn delete(&self, path: &Path) -> Result<(), DeleteError> {
        self.inner.delete(path)
    }

    fn exists(&self, path: &Path) -> Result<bool, OpenReadError> {
        self.inner.exists(path)
    }

    fn open_write(&self, path: &Path) -> Result<WritePtr, OpenWriteError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.root.join(path))
            .map_err(|io_err| {
                if io_err.kind() == io::ErrorKind::AlreadyExists {
                    OpenWriteError::FileAlreadyExists(path.to_path_buf())
                } else {
                    OpenWriteError::wrap_io_error(io_err, path.to_path_buf())
                }
            })?;
        Ok(BufWriter::new(Box::new(UnsyncedFile(file))))
    }

    fn atomic_read(&self, path: &Path) -> Result<Vec<u8>, OpenReadError> {
        self.inner.atomic_read(path)
    }

    fn atomic_write(&self, path: &Path, data: &[u8]) -> io::Result<()> {
        // Written beside the target and renamed over it, so a reader never
        // sees half of it; not synced, as above.
        let (temp_path, mut file) = tempfile_in(&self.root)?;
        let written = file.write_all(data);
        drop(file);
        let result = written.and_then(|()| std::fs::rename(&temp_path, self.root.join(path)));
        if result.is_err() {
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    fn sync_directory(&self) -> io::Result<()> {
        Ok(())
    }

    fn acquire_lock(&self, lock: &Lock) -> Result<DirectoryLock, LockError> {
        self.inner.acquire_lock(lock)
    }

    fn watch(&self, watch_callback: WatchCallback) -> tantivy::Result<WatchHandle> {
        self.inner.watch(watch_callback)
    }
}

/// A new file in `dir` with a name no other writer will pick.
fn tempfile_in(dir: &Path) -> io::Result<(PathBuf, File)> {
    loop {
        let path = dir.join(format!(".tmp-{}", uuid::Uuid::new_v4().simple()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}
