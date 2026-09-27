//! The note APIs reach exactly the files under the vault root that are not
//! protected, judged by where a path lands on disk rather than by its text.
//!
//! Both ways out of that were the same mistake. The resolver checked a path's
//! text, then handed the text back to be opened, and the operating system does
//! not open text. It follows symlinks, and on the default macOS and Windows
//! filesystems it ignores case. So a symlink inside the vault turned creating a
//! note into writing anywhere on the machine, and `.Git/hooks/post-commit` was
//! not `.git` to the check while being `.git` to the disk.

use std::path::{Path, PathBuf};
use tempfile::TempDir;
use turbovault_core::Precondition;
use turbovault_vault::{Error, ServerConfig, VaultConfig, VaultManager};

/// A vault, and a directory beside it standing in for the rest of the machine.
struct Fixture {
    _root: TempDir,
    vault: PathBuf,
    outside: PathBuf,
    manager: VaultManager,
}

fn fixture() -> Fixture {
    let root = TempDir::new().unwrap();
    let vault = root.path().join("vault");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&vault).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let mut config = ServerConfig::new();
    config
        .vaults
        .push(VaultConfig::builder("test", &vault).build().unwrap());
    let manager = VaultManager::new(config).unwrap();
    Fixture {
        _root: root,
        vault,
        outside,
        manager,
    }
}

fn assert_traversal(result: turbovault_vault::Result<PathBuf>, path: &str) {
    match result {
        Err(Error::PathTraversalAttempt { .. }) => {}
        other => panic!("{path:?} should be refused as leaving the vault, got {other:?}"),
    }
}

fn assert_protected(result: turbovault_vault::Result<PathBuf>, path: &str) {
    match result {
        Err(Error::ProtectedPath { .. }) => {}
        other => panic!("{path:?} should be refused as protected, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Symlinks
// ---------------------------------------------------------------------------

/// The reported case. The target does not exist yet, which is exactly when the
/// old resolver stopped consulting the filesystem.
#[cfg(unix)]
#[tokio::test]
async fn a_new_note_under_a_symlink_to_outside_the_vault_is_refused() {
    let f = fixture();
    std::os::unix::fs::symlink(&f.outside, f.vault.join("Attachments")).unwrap();

    assert_traversal(
        f.manager
            .resolve_path(Path::new("Attachments/authorized_keys")),
        "Attachments/authorized_keys",
    );
    let written = f
        .manager
        .write_file(
            Path::new("Attachments/authorized_keys"),
            "ssh-rsa AAAA",
            Precondition::ExpectAbsent,
            "test",
        )
        .await;
    assert!(written.is_err(), "the write went through: {written:?}");
    assert!(
        !f.outside.join("authorized_keys").exists(),
        "a file was created outside the vault"
    );
}

/// Several directories deep under the link, none of which exist.
#[cfg(unix)]
#[test]
fn a_new_nested_path_under_a_symlink_to_outside_the_vault_is_refused() {
    let f = fixture();
    std::os::unix::fs::symlink(&f.outside, f.vault.join("linked")).unwrap();
    assert_traversal(
        f.manager.resolve_path(Path::new("linked/a/b/c.md")),
        "linked/a/b/c.md",
    );
}

/// This one already held, because an existing target could be canonicalized.
/// It stays here so the fix for the other case cannot regress it.
#[cfg(unix)]
#[test]
fn an_existing_file_under_a_symlink_to_outside_the_vault_is_refused() {
    let f = fixture();
    std::fs::write(f.outside.join("secret.md"), "secret").unwrap();
    std::os::unix::fs::symlink(&f.outside, f.vault.join("linked")).unwrap();
    assert_traversal(
        f.manager.resolve_path(Path::new("linked/secret.md")),
        "linked/secret.md",
    );
}

/// A link whose target does not exist yet. Writing through it creates the
/// target, so it has to be judged by where it points.
#[cfg(unix)]
#[test]
fn a_dangling_symlink_to_outside_the_vault_is_refused() {
    let f = fixture();
    std::os::unix::fs::symlink(f.outside.join("new.md"), f.vault.join("evil.md")).unwrap();
    assert_traversal(f.manager.resolve_path(Path::new("evil.md")), "evil.md");
}

/// A link that stays inside the vault is an ordinary thing to have, and still
/// works.
#[cfg(unix)]
#[test]
fn a_symlink_that_stays_inside_the_vault_is_followed() {
    let f = fixture();
    std::fs::create_dir_all(f.vault.join("real")).unwrap();
    std::os::unix::fs::symlink(f.vault.join("real"), f.vault.join("alias")).unwrap();
    assert!(f.manager.resolve_path(Path::new("alias/new.md")).is_ok());
}

/// Staying inside the vault is not enough: a link to the vault's own `.git`
/// reaches the hooks directory without the path ever saying `.git`.
#[cfg(unix)]
#[test]
fn a_symlink_into_a_protected_directory_is_refused() {
    let f = fixture();
    std::fs::create_dir_all(f.vault.join(".git/hooks")).unwrap();
    std::os::unix::fs::symlink(f.vault.join(".git"), f.vault.join("notes")).unwrap();
    assert_protected(
        f.manager.resolve_path(Path::new("notes/hooks/post-commit")),
        "notes/hooks/post-commit",
    );
}

/// A vault registered through a symlinked path (`/tmp` is one on macOS) must
/// not have every note judged to be outside itself.
#[cfg(unix)]
#[test]
fn a_vault_reached_through_a_symlink_still_resolves_its_own_notes() {
    let root = TempDir::new().unwrap();
    let real = root.path().join("real-vault");
    std::fs::create_dir_all(&real).unwrap();
    let linked = root.path().join("linked-vault");
    std::os::unix::fs::symlink(&real, &linked).unwrap();

    let mut config = ServerConfig::new();
    config
        .vaults
        .push(VaultConfig::builder("test", &linked).build().unwrap());
    let manager = VaultManager::new(config).unwrap();

    assert_eq!(
        manager.resolve_path(Path::new("a/b.md")).unwrap(),
        linked.join("a/b.md")
    );
}

// ---------------------------------------------------------------------------
// Protected directories
// ---------------------------------------------------------------------------

/// On a case-insensitive filesystem these are the protected directories. The
/// check has to refuse them everywhere, since it cannot know which kind of
/// filesystem it is on for every component.
#[test]
fn a_protected_directory_is_refused_in_any_case() {
    let f = fixture();
    for path in [
        ".TurboVault/audit/operations.jsonl",
        ".TURBOVAULT/x.md",
        ".Git/hooks/post-commit",
        ".OBSIDIAN/plugins/p/main.js",
        "sub/Node_Modules/x.md",
    ] {
        assert_protected(f.manager.resolve_path(Path::new(path)), path);
    }
}

/// Windows drops trailing dots and spaces from a name, so `.git.` is `.git`.
#[test]
fn a_protected_directory_is_refused_with_a_trailing_dot_or_space() {
    let f = fixture();
    for path in [".git./hooks/x", ".turbovault /x.md", ".obsidian../x.md"] {
        assert_protected(f.manager.resolve_path(Path::new(path)), path);
    }
}

/// Only whole components are protected. A note merely named like one is fine.
#[test]
fn a_name_that_only_contains_a_protected_name_is_allowed() {
    let f = fixture();
    for path in [
        "notes/.gitignore.md",
        "my.obsidian.md",
        "turbovault/notes.md",
    ] {
        assert!(
            f.manager.resolve_path(Path::new(path)).is_ok(),
            "{path:?} was refused"
        );
    }
}

// ---------------------------------------------------------------------------
// Plain paths
// ---------------------------------------------------------------------------

#[test]
fn a_parent_component_that_leaves_the_vault_is_refused() {
    let f = fixture();
    for path in [
        "../outside/x.md",
        "a/../../outside/x.md",
        "../../etc/passwd",
    ] {
        assert_traversal(f.manager.resolve_path(Path::new(path)), path);
    }
}

/// `..` that stays inside is resolved rather than refused, and the result is
/// the path it names, so two spellings of one note are one path.
#[test]
fn a_parent_component_that_stays_inside_is_resolved() {
    let f = fixture();
    assert_eq!(
        f.manager.resolve_path(Path::new("a/../b.md")).unwrap(),
        f.vault.join("b.md")
    );
}

#[test]
fn an_absolute_path_outside_the_vault_is_refused() {
    let f = fixture();
    let path = f.outside.join("x.md");
    assert_traversal(f.manager.resolve_path(&path), "absolute outside path");
}
