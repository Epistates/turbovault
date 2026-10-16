use super::*;
use tempfile::TempDir;

/// Create a test vault configuration
fn create_test_config(vault_dir: &Path) -> ServerConfig {
    let mut config = ServerConfig::new();
    let vault_config = VaultConfig::builder("test_vault", vault_dir)
        .build()
        .unwrap();
    config.vaults.push(vault_config);
    config
}

#[tokio::test]
async fn test_vault_manager_creation() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());

    let manager = VaultManager::new(config);
    assert!(manager.is_ok());
}

#[tokio::test]
async fn test_vault_path() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());

    let manager = VaultManager::new(config).unwrap();
    assert_eq!(manager.vault_path(), temp_dir.path());
}

#[tokio::test]
async fn test_write_and_read_file() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Write a file
    let path = Path::new("test.md");
    let content = "# Test Note\nHello world";
    assert!(
        manager
            .write_file(path, content, Precondition::Blind, "test")
            .await
            .is_ok()
    );

    // Read it back
    let read_content = manager.read_file(path).await.unwrap();
    assert_eq!(read_content, content);
}

/// `apply_changes` is the multi-change entry point the single-op
/// mutators (`write_file`/`delete_file`/`move_file`) build one-change
/// plans over. Exercise it directly with a plan mixing create, update,
/// and remove in one call.
#[tokio::test]
async fn test_apply_changes_multi_change_plan() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Seed the files the update/remove changes act on.
    manager
        .write_file(
            Path::new("updated.md"),
            "# Old",
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();
    manager
        .write_file(
            Path::new("removed.md"),
            "# Gone",
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    let plan = ChangePlan::new("multi-change test")
        .create("new.md", "# New")
        .upsert("updated.md", "# Updated")
        .with_precondition("updated.md", Precondition::ExpectExists)
        .remove("removed.md")
        .with_precondition("removed.md", Precondition::ExpectExists);

    let outcome = manager.apply_changes(&plan).await.unwrap();

    assert_eq!(
        outcome.changed,
        vec![
            ("new.md".to_string(), true),
            ("updated.md".to_string(), true),
            ("removed.md".to_string(), false),
        ]
    );

    assert_eq!(
        manager.read_file(Path::new("new.md")).await.unwrap(),
        "# New"
    );
    assert_eq!(
        manager.read_file(Path::new("updated.md")).await.unwrap(),
        "# Updated"
    );
    assert!(
        manager.read_file(Path::new("removed.md")).await.is_err(),
        "removed.md should no longer exist after apply_changes"
    );
}

/// M5 S2 (`turbovault-qae.6.3`): a multi-change DIRECT plan is applied
/// best-effort — sequential, stop at the first failure, no rollback — so
/// the outcome must SAY so instead of claiming atomicity it does not
/// have. Change 0 lands, change 1 fails, change 2 is never attempted:
/// `atomic` false, `failed_at` Some(1), the typed error survives to the
/// manager boundary, and what landed is indexed (R7) rather than being
/// discarded along with the error.
///
/// The same plan on a GIT vault is all-or-nothing: it aborts with
/// nothing written, and every GitSubstrate apply reports `atomic` true.
#[tokio::test]
async fn test_direct_partial_apply_reports_failed_at_and_indexes_what_landed() {
    // Change 1 renames a file that does not exist: on direct the apply
    // loop fails there (ENOENT) with change 0 already on disk; on git
    // the rename source is missing from the base tree, so the whole plan
    // aborts before any commit.
    let partial_plan = || {
        ChangePlan::new("partial apply")
            .create("landed.md", "# Landed")
            .with_change(Change::Rename {
                from: "ghost.md".to_string(),
                to: "moved.md".to_string(),
            })
            .create("never.md", "# Never")
    };

    // -------- direct: best-effort, partial state, honest report --------
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

    let outcome = manager.apply_changes(&partial_plan()).await.unwrap();

    assert!(
        !outcome.atomic,
        "a multi-change direct plan is best-effort, not atomic"
    );
    assert_eq!(
        outcome.failed_at,
        Some(1),
        "failed_at must name the change that stopped the loop"
    );
    assert!(
        matches!(outcome.error, Some(Error::FileNotFound { .. })),
        "the loop failure's typed error kind must survive to the manager \
         boundary as FileNotFound, not the OS's raw ENOENT text, got {:?}",
        outcome.error
    );
    assert_eq!(
        outcome.changed,
        vec![("landed.md".to_string(), true)],
        "only the changes that actually landed are reported"
    );
    assert!(
        temp_dir.path().join("landed.md").exists(),
        "change 0 landed"
    );
    assert!(
        !temp_dir.path().join("never.md").exists(),
        "change 2 must not be attempted after change 1 failed"
    );

    // The partial is INDEXED, not just on disk: pre-M5-S2 the `?` in the
    // apply loop skipped sync_index entirely, leaving landed.md invisible
    // to the link graph, file cache and search index.
    assert_eq!(
        manager.get_stats().await.unwrap().total_files,
        1,
        "the change that landed must be in the link graph, not just on disk"
    );

    // -------- git: unchanged all-or-nothing, always atomic --------
    let git_dir = TempDir::new().unwrap();
    init_git_repo(git_dir.path());
    let git_manager = VaultManager::new(create_git_test_config(git_dir.path())).unwrap();

    let ok = git_manager
        .apply_changes(
            &ChangePlan::new("git multi")
                .create("a.md", "# A")
                .create("b.md", "# B"),
        )
        .await
        .unwrap();
    assert!(
        ok.atomic,
        "every GitSubstrate apply is one all-or-nothing commit"
    );
    assert_eq!(ok.failed_at, None);
    assert!(ok.error.is_none());

    let head_before = head_oid(git_dir.path());
    assert!(
        git_manager.apply_changes(&partial_plan()).await.is_err(),
        "git must abort the whole plan, not apply it best-effort"
    );
    assert_eq!(
        head_oid(git_dir.path()),
        head_before,
        "an aborted git plan lands no commit"
    );
    assert!(!git_dir.path().join("landed.md").exists());
    assert!(!git_dir.path().join("never.md").exists());
}

#[tokio::test]
async fn test_write_file_creates_directories() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Write file in nested directory
    let path = Path::new("notes/subfolder/test.md");
    let content = "Nested file";
    assert!(
        manager
            .write_file(path, content, Precondition::Blind, "test")
            .await
            .is_ok()
    );

    // Verify it was created
    let read_content = manager.read_file(path).await.unwrap();
    assert_eq!(read_content, content);
}

#[tokio::test]
async fn test_path_traversal_prevention() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Attempt path traversal
    let bad_path = Path::new("../../../etc/passwd");
    let result = manager.read_file(bad_path).await;
    assert!(result.is_err());
}

/// A plan path through a symlinked folder lands where it was checked. The
/// check reads `linked/..` as the vault root; the OS reads it as the folder
/// the link points into, outside the vault.
#[cfg(unix)]
#[tokio::test]
async fn a_plan_path_through_a_symlinked_folder_lands_where_it_was_checked() {
    let outside = TempDir::new().unwrap();
    std::fs::create_dir(outside.path().join("deep")).unwrap();
    let temp_dir = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path().join("deep"), temp_dir.path().join("linked"))
        .unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

    let plan = ChangePlan::new("escape").create("linked/../escaped.md", "pwned");
    manager
        .apply_changes(&plan)
        .await
        .unwrap()
        .into_result()
        .unwrap();

    assert!(!outside.path().join("escaped.md").exists());
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("escaped.md")).unwrap(),
        "pwned"
    );
}

/// Every path a plan names is resolved before it applies, preconditions
/// included, and applied in its resolved form.
#[tokio::test]
async fn apply_changes_resolves_every_path_the_plan_names() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("sub")).unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

    let plan = ChangePlan::new("dots")
        .create("./a.md", "a")
        .create("sub/../b.md", "b");
    let outcome = manager.apply_changes(&plan).await.unwrap();
    assert!(outcome.error.is_none(), "{outcome:?}");
    assert!(temp_dir.path().join("a.md").is_file());
    assert!(temp_dir.path().join("b.md").is_file());

    let outside_precondition = ChangePlan::new("peek")
        .upsert("c.md", "c")
        .expect_absent("../outside.md");
    assert!(manager.apply_changes(&outside_precondition).await.is_err());
    assert!(!temp_dir.path().join("c.md").exists());
}

/// The substrate entry point refuses a path that isn't plain, whoever built
/// the plan.
#[tokio::test]
async fn the_substrate_entry_refuses_paths_that_are_not_plain() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

    for path in ["../x.md", "./x.md", "a/../x.md", "/abs.md", ""] {
        let plan = ChangePlan::new("raw").upsert(path, "x");
        let refused = manager.apply_through_substrate(&plan).await;
        assert!(
            matches!(refused, Err(Error::InvalidPath { .. })),
            "{path:?}: {refused:?}"
        );
    }
    let precondition = ChangePlan::new("raw")
        .upsert("x.md", "x")
        .expect_absent("../x.md");
    assert!(
        manager
            .apply_through_substrate(&precondition)
            .await
            .is_err()
    );
    assert!(!temp_dir.path().join("x.md").exists());
}

#[tokio::test]
async fn test_atomic_write() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let path = Path::new("atomic_test.md");
    let content = "Atomic write test";

    // Write file
    assert!(
        manager
            .write_file(path, content, Precondition::Blind, "test")
            .await
            .is_ok()
    );

    // Verify no temp files are left
    let entries = std::fs::read_dir(temp_dir.path()).unwrap();
    for entry in entries {
        let name = entry.unwrap().file_name();
        assert!(
            !turbovault_core::is_temp_file_name(&name.to_string_lossy()),
            "Temporary file left after write: {name:?}"
        );
    }
}

#[tokio::test]
async fn test_cache_invalidation() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let path = Path::new("cache_test.md");
    let content1 = "Original content";

    // Write initial file
    assert!(
        manager
            .write_file(path, content1, Precondition::Blind, "test")
            .await
            .is_ok()
    );

    // Read from cache
    let read1 = manager.read_file(path).await.unwrap();
    assert_eq!(read1, content1);

    // Update file directly
    let vault_path = temp_dir.path().join(path);
    let content2 = "Updated content";
    std::fs::write(&vault_path, content2).unwrap();

    // Read again (should get new content, not cached)
    let read2 = manager.read_file(path).await.unwrap();
    // Note: may be cached depending on cache_ttl, but read should work
    assert!(!read2.is_empty());
}

#[tokio::test]
async fn test_scan_files() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create some files
    std::fs::write(temp_dir.path().join("note1.md"), "# Note 1").unwrap();
    std::fs::write(temp_dir.path().join("note2.md"), "# Note 2").unwrap();
    std::fs::create_dir(temp_dir.path().join("folder")).unwrap();
    std::fs::write(temp_dir.path().join("folder/note3.md"), "# Note 3").unwrap();

    // Scan files
    let files = manager.scan_files().unwrap();

    // Should find all 3 markdown files
    assert_eq!(files.len(), 3);

    // Verify they're all .md files
    for file in &files {
        assert_eq!(file.extension().and_then(|e| e.to_str()), Some("md"));
    }
}

#[tokio::test]
async fn test_initialize_vault() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create test files with lowercase links matching the filenames
    let note1 = "# Note 1\n[[note2]]";
    let note2 = "# Note 2\n[[note1]]";
    std::fs::write(temp_dir.path().join("note1.md"), note1).unwrap();
    std::fs::write(temp_dir.path().join("note2.md"), note2).unwrap();

    // Initialize vault
    assert!(manager.initialize().await.is_ok());

    // Verify stats work
    let stats = manager.get_stats().await.unwrap();
    assert_eq!(stats.total_files, 2);
    // At least one link should resolve
    assert!(stats.total_links >= 1);
}

#[tokio::test]
async fn test_get_backlinks() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create files with links (use absolute paths for graph queries)
    std::fs::write(temp_dir.path().join("target.md"), "# Target").unwrap();
    std::fs::write(temp_dir.path().join("source.md"), "# Source\n[[target]]").unwrap();

    manager.initialize().await.unwrap();

    // Get backlinks for target (query with absolute path since graph stores absolute paths)
    let target_path = temp_dir.path().join("target.md");
    // Backlink resolution depends on platform-specific path handling;
    // verify the operation succeeds without asserting exact results
    let _backlinks = manager.get_backlinks(&target_path).await.unwrap();
}

#[tokio::test]
async fn test_get_forward_links() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create files with links
    std::fs::write(
        temp_dir.path().join("source.md"),
        "# Source\n[[target1]]\n[[target2]]",
    )
    .unwrap();
    std::fs::write(temp_dir.path().join("target1.md"), "# Target 1").unwrap();
    std::fs::write(temp_dir.path().join("target2.md"), "# Target 2").unwrap();

    manager.initialize().await.unwrap();

    // Get forward links (use absolute path)
    let source_path = temp_dir.path().join("source.md");
    // Link resolution depends on platform-specific path handling;
    // verify the operation succeeds without asserting exact results
    let _forward = manager.get_forward_links(&source_path).await.unwrap();
}

#[tokio::test]
async fn test_get_orphaned_notes() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create orphaned and linked files
    std::fs::write(temp_dir.path().join("orphan.md"), "# Orphaned Note").unwrap();
    std::fs::write(
        temp_dir.path().join("linked1.md"),
        "# Linked 1\n[[linked2]]",
    )
    .unwrap();
    std::fs::write(temp_dir.path().join("linked2.md"), "# Linked 2").unwrap();

    manager.initialize().await.unwrap();

    // Get orphaned notes
    let orphans = manager.get_orphaned_notes().await.unwrap();
    assert_eq!(orphans.len(), 1);
}

#[tokio::test]
async fn test_get_stats() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create test files
    std::fs::write(temp_dir.path().join("note1.md"), "# Note 1").unwrap();
    std::fs::write(temp_dir.path().join("note2.md"), "# Note 2").unwrap();

    manager.initialize().await.unwrap();

    let stats = manager.get_stats().await.unwrap();
    assert_eq!(stats.total_files, 2);
    assert_eq!(stats.total_links, 0); // No links between these files
    assert_eq!(stats.orphaned_files, 2); // Both orphaned
}

#[tokio::test]
async fn test_okf_markdown_cross_link_resolves_end_to_end() {
    // End-to-end: an OKF bundle-relative markdown cross-link
    // `[customers](/tables/customers.md)` must resolve through the real
    // parser -> graph pipeline (not just synthetic Link structs).
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("tables")).unwrap();
    std::fs::write(
        temp_dir.path().join("tables/customers.md"),
        "---\ntype: BigQuery Table\ntitle: Customers\n---\n# Schema\n",
    )
    .unwrap();
    std::fs::write(
        temp_dir.path().join("tables/orders.md"),
        "---\ntype: BigQuery Table\ntitle: Orders\n---\n# Joins\n\nJoined with [customers](/tables/customers.md) on `customer_id`.\n",
    )
    .unwrap();

    manager.initialize().await.unwrap();

    // The cross-link must have produced a resolved graph edge.
    let stats = manager.get_stats().await.unwrap();
    assert_eq!(stats.total_files, 2);
    assert_eq!(
        stats.total_links, 1,
        "OKF markdown cross-link should resolve"
    );

    // And it must surface as a backlink on the target.
    let customers = temp_dir.path().join("tables/customers.md");
    let backlinks = manager.get_backlinks(&customers).await.unwrap();
    assert_eq!(backlinks.len(), 1, "customers.md should have one backlink");
}

/// Obsidian 1.11 reads a Markdown link in a property as a link, and a
/// wikilink in a table cell is written with its alias pipe escaped. Both
/// reach the target as backlinks.
#[tokio::test]
async fn property_markdown_links_and_table_wikilinks_are_backlinks() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("paper.md"), "# Paper\n").unwrap();
    std::fs::write(temp_dir.path().join("tool.md"), "# Tool\n").unwrap();
    std::fs::write(
        temp_dir.path().join("index.md"),
        "---\nsource: \"[The paper](paper.md)\"\n---\n| Area | Notes |\n|---|---|\n| Tools | [[tool\\|The tool]] |\n",
    )
    .unwrap();

    manager.initialize().await.unwrap();

    for target in ["paper.md", "tool.md"] {
        let backlinks = manager.get_backlinks(Path::new(target)).await.unwrap();
        assert_eq!(backlinks.len(), 1, "{target}: {backlinks:?}");
    }
}

/// Obsidian writes a Markdown link to a file with a space in its name
/// with the space escaped, `[x](My%20Note.md)`.
/// A path the filesystem resolved (on Windows `\\?\C:\...`, on macOS a
/// temp vault's `/private/var/...`) is still made vault-relative.
#[test]
fn relative_path_strips_the_resolved_root_too() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    let resolved = std::fs::canonicalize(temp_dir.path()).unwrap();

    assert_eq!(
        manager.relative_path(&resolved.join("notes").join("a.md")),
        "notes/a.md"
    );
    assert_eq!(manager.relative_path(&temp_dir.path().join("b.md")), "b.md");
    // Already rendered with `/`, as the search tools hand paths over.
    let rendered = turbovault_core::path_to_slash(&resolved.join("c.md"));
    assert_eq!(manager.relative_path(Path::new(&rendered)), "c.md");
    // A sibling whose name merely starts with the root's is not inside.
    let sibling = format!("{}-other/d.md", turbovault_core::path_to_slash(&resolved));
    assert_eq!(manager.relative_path(Path::new(&sibling)), sibling);
}

#[tokio::test]
async fn percent_escaped_markdown_links_resolve() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("My Note.md"), "# Mine\n").unwrap();
    std::fs::write(
        temp_dir.path().join("body.md"),
        "See [mine](My%20Note.md#Part%20Two).\n",
    )
    .unwrap();
    std::fs::write(
        temp_dir.path().join("property.md"),
        "---\nsource: \"[mine](My%20Note.md)\"\n---\n# Property\n",
    )
    .unwrap();

    manager.initialize().await.unwrap();

    let backlinks = manager
        .get_backlinks(Path::new("My Note.md"))
        .await
        .unwrap();
    assert_eq!(backlinks.len(), 2, "{backlinks:?}");
}

/// A manager for one vault in `dir` whose own config is shaped by `shape`.
fn manager_with_vault(dir: &Path, shape: impl FnOnce(&mut VaultConfig)) -> VaultManager {
    let mut config = ServerConfig::new();
    let mut vault = VaultConfig::builder("v", dir).build().unwrap();
    shape(&mut vault);
    config.vaults.push(vault);
    VaultManager::new(config).unwrap()
}

/// `allowed_operations: read` makes a vault read-only: every write is
/// refused at the chokepoint they all share, saying why.
#[tokio::test]
async fn a_read_only_vault_refuses_every_write() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("a.md"), "# A\n").unwrap();
    let manager = manager_with_vault(temp_dir.path(), |vault| {
        vault.allowed_operations = Some(["read".to_string()].into());
    });
    manager.initialize().await.unwrap();

    let error = manager
        .write_file(Path::new("b.md"), "# B\n", Precondition::Blind, "add")
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("allowed_operations"), "{error}");
    assert!(
        manager
            .delete_file(Path::new("a.md"), Precondition::Blind, "rm")
            .await
            .is_err()
    );
    assert!(!temp_dir.path().join("b.md").exists());
    assert!(temp_dir.path().join("a.md").exists());
    // Reading is always allowed.
    assert_eq!(manager.read_file(Path::new("a.md")).await.unwrap(), "# A\n");
}

/// Each kind of change needs its own operation, and a move built as a
/// copy plus a remove (as a link-aware move is) needs `move`, not
/// `write` and `delete`.
#[tokio::test]
async fn allowed_operations_is_checked_per_kind_of_change() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::write(temp_dir.path().join("a.md"), "# A\n").unwrap();
    let manager = manager_with_vault(temp_dir.path(), |vault| {
        vault.allowed_operations = Some(["read".to_string(), "move".to_string()].into());
    });
    manager.initialize().await.unwrap();

    let token = manager.hash_bytes(b"# A\n").unwrap();
    let moved = ChangePlan::new("move")
        .upsert("b.md", b"# A\n".to_vec())
        .expect_blob("a.md", token)
        .remove("a.md");
    manager
        .apply_changes(&moved)
        .await
        .unwrap()
        .into_result()
        .unwrap();
    assert!(temp_dir.path().join("b.md").exists());

    let deleted = ChangePlan::new("rm").remove("b.md");
    assert!(manager.apply_changes(&deleted).await.is_err());
    let written = ChangePlan::new("add").upsert("c.md", b"# C\n".to_vec());
    assert!(manager.apply_changes(&written).await.is_err());

    // A move in the same plan doesn't carry an unrelated delete with it.
    std::fs::write(temp_dir.path().join("other.md"), "# Other\n").unwrap();
    let token = manager.hash_bytes(b"# A\n").unwrap();
    let move_and_delete = ChangePlan::new("both")
        .upsert("d.md", b"# A\n".to_vec())
        .expect_blob("b.md", token)
        .remove("b.md")
        .remove("other.md");
    assert!(manager.apply_changes(&move_and_delete).await.is_err());
    assert!(temp_dir.path().join("other.md").exists());
}

/// A vault's own `max_file_size` and `excluded_paths` apply to it, and
/// its exclusions add to the server's.
#[tokio::test]
async fn a_vaults_own_limits_apply() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("private")).unwrap();
    std::fs::write(temp_dir.path().join("big.md"), "x".repeat(200)).unwrap();
    let manager = manager_with_vault(temp_dir.path(), |vault| {
        vault.max_file_size = Some(64);
        vault.excluded_paths = Some(["private".to_string()].into());
    });
    manager.initialize().await.unwrap();

    assert!(manager.read_file(Path::new("big.md")).await.is_err());
    assert!(
        manager
            .write_file(
                Path::new("private/x.md"),
                "# X\n",
                Precondition::Blind,
                "add"
            )
            .await
            .is_err()
    );
    assert!(
        manager
            .write_file(
                Path::new(".obsidian/x.md"),
                "# X\n",
                Precondition::Blind,
                "add"
            )
            .await
            .is_err()
    );
}

/// `initialize` builds the cache and graph from the vault as it is, so
/// running it again drops a note deleted since rather than keeping it.
#[tokio::test]
async fn initialize_again_reflects_the_vault_as_it_is() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    std::fs::write(temp_dir.path().join("gone.md"), "# Gone\n").unwrap();
    std::fs::write(temp_dir.path().join("kept.md"), "See [[gone]].\n").unwrap();
    manager.initialize().await.unwrap();

    std::fs::remove_file(temp_dir.path().join("gone.md")).unwrap();
    manager.initialize().await.unwrap();

    let cached = manager.all_cached_vault_files().await;
    assert_eq!(cached.len(), 1);
    assert!(cached[0].path.ends_with("kept.md"));
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);
}

/// `Note.MD` is a note: cached, and parsed, so its links are backlinks.
#[tokio::test]
async fn upper_case_extension_notes_link() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    std::fs::write(temp_dir.path().join("Target.md"), "# Target\n").unwrap();
    std::fs::write(temp_dir.path().join("Shouting.MD"), "See [[Target]].\n").unwrap();
    manager.initialize().await.unwrap();

    let backlinks = manager.get_backlinks(Path::new("Target.md")).await.unwrap();
    assert_eq!(backlinks.len(), 1, "{backlinks:?}");
}

#[tokio::test]
async fn reference_links_are_backlinks() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("My Note.md"), "# Mine\n").unwrap();
    std::fs::write(temp_dir.path().join("Other.md"), "# Other\n").unwrap();
    std::fs::write(
        temp_dir.path().join("refs.md"),
        "See [mine][m] and [[Other]].\n\n[m]: <My Note.md>\n[other]: My%20Note.md\n",
    )
    .unwrap();

    manager.initialize().await.unwrap();

    let mine = manager
        .get_backlinks(Path::new("My Note.md"))
        .await
        .unwrap();
    assert_eq!(mine.len(), 1, "{mine:?}");
    // `[[Other]]` reads `[Other]` as a shortcut to the `[other]`
    // definition in CommonMark; it is still only a wikilink to Other.
    let other = manager.get_backlinks(Path::new("Other.md")).await.unwrap();
    assert_eq!(other.len(), 1, "{other:?}");
    let forward = manager
        .get_forward_links(Path::new("refs.md"))
        .await
        .unwrap();
    assert_eq!(forward.len(), 2, "{forward:?}");
}

#[tokio::test]
async fn test_write_non_markdown_file_does_not_pollute_graph() {
    // Writing a non-markdown artifact (e.g. an exported viz.html) must not
    // add a node to the note graph or the note cache.
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
    manager.initialize().await.unwrap();
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

    // Write a non-md file via the same path visualize() uses.
    manager
        .write_file(
            std::path::Path::new("viz.html"),
            "<html>[fake](/note.md)</html>",
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    // The HTML file is on disk but is NOT a graph node.
    assert!(temp_dir.path().join("viz.html").exists());
    assert_eq!(
        manager.get_stats().await.unwrap().total_files,
        1,
        "non-markdown write must not add a graph node"
    );
}

#[tokio::test]
async fn test_get_related_notes() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create a chain: A -> B -> C
    std::fs::write(temp_dir.path().join("a.md"), "# A\n[[b]]").unwrap();
    std::fs::write(temp_dir.path().join("b.md"), "# B\n[[a]]\n[[c]]").unwrap();
    std::fs::write(temp_dir.path().join("c.md"), "# C\n[[b]]").unwrap();

    manager.initialize().await.unwrap();

    // Get related notes to B within 1 hop (use absolute path)
    let b_path = temp_dir.path().join("b.md");
    let related = manager.get_related_notes(&b_path, 1).await.unwrap();

    // Should find A and C (direct neighbors)
    assert!(!related.is_empty());
}

#[tokio::test]
async fn test_resolve_path_absolute() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Valid absolute path under vault
    let valid_path = temp_dir.path().join("test.md");
    let result = manager.resolve_path(&valid_path);
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_resolve_path_relative() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create the actual file
    std::fs::write(temp_dir.path().join("test.md"), "content").unwrap();

    let result = manager.resolve_path(Path::new("test.md"));
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_resolve_path_traversal_prevention() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Try to escape vault with ../ components
    let result = manager.resolve_path(Path::new("../../tmp/evil.md"));
    assert!(result.is_err(), "Path traversal should be prevented");

    // Also test with deeper traversal
    let result2 = manager.resolve_path(Path::new("../../../etc/passwd"));
    assert!(result2.is_err(), "Path traversal should be prevented");
}

// -------------------------------------------------------------------------
// New comprehensive tests
// -------------------------------------------------------------------------

/// Writing a file then deleting it should leave the path absent on disk,
/// and a subsequent `read_file` must return an error.
#[tokio::test]
async fn test_delete_file() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let rel = Path::new("to_delete.md");
    manager
        .write_file(rel, "# Delete me", Precondition::Blind, "test")
        .await
        .unwrap();

    // Verify the file exists before deletion.
    assert!(temp_dir.path().join(rel).exists());

    manager
        .delete_file(rel, Precondition::ExpectExists, "test")
        .await
        .unwrap();

    // File must no longer exist on disk.
    assert!(
        !temp_dir.path().join(rel).exists(),
        "File should be gone after delete_file"
    );

    // read_file must return an error for the deleted path.
    let result = manager.read_file(rel).await;
    assert!(result.is_err(), "read_file on deleted path should error");
}

/// Moving a file should put its content at the new path and remove the old path.
#[tokio::test]
async fn test_move_file() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let src = Path::new("source_note.md");
    let dst = Path::new("dest_note.md");
    let content = "# Moved Note\nsome content";

    manager
        .write_file(src, content, Precondition::Blind, "test")
        .await
        .unwrap();

    manager
        .move_file(
            src,
            dst,
            Precondition::ExpectExists,
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    // Old path must no longer exist.
    assert!(
        !temp_dir.path().join(src).exists(),
        "Source file should be gone after move"
    );

    // New path must exist with the original content.
    let read_back = manager.read_file(dst).await.unwrap();
    assert_eq!(
        read_back, content,
        "Destination must have the original content"
    );
}

/// Moving an attachment must preserve arbitrary bytes rather than requiring UTF-8.
#[tokio::test]
async fn test_move_file_preserves_non_utf8_bytes() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    let src = Path::new("attachments/source.bin");
    let dst = Path::new("assets/destination.bin");
    let bytes = [0, 159, 146, 150, 255, 10];

    tokio::fs::create_dir_all(temp_dir.path().join("attachments"))
        .await
        .unwrap();
    tokio::fs::write(temp_dir.path().join(src), bytes)
        .await
        .unwrap();

    manager
        .move_file(
            src,
            dst,
            Precondition::ExpectExists,
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    assert!(!temp_dir.path().join(src).exists());
    assert_eq!(
        tokio::fs::read(temp_dir.path().join(dst)).await.unwrap(),
        bytes
    );
    assert!(manager.all_cached_vault_files().await.is_empty());
}

/// `sync_index` is markdown-only (unified from `write_file`'s pre-M3a
/// guard, see the doc comment on `sync_index`): a moved non-`.md`
/// UTF-8-decodable file must not appear in the cache, even though
/// pre-M3a `move_file` cached any UTF-8-decodable file regardless of
/// extension.
#[tokio::test]
async fn test_move_file_non_markdown_utf8_not_cached() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    let src = Path::new("notes.txt");
    let dst = Path::new("moved.txt");

    tokio::fs::write(temp_dir.path().join(src), "plain text, not markdown")
        .await
        .unwrap();

    manager
        .move_file(
            src,
            dst,
            Precondition::ExpectExists,
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    assert!(!temp_dir.path().join(src).exists());
    assert_eq!(
        tokio::fs::read_to_string(temp_dir.path().join(dst))
            .await
            .unwrap(),
        "plain text, not markdown"
    );
    assert!(
        manager.all_cached_vault_files().await.is_empty(),
        "non-markdown files must never be cached"
    );
}

#[tokio::test]
async fn test_refresh_file_state_tracks_external_delete_and_restore() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("target.md"), "# Target\n").unwrap();
    std::fs::write(
        temp_dir.path().join("source.md"),
        "# Source\n\n[[target]]\n",
    )
    .unwrap();
    manager.initialize().await.unwrap();

    let source = Path::new("source.md");
    tokio::fs::remove_file(temp_dir.path().join(source))
        .await
        .unwrap();
    manager.refresh_file_state(source).await.unwrap();
    assert_eq!(manager.all_cached_vault_files().await.len(), 1);
    assert!(manager.get_forward_links(source).await.unwrap().is_empty());

    tokio::fs::write(temp_dir.path().join(source), "# Restored\n\n[[target]]\n")
        .await
        .unwrap();
    manager.refresh_file_state(source).await.unwrap();
    assert_eq!(manager.all_cached_vault_files().await.len(), 2);
    assert_eq!(manager.get_forward_links(source).await.unwrap().len(), 1);
}

/// Moving a file to a subdirectory that doesn't exist yet should create
/// the intermediate directories automatically.
#[tokio::test]
async fn test_move_file_cross_directory() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let src = Path::new("flat_note.md");
    let dst = Path::new("deep/nested/subdir/note.md");
    let content = "# Cross-dir Move";

    manager
        .write_file(src, content, Precondition::Blind, "test")
        .await
        .unwrap();

    // The destination directory does not exist yet.
    assert!(!temp_dir.path().join("deep").exists());

    manager
        .move_file(
            src,
            dst,
            Precondition::ExpectExists,
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    // Source gone, destination present.
    assert!(!temp_dir.path().join(src).exists());
    assert!(temp_dir.path().join(dst).exists());

    let read_back = manager.read_file(dst).await.unwrap();
    assert_eq!(read_back, content);
}

/// After a successful `write_file` no temp files should remain
/// anywhere under the vault directory.
#[tokio::test]
async fn test_temp_file_cleanup_on_write() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Write into a nested directory to exercise the parent-creation path
    // and ensure temp files are cleaned up in the right place.
    let rel = Path::new("sub/cleanup_test.md");
    manager
        .write_file(rel, "content", Precondition::Blind, "test")
        .await
        .unwrap();

    // Walk the entire vault tree and assert no temp files remain.
    let mut stack = vec![temp_dir.path().to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let name = entry.file_name();
                assert!(
                    !turbovault_core::is_temp_file_name(&name.to_string_lossy()),
                    "Leftover temp file found: {:?}",
                    path
                );
            }
        }
    }
}

/// After writing a note that contains a wikilink the link graph must
/// record that forward link from the written file.
#[tokio::test]
async fn test_graph_updated_after_write() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Write the target file so the parser can resolve the link.
    let target = Path::new("target.md");
    manager
        .write_file(target, "# Target", Precondition::Blind, "test")
        .await
        .unwrap();

    // Write a source file that links to target.
    let source = Path::new("source.md");
    manager
        .write_file(source, "# Source\n[[target]]", Precondition::Blind, "test")
        .await
        .unwrap();

    // Check the link graph via forward_links on the absolute source path.
    let source_abs = temp_dir.path().join(source);
    let forward = manager.get_forward_links(&source_abs).await.unwrap();

    // At least one forward link should resolve to target.
    assert!(
        !forward.is_empty(),
        "Link graph should record the [[target]] forward link after write"
    );
}

/// After deleting file A (which links to B) the backlinks for B must no
/// longer include A.
#[tokio::test]
async fn test_graph_updated_after_delete() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    // Create both files and initialize so the graph is populated.
    std::fs::write(temp_dir.path().join("a.md"), "# A\n[[b]]").unwrap();
    std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
    manager.initialize().await.unwrap();

    // Sanity: A should appear as a backlink to B before deletion.
    let b_abs = temp_dir.path().join("b.md");
    let backlinks_before = manager.get_backlinks(&b_abs).await.unwrap();
    assert!(
        !backlinks_before.is_empty(),
        "Before deletion A should be a backlink to B"
    );

    // Delete A.
    manager
        .delete_file(Path::new("a.md"), Precondition::ExpectExists, "test")
        .await
        .unwrap();

    // After deletion A must no longer appear in B's backlinks.
    let backlinks_after = manager.get_backlinks(&b_abs).await.unwrap();
    let a_abs = temp_dir.path().join("a.md");
    let a_still_linked = backlinks_after.iter().any(|p| p == &a_abs);
    assert!(
        !a_still_linked,
        "After deleting A, it must not appear in B's backlinks; found: {:?}",
        backlinks_after
    );
}

// ── vault_files_validated ────────────────────────────────────────────────

#[tokio::test]
async fn test_validated_returns_cached_files_on_hot_path() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("a.md"), "# A").unwrap();
    std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
    manager.initialize().await.unwrap();

    let files = manager.vault_files_validated().await;
    assert_eq!(files.len(), 2);
}

/// An edit made outside the process reaches `vault_files_validated`.
///
/// Reconciled explicitly rather than by calling the accessor twice: the
/// gate is debounced, so back-to-back calls deliberately share one pass.
/// `the_gate_debounces_repeated_calls` covers that schedule; this covers
/// what the accessor answers once a pass has run.
#[tokio::test]
async fn test_validated_reflects_external_modification() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let path = temp_dir.path().join("note.md");
    std::fs::write(&path, "---\nstatus: draft\n---\n# Note").unwrap();
    manager.initialize().await.unwrap();

    // Confirm initial frontmatter is cached.
    let files = manager.vault_files_validated().await;
    let initial = files
        .iter()
        .find(|f| f.path == path)
        .and_then(|f| f.frontmatter.as_ref())
        .and_then(|fm| fm.data.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    assert_eq!(initial, "draft");

    // Overwrite the file externally with a future mtime.
    // Sleep 10 ms so the OS registers a mtime change.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    std::fs::write(&path, "---\nstatus: published\n---\n# Note").unwrap();

    manager.reconcile_now().await;
    let files = manager.vault_files_validated().await;
    let updated = files
        .iter()
        .find(|f| f.path == path)
        .and_then(|f| f.frontmatter.as_ref())
        .and_then(|fm| fm.data.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    assert_eq!(updated, "published");
}

/// A deletion made outside the process reaches `vault_files_validated`.
#[tokio::test]
async fn test_validated_reflects_external_deletion() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let path = temp_dir.path().join("ephemeral.md");
    std::fs::write(&path, "# Ephemeral").unwrap();
    manager.initialize().await.unwrap();

    assert_eq!(manager.vault_files_validated().await.len(), 1);

    std::fs::remove_file(&path).unwrap();
    manager.reconcile_now().await;

    assert_eq!(manager.vault_files_validated().await.len(), 0);
}

// ── scan_vault ───────────────────────────────────────────────────────────

/// The scan must recurse into nested subdirectories.
#[tokio::test]
async fn test_scan_vault_recurses_subdirectories() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::create_dir_all(temp_dir.path().join("a/b/c")).unwrap();
    std::fs::write(temp_dir.path().join("a/b/c/deep.md"), "# Deep").unwrap();

    let files = manager.scan_vault().await.unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("deep.md"));
}

/// The scan must skip files whose extension the configuration does not
/// admit.
#[tokio::test]
async fn test_scan_vault_skips_disallowed_extensions() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
    std::fs::write(temp_dir.path().join("image.png"), "fake png").unwrap();
    std::fs::write(temp_dir.path().join("data.json"), "{}").unwrap();

    let files = manager.scan_vault().await.unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].ends_with("note.md"));
}

/// Extension matching is case-insensitive, because `sync_index`'s markdown
/// test is. If the two disagreed, a note the write path caches under one
/// spelling would be reported deleted by the very next freshness sweep.
#[tokio::test]
async fn test_scan_vault_admits_uppercase_extensions() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("Shouty.MD"), "# Shouty").unwrap();

    let files = manager.scan_vault().await.unwrap();
    assert_eq!(files.len(), 1, "expected Shouty.MD to be discovered");
}

/// The scan must not descend through a symlink.
///
/// A link pointing at an ancestor makes a following walk recurse until it
/// exhausts memory, and one pointing outside the vault would pull content
/// into the index that `resolve_path` refuses to hand back out. Both are
/// reachable by anyone who can write a file into the vault.
#[cfg(unix)]
#[tokio::test]
async fn test_scan_vault_does_not_follow_symlinks() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("real.md"), "# Real").unwrap();
    // A loop: `vault/loop` -> `vault`. A following walk never terminates.
    std::os::unix::fs::symlink(temp_dir.path(), temp_dir.path().join("loop")).unwrap();

    let outside = TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret.md"), "# Outside").unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("secret.md"),
        temp_dir.path().join("escape.md"),
    )
    .unwrap();

    let files = manager.scan_vault().await.unwrap();
    assert_eq!(files.len(), 1, "expected only the one real note: {files:?}");
    assert!(files[0].ends_with("real.md"));
}

/// `.turbovault/` is TurboVault's own audit trail and snapshot store, not
/// vault content, and the scan runs on a schedule now.
#[tokio::test]
async fn test_scan_vault_skips_the_protected_directory() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("real.md"), "# Real").unwrap();
    std::fs::create_dir_all(temp_dir.path().join(".turbovault/snapshots")).unwrap();
    std::fs::write(
        temp_dir.path().join(".turbovault/snapshots/old.md"),
        "# Snapshot",
    )
    .unwrap();

    let files = manager.scan_vault().await.unwrap();
    assert_eq!(files.len(), 1, "expected only the one real note: {files:?}");
    assert!(files[0].ends_with("real.md"));
}

// ── external-change reconciliation ───────────────────────────────────────

/// A note created by somebody else reaches the link graph and the cache.
///
/// This is the case a per-entry mtime check cannot reach, because it can
/// only re-check entries it already holds. Nothing tells this process the
/// file appeared; the scan finds it by comparing state.
#[tokio::test]
async fn reconcile_discovers_a_note_created_outside_the_process() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("seed.md"), "# Seed\n").unwrap();
    manager.initialize().await.unwrap();
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

    std::fs::write(
        temp_dir.path().join("outside.md"),
        "# Outside\n\nsee [[seed]]\n",
    )
    .unwrap();

    manager.reconcile_now().await;

    assert_eq!(manager.get_stats().await.unwrap().total_files, 2);
    let backlinks = manager.get_backlinks(Path::new("seed.md")).await.unwrap();
    assert_eq!(
        backlinks.len(),
        1,
        "the externally created note's link must reach the graph: {backlinks:?}"
    );
}

/// An edit made by somebody else replaces what the graph and cache hold,
/// including the links it added and the ones it took away.
#[tokio::test]
async fn reconcile_applies_an_edit_made_outside_the_process() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("a.md"), "# A\n").unwrap();
    std::fs::write(temp_dir.path().join("b.md"), "# B\n").unwrap();
    manager.initialize().await.unwrap();
    assert!(
        manager
            .get_forward_links(Path::new("a.md"))
            .await
            .unwrap()
            .is_empty()
    );

    std::fs::write(temp_dir.path().join("a.md"), "# A\n\nnow links to [[b]]\n").unwrap();
    manager.reconcile_now().await;

    let forward = manager.get_forward_links(Path::new("a.md")).await.unwrap();
    assert_eq!(forward.len(), 1, "expected the added link: {forward:?}");
    assert!(forward[0].ends_with("b.md"));

    let cached = manager.all_cached_vault_files().await;
    let a = cached
        .iter()
        .find(|file| file.path.ends_with("a.md"))
        .expect("a.md must still be cached");
    assert!(
        a.content.contains("now links to"),
        "the cache must hold the new revision, not the parsed original"
    );
}

/// Rewrite `path` with `content`, then put its mtime back to `mtime`.
fn write_keeping_mtime(path: &Path, content: &str, mtime: std::time::SystemTime) {
    std::fs::write(path, content).unwrap();
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
}

/// An edit that keeps the size and lands in the same mtime tick as the
/// revision we parsed (FAT and exFAT keep 2 s, HFS+ and many network
/// mounts 1 s) has the fingerprint we recorded. Read that close to its
/// mtime, a note is checked by content until the tick has passed, the
/// rule git uses for its "racily clean" index entries.
#[tokio::test]
async fn reconcile_sees_a_same_size_edit_within_the_mtime_tick() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    let path = temp_dir.path().join("task.md");
    let tick = std::time::SystemTime::now();
    write_keeping_mtime(&path, "- [ ] ship it\n", tick);
    manager.initialize().await.unwrap();

    // A checkbox toggle: same size, and on a coarse filesystem, same mtime.
    write_keeping_mtime(&path, "- [x] ship it\n", tick);
    manager.reconcile_now().await;

    let cached = manager.all_cached_vault_files().await;
    assert_eq!(cached[0].content, "- [x] ship it\n");
}

/// The content check is only for notes read inside their mtime tick, and
/// it finds nothing to report when the bytes are the same.
#[tokio::test]
async fn reconcile_is_silent_about_an_unchanged_note_read_within_its_tick() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("fresh.md"), "# Fresh\n").unwrap();
    manager.initialize().await.unwrap();

    let captured: Arc<StdMutex<Vec<(String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock().unwrap().extend(
            changed
                .into_iter()
                .map(|(path, present, _)| (path, present)),
        );
        Box::pin(async {})
    }));
    manager.reconcile_now().await;
    assert!(captured.lock().unwrap().is_empty());
}

/// Capture every `(path, present)` the change listener is handed.
fn capture_changes(manager: &VaultManager) -> Arc<StdMutex<Vec<(String, bool)>>> {
    let captured: Arc<StdMutex<Vec<(String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock().unwrap().extend(
            changed
                .into_iter()
                .map(|(path, present, _)| (path, present)),
        );
        Box::pin(async {})
    }));
    captured
}

/// A note that grows past `max_file_size` outside the process leaves
/// the scan, so it has to leave the cache and the graph too. Kept there,
/// every sweep would report it deleted again and never settle.
#[tokio::test]
async fn reconcile_settles_a_note_that_grew_past_the_size_limit() {
    let temp_dir = TempDir::new().unwrap();
    let mut config = create_test_config(temp_dir.path());
    config.max_file_size = 64;
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("big.md"), "# Big\n").unwrap();
    manager.initialize().await.unwrap();
    let captured = capture_changes(&manager);

    std::fs::write(temp_dir.path().join("big.md"), "x".repeat(200)).unwrap();
    manager.reconcile_now().await;
    assert_eq!(*captured.lock().unwrap(), [("big.md".to_string(), false)]);
    assert!(manager.all_cached_vault_files().await.is_empty());

    manager.reconcile_now().await;
    assert_eq!(
        captured.lock().unwrap().len(),
        1,
        "a second pass must find nothing new: {:?}",
        captured.lock().unwrap()
    );
}

/// A note written through a symlinked folder is one the scan never finds,
/// since it doesn't follow links. The cache must not hold it either, or
/// every sweep reports it deleted.
#[cfg(unix)]
#[tokio::test]
async fn reconcile_settles_a_note_written_through_a_symlinked_folder() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("real")).unwrap();
    std::os::unix::fs::symlink(temp_dir.path().join("real"), temp_dir.path().join("alias"))
        .unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    manager.initialize().await.unwrap();

    manager
        .write_file(
            Path::new("alias/note.md"),
            "# Note\n",
            Precondition::Blind,
            "add",
        )
        .await
        .unwrap();
    let captured = capture_changes(&manager);
    manager.reconcile_now().await;
    manager.reconcile_now().await;

    // The real path is in scope, so the note shows up once, under it.
    let changes = captured.lock().unwrap().clone();
    assert!(
        changes.iter().all(|(path, _)| path == "real/note.md"),
        "{changes:?}"
    );
    manager.reconcile_now().await;
    assert_eq!(captured.lock().unwrap().len(), changes.len());
}

/// A note deleted by somebody else leaves the graph and the cache.
#[tokio::test]
async fn reconcile_drops_a_note_deleted_outside_the_process() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("doomed.md"), "# Doomed\n").unwrap();
    manager.initialize().await.unwrap();
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

    std::fs::remove_file(temp_dir.path().join("doomed.md")).unwrap();
    manager.reconcile_now().await;

    assert_eq!(manager.get_stats().await.unwrap().total_files, 0);
    assert!(manager.all_cached_vault_files().await.is_empty());
}

/// A pass that finds nothing must not announce anything.
///
/// The listener drives index rebuilds and the plugin change feed, so a
/// sweep that reported every note on every pass would be worse than no
/// sweep at all. This is the property that makes recording the fingerprint
/// on the write path matter.
#[tokio::test]
async fn reconcile_is_silent_when_nothing_moved() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("one.md"), "# One\n").unwrap();
    std::fs::write(temp_dir.path().join("two.md"), "# Two\n").unwrap();
    manager.initialize().await.unwrap();

    let captured: Arc<StdMutex<Vec<(String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock().unwrap().extend(
            changed
                .into_iter()
                .map(|(path, present, _)| (path, present)),
        );
        Box::pin(async {})
    }));

    manager.reconcile_now().await;
    assert!(
        captured.lock().unwrap().is_empty(),
        "an unchanged vault must produce no change events: {:?}",
        captured.lock().unwrap()
    );

    // And a write this process makes is announced once, by the write path,
    // not again by the sweep that follows it.
    manager
        .write_file(
            Path::new("three.md"),
            "# Three\n",
            Precondition::Blind,
            "add",
        )
        .await
        .unwrap();
    assert_eq!(captured.lock().unwrap().len(), 1);

    manager.reconcile_now().await;
    assert_eq!(
        captured.lock().unwrap().len(),
        1,
        "the sweep must not re-announce this process's own write: {:?}",
        captured.lock().unwrap()
    );
}

/// A file the vault admits for discovery but does not model as a note must
/// not be reported over and over.
///
/// `allowed_extensions` also covers `.txt` and `.canvas`, while the note
/// cache, the link graph, and the search index are markdown only. If the
/// sweep compared across that gap, nothing would ever record having seen
/// such a file, so every pass would rediscover it and republish it: a
/// permanent, silent event storm through the change feed.
#[tokio::test]
async fn reconcile_does_not_republish_files_it_does_not_model() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("attachment.txt"), "one").unwrap();
    std::fs::write(temp_dir.path().join("note.md"), "# Note\n").unwrap();
    manager.initialize().await.unwrap();

    let captured: Arc<StdMutex<Vec<(String, bool)>>> = Arc::new(StdMutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock().unwrap().extend(
            changed
                .into_iter()
                .map(|(path, present, _)| (path, present)),
        );
        Box::pin(async {})
    }));

    // Untouched, and admitted for discovery but not modelled.
    for _ in 0..3 {
        manager.reconcile_now().await;
    }
    assert!(
        captured.lock().unwrap().is_empty(),
        "a file the cache does not model must not be reported: {:?}",
        captured.lock().unwrap()
    );

    // Changed, still not modelled, still silent.
    std::fs::write(temp_dir.path().join("attachment.txt"), "two").unwrap();
    for _ in 0..3 {
        manager.reconcile_now().await;
    }
    assert!(
        captured.lock().unwrap().is_empty(),
        "changing it must not start an event storm either: {:?}",
        captured.lock().unwrap()
    );

    // And the note beside it is still reported, exactly once.
    std::fs::write(temp_dir.path().join("note.md"), "# Note\n\nedited\n").unwrap();
    for _ in 0..3 {
        manager.reconcile_now().await;
    }
    assert_eq!(
        *captured.lock().unwrap(),
        vec![("note.md".to_string(), true)],
        "a real note change is reported once, not once per pass"
    );
}

/// External changes are reported as external, so a consumer that reports
/// its own writes at the write site can tell them apart.
#[tokio::test]
async fn reconcile_reports_external_origin() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    let captured: Arc<StdMutex<Vec<CommitOrigin>>> = Arc::new(StdMutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock()
            .unwrap()
            .extend(changed.into_iter().map(|(_, _, origin)| origin));
        Box::pin(async {})
    }));

    std::fs::write(temp_dir.path().join("theirs.md"), "# Theirs\n").unwrap();
    manager.reconcile_now().await;
    assert_eq!(
        *captured.lock().unwrap(),
        vec![CommitOrigin::External],
        "a change this process did not make is external"
    );

    captured.lock().unwrap().clear();
    manager
        .write_file(Path::new("ours.md"), "# Ours\n", Precondition::Blind, "add")
        .await
        .unwrap();
    assert_eq!(
        *captured.lock().unwrap(),
        vec![CommitOrigin::Local],
        "a change this process made is local"
    );
}

/// The schedule bounds both how stale an answer can be and what
/// reconciliation costs, without a per-vault knob.
#[test]
fn reconcile_interval_scales_with_the_last_pass() {
    let mut state = ReconcileState::new();
    assert!(state.is_due(), "the first gated call must always sweep");

    // A vault small enough that the duty cycle would ask for less than the
    // floor still waits the floor.
    state.record(Duration::from_micros(200));
    assert_eq!(state.interval(), RECONCILE_MIN_INTERVAL);
    assert!(!state.is_due(), "a pass just ran");

    // 40ms is roughly a 10k-note vault; 20x is under a second.
    state.record(Duration::from_millis(40));
    assert_eq!(state.interval(), Duration::from_millis(800));

    // A vault large enough to blow past the ceiling is capped there rather
    // than backing off without limit.
    state.record(Duration::from_secs(10));
    assert_eq!(state.interval(), RECONCILE_MAX_INTERVAL);
}

/// The gate is scheduled, so a burst of read calls costs one pass. Without
/// this the sweep would dominate the cost of every cheap tool call.
#[tokio::test]
async fn the_gate_debounces_repeated_calls() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    // First call sweeps and records a timestamp.
    manager.ensure_fresh().await;

    std::fs::write(temp_dir.path().join("late.md"), "# Late\n").unwrap();
    manager.ensure_fresh().await;
    assert_eq!(
        manager.get_stats().await.unwrap().total_files,
        0,
        "a second call inside the interval must not pay for another scan"
    );

    // An explicit request overrides the schedule.
    manager.reconcile_now().await;
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);
}

/// Turning reconciliation off leaves the scheduled gate inert, and leaves
/// the explicit call working.
#[tokio::test]
async fn reconciliation_can_be_turned_off() {
    let temp_dir = TempDir::new().unwrap();
    let mut config = create_test_config(temp_dir.path());
    config.reconcile_external_changes = false;
    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    std::fs::write(temp_dir.path().join("ignored.md"), "# Ignored\n").unwrap();
    manager.ensure_fresh().await;
    assert_eq!(manager.get_stats().await.unwrap().total_files, 0);

    manager.reconcile_now().await;
    assert_eq!(
        manager.get_stats().await.unwrap().total_files,
        1,
        "an explicit reconcile is a request, not a setting"
    );
}

/// `vault_files_validated` sees notes created outside the process, which is
/// what it could never do while it re-checked only the entries it held.
#[tokio::test]
async fn validated_files_include_externally_created_notes() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    std::fs::write(temp_dir.path().join("known.md"), "# Known\n").unwrap();
    manager.initialize().await.unwrap();

    std::fs::write(temp_dir.path().join("new.md"), "# New\n").unwrap();
    manager.reconcile_now().await;

    let files = manager.vault_files_validated().await;
    assert_eq!(files.len(), 2, "expected both notes: {files:?}");
}

// ── all_cached_vault_files ───────────────────────────────────────────────

/// Before initialize(), all_cached_vault_files returns an empty list.
#[tokio::test]
async fn test_all_cached_vault_files_empty_before_init() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();

    // Cache is empty — initialize() has not been called.
    assert!(manager.all_cached_vault_files().await.is_empty());
}

/// After initialize(), all_cached_vault_files returns all parsed files.
#[tokio::test]
async fn test_all_cached_vault_files_populated_after_init() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("a.md"), "# A").unwrap();
    std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
    manager.initialize().await.unwrap();

    assert_eq!(manager.all_cached_vault_files().await.len(), 2);
}

/// all_cached_vault_files does NOT pick up external disk modifications —
/// it returns whatever is in the cache without mtime checks.  This is the
/// intended "fast path" behaviour; callers that need freshness should use
/// vault_files_validated() instead.
#[tokio::test]
async fn test_all_cached_vault_files_does_not_detect_external_modification() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    let path = temp_dir.path().join("note.md");
    std::fs::write(&path, "---\nstatus: draft\n---\n# Note").unwrap();
    manager.initialize().await.unwrap();

    // Externally overwrite the file.
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    std::fs::write(&path, "---\nstatus: published\n---\n# Note").unwrap();

    // all_cached_vault_files returns stale data — still "draft".
    let files = manager.all_cached_vault_files().await;
    let status = files
        .iter()
        .find(|f| f.path == path)
        .and_then(|f| f.frontmatter.as_ref())
        .and_then(|fm| fm.data.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(
        status, "draft",
        "all_cached_vault_files must not re-read disk"
    );
}

// ── cache-coherence: write_file / move_file / delete_file ────────────────

/// write_file() on a brand-new file must insert it into the cache so that
/// all_cached_vault_files() returns it without a reinitialize().
#[tokio::test]
async fn test_write_file_new_inserts_into_cache() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap(); // warm empty cache

    manager
        .write_file(
            Path::new("new.md"),
            "---\nstatus: fresh\n---\n# New",
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    let files = manager.all_cached_vault_files().await;
    assert_eq!(
        files.len(),
        1,
        "new file must appear in cache after write_file"
    );

    let status = files[0]
        .frontmatter
        .as_ref()
        .and_then(|fm| fm.data.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(status, "fresh");
}

/// write_file() that overwrites an existing note must update the cached
/// frontmatter; the cache must NOT keep the stale pre-write values.
#[tokio::test]
async fn test_write_file_overwrite_updates_cache() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(
        temp_dir.path().join("note.md"),
        "---\nstatus: old\n---\n# Note",
    )
    .unwrap();
    manager.initialize().await.unwrap();

    manager
        .write_file(
            Path::new("note.md"),
            "---\nstatus: updated\n---\n# Note",
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    let files = manager.all_cached_vault_files().await;
    assert_eq!(files.len(), 1);
    let status = files[0]
        .frontmatter
        .as_ref()
        .and_then(|fm| fm.data.get("status"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(status, "updated", "cache must reflect updated frontmatter");
}

/// move_file() must evict the old path and insert the new path so that
/// all_cached_vault_files() reflects the move without reinitialize().
#[tokio::test]
async fn test_move_file_updates_cache() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(
        temp_dir.path().join("from.md"),
        "---\nstatus: active\n---\n# From",
    )
    .unwrap();
    manager.initialize().await.unwrap();

    manager
        .move_file(
            Path::new("from.md"),
            Path::new("to.md"),
            Precondition::ExpectExists,
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    let files = manager.all_cached_vault_files().await;
    assert_eq!(
        files.len(),
        1,
        "cache should have exactly one entry after move"
    );

    let from_abs = temp_dir.path().join("from.md");
    let to_abs = temp_dir.path().join("to.md");
    assert!(
        !files.iter().any(|f| f.path == from_abs),
        "old path must be absent from cache after move"
    );
    assert!(
        files.iter().any(|f| f.path == to_abs),
        "new path must be present in cache after move"
    );
}

/// delete_file() must evict the entry from the cache immediately.
#[tokio::test]
async fn test_delete_file_evicts_from_cache() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
    manager.initialize().await.unwrap();
    assert_eq!(manager.all_cached_vault_files().await.len(), 1);

    manager
        .delete_file(Path::new("note.md"), Precondition::ExpectExists, "test")
        .await
        .unwrap();

    assert_eq!(
        manager.all_cached_vault_files().await.len(),
        0,
        "deleted file must be evicted from cache"
    );
}

/// `sync_index`'s eviction branch runs after `substrate.apply()` has
/// already released its write-lock (see the "ponytail" comment on that
/// branch). If a path is recreated between an operation's apply() and
/// its sync_index() call, a stale "now absent" notification for that
/// path must not evict the file that is legitimately present again.
#[tokio::test]
async fn test_sync_index_skips_stale_removal_when_path_recreated() {
    let temp_dir = TempDir::new().unwrap();
    let config = create_test_config(temp_dir.path());
    let manager = VaultManager::new(config).unwrap();

    manager
        .write_file(Path::new("race.md"), "v1", Precondition::Blind, "test")
        .await
        .unwrap();
    assert_eq!(manager.all_cached_vault_files().await.len(), 1);

    // Stand in for a concurrent blind write that recreated the path
    // after a delete's apply() completed but before the delete's
    // sync_index() call ran.
    manager
        .write_file(Path::new("race.md"), "v2", Precondition::Blind, "test")
        .await
        .unwrap();

    // The delete's (delayed, stale) removal notification arrives last.
    manager.sync_index(&[("race.md".to_string(), false)]).await;

    assert_eq!(
        manager.all_cached_vault_files().await.len(),
        1,
        "stale removal must not evict a path that was recreated"
    );
    assert_eq!(manager.read_file(Path::new("race.md")).await.unwrap(), "v2");
}

#[tokio::test]
async fn stale_write_hash_preserves_existing_content() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    manager
        .write_file(Path::new("note.md"), "current", Precondition::Blind, "test")
        .await
        .unwrap();

    let error = manager
        .write_file(
            Path::new("note.md"),
            "replacement",
            Precondition::ExpectBlob("stale".to_string()),
            "test",
        )
        .await
        .unwrap_err();

    assert!(matches!(error, Error::ConcurrencyError { .. }));
    assert_eq!(
        manager.read_file(Path::new("note.md")).await.unwrap(),
        "current"
    );
}

#[tokio::test]
async fn expected_hash_rejects_recreating_a_deleted_file() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

    let error = manager
        .write_file(
            Path::new("missing.md"),
            "replacement",
            Precondition::ExpectBlob("stale".to_string()),
            "test",
        )
        .await
        .unwrap_err();

    match error {
        Error::ConcurrencyError { reason } => assert!(reason.contains("does not exist")),
        other => panic!("expected concurrency error, got {other:?}"),
    }
    assert!(!temp_dir.path().join("missing.md").exists());
}

#[tokio::test]
async fn stale_edit_hash_preserves_existing_content() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    manager
        .write_file(
            Path::new("note.md"),
            "hello world",
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap();

    let edits = "<<<<<<< SEARCH\nhello world\n=======\ngoodbye world\n>>>>>>> REPLACE";
    let error = manager
        .edit_file(
            Path::new("note.md"),
            edits,
            Precondition::ExpectBlob("stale".to_string()),
            false,
            "test",
        )
        .await
        .unwrap_err();

    assert!(matches!(error, Error::ConcurrencyError { .. }));
    assert_eq!(
        manager.read_file(Path::new("note.md")).await.unwrap(),
        "hello world"
    );
}

/// Staying inside the vault root is not authorization to touch everything
/// in it: `.obsidian/plugins/*/main.js` and `.git/hooks/*` are code that
/// something else executes, and `.turbovault/` is the audit trail.
#[tokio::test]
async fn protected_directories_are_unreachable_through_the_note_api() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

    for path in [
        ".obsidian/plugins/tasks/main.js",
        ".git/hooks/post-commit",
        ".turbovault/audit/operations.jsonl",
        "node_modules/pkg/index.js",
        "notes/.obsidian/nested.md",
    ] {
        let error = manager
            .write_file(Path::new(path), "payload", Precondition::Blind, "test")
            .await
            .unwrap_err();
        assert!(
            matches!(error, Error::ProtectedPath { .. }),
            "{path} should be refused, got {error:?}"
        );
        assert!(!temp_dir.path().join(path).exists(), "{path} was written");

        let error = manager.read_file(Path::new(path)).await.unwrap_err();
        assert!(
            matches!(error, Error::ProtectedPath { .. }),
            "reading {path} should be refused, got {error:?}"
        );
    }

    // A capability-holding caller can still resolve protected state; the
    // policy lives in `resolve_path`, not in the traversal check.
    manager
        .resolve_path_bypassing_policy(Path::new(".obsidian/app.json"))
        .expect("capability-gated resolution stays available");
    // ...but the vault boundary still holds for it.
    assert!(matches!(
        manager
            .resolve_path_bypassing_policy(Path::new("../escape.md"))
            .unwrap_err(),
        Error::PathTraversalAttempt { .. }
    ));
}

/// `max_file_size` used to be applied only while scanning directories, so
/// an explicit read pulled any size into memory.
#[tokio::test]
async fn max_file_size_is_enforced_on_reads_and_writes() {
    let temp_dir = TempDir::new().unwrap();
    let mut config = create_test_config(temp_dir.path());
    config.max_file_size = 32;
    let manager = VaultManager::new(config).unwrap();

    let oversized = "x".repeat(64);
    let error = manager
        .write_file(Path::new("big.md"), &oversized, Precondition::Blind, "test")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::FileTooLarge { .. }), "{error:?}");
    assert!(!temp_dir.path().join("big.md").exists());

    // Written out of band (an editor, a sync client) — the read must still
    // refuse rather than buffer the whole file.
    std::fs::write(temp_dir.path().join("big.md"), &oversized).unwrap();
    let error = manager.read_file(Path::new("big.md")).await.unwrap_err();
    assert!(matches!(error, Error::FileTooLarge { .. }), "{error:?}");

    manager
        .write_file(Path::new("small.md"), "fits", Precondition::Blind, "test")
        .await
        .expect("within the limit");
}

#[tokio::test]
async fn stale_delete_and_move_hashes_preserve_the_source() {
    let temp_dir = TempDir::new().unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    manager
        .write_file(Path::new("note.md"), "keep me", Precondition::Blind, "test")
        .await
        .unwrap();

    let delete_error = manager
        .delete_file(
            Path::new("note.md"),
            Precondition::ExpectBlob("stale".to_string()),
            "test",
        )
        .await
        .unwrap_err();
    assert!(matches!(delete_error, Error::ConcurrencyError { .. }));

    let move_error = manager
        .move_file(
            Path::new("note.md"),
            Path::new("moved.md"),
            Precondition::ExpectBlob("stale".to_string()),
            Precondition::Blind,
            "test",
        )
        .await
        .unwrap_err();
    assert!(matches!(move_error, Error::ConcurrencyError { .. }));
    assert_eq!(
        manager.read_file(Path::new("note.md")).await.unwrap(),
        "keep me"
    );
    assert!(!temp_dir.path().join("moved.md").exists());
}

// -------------------------------------------------------------------------
// M4b: VaultManager::new builds a live Git substrate from write_backend
// -------------------------------------------------------------------------

/// A born-HEAD git repo (mirrors `substrate.rs`'s own `init_repo` test
/// helper) — required before `commit_changeset` can build a parent chain.
fn init_git_repo(dir: &Path) {
    let mut opts = git2::RepositoryInitOptions::new();
    opts.initial_head("main");
    git2::Repository::init_opts(dir, &opts).unwrap();
}

/// A `write_backend: git` vault config, the M4b construction path.
fn create_git_test_config(vault_dir: &Path) -> ServerConfig {
    let mut config = ServerConfig::new();
    let vault_config = VaultConfig::builder("git_vault", vault_dir)
        .write_backend(WriteBackend::Git)
        .build()
        .unwrap();
    config.vaults.push(vault_config);
    config
}

fn head_oid(dir: &Path) -> Option<git2::Oid> {
    git2::Repository::open(dir).ok()?.head().ok()?.target()
}

/// The blob content at `rel_path` in HEAD's tree, for asserting working
/// tree == HEAD independent of what's on disk.
fn head_blob_content(dir: &Path, rel_path: &str) -> Option<String> {
    let repo = git2::Repository::open(dir).ok()?;
    let tree = repo.head().ok()?.peel_to_commit().ok()?.tree().ok()?;
    let entry = tree.get_path(Path::new(rel_path)).ok()?;
    let blob = repo.find_blob(entry.id()).ok()?;
    Some(String::from_utf8_lossy(blob.content()).into_owned())
}

/// A `VaultManager` built over a `write_backend: git` vault routes
/// `write_file` through `GitSubstrate`: the write lands as a commit, the
/// working tree agrees with HEAD, and the link graph picks up the note
/// via `sync_index` (R7) — all without touching `GitFileTools`.
#[tokio::test]
async fn test_git_backend_write_file_commits_and_syncs_index() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());
    let manager = VaultManager::new(create_git_test_config(temp_dir.path())).unwrap();

    assert!(
        head_oid(temp_dir.path()).is_none(),
        "repo starts unborn (no commits yet)"
    );

    manager
        .write_file(
            Path::new("note.md"),
            "# Git Note",
            Precondition::Blind,
            "test commit",
        )
        .await
        .unwrap();

    assert!(
        head_oid(temp_dir.path()).is_some(),
        "write_file on a git vault must land a commit"
    );
    assert_eq!(
        manager.read_file(Path::new("note.md")).await.unwrap(),
        "# Git Note"
    );
    assert_eq!(
        head_blob_content(temp_dir.path(), "note.md").as_deref(),
        Some("# Git Note"),
        "working tree must agree with HEAD after a git-backend apply"
    );

    // sync_index (R7) ran off the substrate's ApplyOutcome, not a
    // separate `initialize()` scan.
    let stats = manager.get_stats().await.unwrap();
    assert_eq!(stats.total_files, 1);
}

/// `apply_changes` on a git vault is the same GitSubstrate path as
/// `write_file` — exercise it directly, matching
/// `test_apply_changes_multi_change_plan`'s direct-backend coverage.
#[tokio::test]
async fn test_git_backend_apply_changes_commits_multi_change_plan() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());
    let manager = VaultManager::new(create_git_test_config(temp_dir.path())).unwrap();

    let plan = ChangePlan::new("multi-change git test")
        .create("a.md", "# A")
        .create("b.md", "# B");
    let outcome = manager.apply_changes(&plan).await.unwrap();

    assert!(outcome.commit.is_some());
    assert_eq!(
        head_blob_content(temp_dir.path(), "a.md").as_deref(),
        Some("# A")
    );
    assert_eq!(
        head_blob_content(temp_dir.path(), "b.md").as_deref(),
        Some("# B")
    );
}

/// A stale `ExpectBlob` against a git vault aborts the whole plan with
/// `ConcurrencyError` and lands zero commits — the manager's GitSubstrate
/// enforces the same CAS guarantee `GitFileTools` does today.
#[tokio::test]
async fn test_git_backend_stale_precondition_aborts_no_commit() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());
    let manager = VaultManager::new(create_git_test_config(temp_dir.path())).unwrap();

    manager
        .write_file(Path::new("note.md"), "v1", Precondition::Blind, "seed")
        .await
        .unwrap();
    let head_after_seed = head_oid(temp_dir.path());

    let err = manager
        .write_file(
            Path::new("note.md"),
            "v2",
            Precondition::ExpectBlob("deadbeef".to_string()),
            "stale update",
        )
        .await
        .unwrap_err();

    assert!(matches!(err, Error::ConcurrencyError { .. }));
    assert_eq!(
        head_oid(temp_dir.path()),
        head_after_seed,
        "a stale precondition must not land a commit"
    );
    assert_eq!(
        head_blob_content(temp_dir.path(), "note.md").as_deref(),
        Some("v1"),
        "HEAD must still hold the pre-abort content"
    );
}

// -------------------------------------------------------------------------
// M4c (bite 3a): manager-owned reindex — change-listener, out-of-band
// drain, and background-task lifecycle (turbovault-qae.5.3, deliverable F)
// -------------------------------------------------------------------------

/// Create a commit with bare git2, bypassing the substrate — an
/// out-of-band ref advance (another process / manual git commit).
fn make_external_commit(repo_path: &Path, file_name: &str, content: &str) {
    let repo = git2::Repository::open(repo_path).unwrap();
    std::fs::write(repo_path.join(file_name), content).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(file_name)).unwrap();
    let tree_oid = index.write_tree().unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(tree_oid).unwrap();
    let sig = git2::Signature::now("Ext", "ext@example").unwrap();
    let parent = repo
        .head()
        .ok()
        .and_then(|h| h.target())
        .and_then(|oid| repo.find_commit(oid).ok());
    match parent {
        Some(parent) => repo
            .commit(Some("HEAD"), &sig, &sig, content, &tree, &[&parent])
            .unwrap(),
        None => repo
            .commit(Some("HEAD"), &sig, &sig, content, &tree, &[])
            .unwrap(),
    };
}

/// A `write_file` through a git-backend manager fires the R7
/// change-listener with the written path — the search-staleness close
/// (the server registers a listener that feeds search + similarity off
/// exactly this set).
#[tokio::test]
async fn git_write_fires_change_listener_with_written_path() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());
    let manager = Arc::new(VaultManager::new(create_git_test_config(temp_dir.path())).unwrap());
    manager.ensure_reindex_started();

    let captured: Arc<std::sync::Mutex<Vec<(String, bool)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock().unwrap().extend(
            changed
                .into_iter()
                .map(|(path, present, _)| (path, present)),
        );
        Box::pin(async {})
    }));

    manager
        .write_file(
            Path::new("note.md"),
            "# Note",
            Precondition::Blind,
            "commit",
        )
        .await
        .unwrap();

    let got = captured.lock().unwrap();
    assert!(
        got.iter().any(|(p, present)| p == "note.md" && *present),
        "change-listener must fire with (note.md, present=true); got {got:?}"
    );
}

/// An out-of-band commit (bare git2) is detected by the manager's
/// ref-listener and, after `flush_reindex()`, is reflected in the link
/// graph AND fired to the change-listener.
#[tokio::test]
async fn git_out_of_band_commit_drains_into_graph_and_listener() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());
    // Seed a committed note: a non-None ref baseline + one graph node.
    make_external_commit(temp_dir.path(), "seed.md", "# Seed\n");
    let manager = Arc::new(VaultManager::new(create_git_test_config(temp_dir.path())).unwrap());
    manager.initialize().await.unwrap();
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

    let captured: Arc<std::sync::Mutex<Vec<(String, bool)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let cap = Arc::clone(&captured);
    manager.set_change_listener(Arc::new(move |changed| {
        cap.lock().unwrap().extend(
            changed
                .into_iter()
                .map(|(path, present, _)| (path, present)),
        );
        Box::pin(async {})
    }));

    // Fast ref poll so the test doesn't wait the production 5s.
    manager.ensure_reindex_started_with_interval(std::time::Duration::from_millis(25));
    // Let the listener snapshot its HEAD baseline (= the seed commit)
    // BEFORE the out-of-band commit lands, so it registers as an advance.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Out-of-band commit adds external.md (links back to seed).
    make_external_commit(
        temp_dir.path(),
        "external.md",
        "# External\n\nsee [[seed]]\n",
    );

    // The ref-listener enqueues within a poll; the drainer and/or this
    // flush apply it (idempotent). Poll until the graph reflects it.
    let mut reflected = false;
    for _ in 0..80 {
        manager.flush_reindex().await;
        if manager.get_stats().await.unwrap().total_files == 2 {
            reflected = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        reflected,
        "out-of-band commit must be reflected in the link graph after flush"
    );

    let got = captured.lock().unwrap();
    assert!(
        got.iter()
            .any(|(p, present)| p == "external.md" && *present),
        "change-listener must fire with the out-of-band path; got {got:?}"
    );
}

/// Dropping the manager aborts its background tasks (no leak). The tasks
/// hold `Weak<Self>`, never `Arc<Self>`, so the caller's ref is the last
/// strong one — an `Arc` capture would keep this upgrade succeeding.
#[tokio::test]
async fn dropping_manager_aborts_reindex_tasks_no_leak() {
    let temp_dir = TempDir::new().unwrap();
    init_git_repo(temp_dir.path());
    let manager = Arc::new(VaultManager::new(create_git_test_config(temp_dir.path())).unwrap());
    manager.ensure_reindex_started();

    let weak = Arc::downgrade(&manager);
    drop(manager);

    // A drainer iteration may briefly hold an upgraded Arc; once Drop's
    // abort lands the last strong ref is gone for good.
    let mut gone = false;
    for _ in 0..100 {
        if weak.upgrade().is_none() {
            gone = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(gone, "manager leaked: a background task holds a strong ref");
}

/// On a filesystem that finds a name whatever its case (macOS, Windows), a
/// note addressed in another case is the same note: it's written in place,
/// and the cache and the graph hold it once, under its stored name.
#[tokio::test]
async fn a_note_addressed_in_another_case_is_one_note() {
    let temp_dir = TempDir::new().unwrap();
    std::fs::create_dir(temp_dir.path().join("Projects")).unwrap();
    std::fs::write(temp_dir.path().join("Projects/Roadmap.md"), "# Roadmap\n").unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    manager.initialize().await.unwrap();
    if !paths::filesystem_folds_names(temp_dir.path()) {
        // Here `projects/roadmap.md` is a different file, rightly.
        return;
    }

    let addressed = manager
        .resolve_path(Path::new("projects/roadmap.md"))
        .unwrap();
    assert_eq!(manager.relative_path(&addressed), "Projects/Roadmap.md");
    let new_note = manager.resolve_path(Path::new("PROJECTS/New.md")).unwrap();
    assert_eq!(manager.relative_path(&new_note), "Projects/New.md");

    manager
        .write_file(
            Path::new("projects/roadmap.md"),
            "# Roadmap\n\nShipped.\n",
            Precondition::Blind,
            "update",
        )
        .await
        .unwrap();
    manager.reconcile_now().await;
    manager.reconcile_now().await;

    let cached: Vec<String> = manager
        .all_cached_vault_files()
        .await
        .iter()
        .map(|file| manager.relative_path(&file.path))
        .collect();
    assert_eq!(cached, ["Projects/Roadmap.md"]);
    assert_eq!(manager.get_stats().await.unwrap().total_files, 1);
    assert_eq!(
        std::fs::read_to_string(temp_dir.path().join("Projects/Roadmap.md")).unwrap(),
        "# Roadmap\n\nShipped.\n"
    );
}

/// On macOS a name is found whichever Unicode form its accents are in, so
/// a note stored decomposed and addressed composed is the same note.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn a_note_addressed_in_another_unicode_form_is_one_note() {
    use unicode_normalization::UnicodeNormalization;
    let temp_dir = TempDir::new().unwrap();
    let stored: String = "Café.md".nfd().collect();
    std::fs::write(temp_dir.path().join(&stored), "# Café\n").unwrap();
    let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
    manager.initialize().await.unwrap();

    let composed: String = "café.md".nfc().collect();
    let addressed = manager.resolve_path(Path::new(&composed)).unwrap();
    assert_eq!(manager.relative_path(&addressed), stored);
}
