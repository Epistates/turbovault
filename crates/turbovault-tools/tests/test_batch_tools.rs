//! Unit tests for BatchTools

use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::{Change, ConfigProfile, Precondition, VaultConfig};
use turbovault_tools::{BatchOperation, BatchTools, MetadataTools};
use turbovault_vault::VaultManager;

async fn setup_test_vault() -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    tokio::fs::write(
        vault_path.join("existing.md"),
        "# Existing\nOriginal content",
    )
    .await
    .unwrap();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    (temp_dir, Arc::new(manager))
}

/// Like [`setup_test_vault`], but takes the seed files directly and
/// initializes AFTER writing them, so the link graph resolves backlinks
/// among them (write-substrate-layering M4a link-aware plan tests).
async fn setup_vault_with_files(files: &[(&str, &str)]) -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    for (path, content) in files {
        let full = vault_path.join(path);
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(full, content).await.unwrap();
    }

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    (temp_dir, Arc::new(manager))
}

#[tokio::test]
async fn test_batch_execute_single_write() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::WriteNote {
        path: "new.md".to_string(),
        content: "# New Note\nContent".to_string(),
        expected_hash: None,
    }];

    let result = tools.batch_execute(ops, "test batch").await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
    assert_eq!(batch_result.executed, 1);

    // Verify file was created
    let vault_path = manager.vault_path();
    assert!(vault_path.join("new.md").exists());
}

#[tokio::test]
async fn test_batch_execute_multiple_writes() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "note1.md".to_string(),
            content: "# Note 1".to_string(),
            expected_hash: None,
        },
        BatchOperation::WriteNote {
            path: "note2.md".to_string(),
            content: "# Note 2".to_string(),
            expected_hash: None,
        },
        BatchOperation::WriteNote {
            path: "note3.md".to_string(),
            content: "# Note 3".to_string(),
            expected_hash: None,
        },
    ];

    let result = tools.batch_execute(ops, "test batch").await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
    assert_eq!(batch_result.executed, 3);
}

#[tokio::test]
async fn test_batch_execute_delete() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::DeleteNote {
        path: "existing.md".to_string(),
        expected_hash: None,
        on_backlinks: None,
    }];

    let result = tools.batch_execute(ops, "test batch").await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);

    // Verify file was deleted
    let vault_path = manager.vault_path();
    assert!(!vault_path.join("existing.md").exists());
}

#[tokio::test]
async fn test_batch_execute_move() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::MoveNote {
        from: "existing.md".to_string(),
        to: "moved.md".to_string(),
        expected_hash: None,
        update_backlinks: None,
    }];

    let result = tools.batch_execute(ops, "test batch").await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);

    // Verify file was moved
    let vault_path = manager.vault_path();
    assert!(!vault_path.join("existing.md").exists());
    assert!(vault_path.join("moved.md").exists());
}

#[tokio::test]
async fn test_batch_execute_mixed_operations() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "new1.md".to_string(),
            content: "# New 1".to_string(),
            expected_hash: None,
        },
        BatchOperation::WriteNote {
            path: "new2.md".to_string(),
            content: "# New 2".to_string(),
            expected_hash: None,
        },
        BatchOperation::MoveNote {
            from: "existing.md".to_string(),
            to: "renamed.md".to_string(),
            expected_hash: None,
            update_backlinks: None,
        },
    ];

    let result = tools.batch_execute(ops, "test batch").await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
    assert_eq!(batch_result.executed, 3);
}

#[tokio::test]
async fn test_batch_execute_rollback_on_error() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "success1.md".to_string(),
            content: "# Success 1".to_string(),
            expected_hash: None,
        },
        BatchOperation::DeleteNote {
            path: "nonexistent.md".to_string(), // This will fail,
            expected_hash: None,
            on_backlinks: None,
        },
        BatchOperation::WriteNote {
            path: "success2.md".to_string(),
            content: "# Success 2".to_string(),
            expected_hash: None,
        },
    ];

    let result = tools.batch_execute(ops, "test batch").await;
    // Implementation returns Ok(BatchResult { success: false }), not Err
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(!batch_result.success);
    // write-substrate-layering M4d/M4e: the manager-routed batch folds every
    // op into ONE ChangePlan and reports `executed: 0` on any apply failure
    // (per-index `failed_at` tracking on the direct backend is M5.2 future
    // work) — it does NOT mean nothing landed on disk; see below.
    assert_eq!(batch_result.executed, 0);

    // Note: the direct backend's apply loop is sequential with no rollback
    // (only the precondition GATE is atomic) — operation 0 (success1.md) was
    // written before the delete of a nonexistent file failed mid-loop.
    let vault_path = manager.vault_path();
    assert!(vault_path.join("success1.md").exists()); // Written before failure
    assert!(!vault_path.join("success2.md").exists()); // Not executed after failure
}

#[tokio::test]
async fn test_batch_execute_empty_operations() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager);

    let ops: Vec<BatchOperation> = vec![];

    let result = tools.batch_execute(ops, "test batch").await;
    // Should handle empty operations gracefully
    assert!(result.is_err() || result.unwrap().executed == 0);
}

#[tokio::test]
async fn test_batch_execute_creates_directories() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::WriteNote {
        path: "nested/deep/folder/note.md".to_string(),
        content: "# Nested Note".to_string(),
        expected_hash: None,
    }];

    let result = tools.batch_execute(ops, "test batch").await;
    assert!(result.is_ok());

    // Verify nested directories were created
    let vault_path = manager.vault_path();
    assert!(vault_path.join("nested/deep/folder/note.md").exists());
}

#[tokio::test]
async fn test_batch_execute_atomic_guarantees() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    // First batch should succeed
    let ops1 = vec![
        BatchOperation::WriteNote {
            path: "atomic1.md".to_string(),
            content: "# Atomic 1".to_string(),
            expected_hash: None,
        },
        BatchOperation::WriteNote {
            path: "atomic2.md".to_string(),
            content: "# Atomic 2".to_string(),
            expected_hash: None,
        },
    ];

    let result1 = tools.batch_execute(ops1, "test batch 1").await;
    assert!(result1.is_ok());

    // Second batch with error should not affect first batch
    let ops2 = vec![
        BatchOperation::WriteNote {
            path: "atomic3.md".to_string(),
            content: "# Atomic 3".to_string(),
            expected_hash: None,
        },
        BatchOperation::DeleteNote {
            path: "nonexistent_for_atomic_test.md".to_string(),
            expected_hash: None,
            on_backlinks: None,
        },
    ];

    let result2 = tools.batch_execute(ops2, "test batch 2").await;
    // Implementation returns Ok(BatchResult { success: false }), not Err
    assert!(result2.is_ok());
    let batch_result2 = result2.unwrap();
    assert!(!batch_result2.success);
    // See test_batch_execute_rollback_on_error: the manager-routed batch
    // reports `executed: 0` on any apply failure (M5.2 adds per-index
    // `failed_at`); atomic3.md still landed (asserted below) since the
    // direct backend's apply loop is sequential with no rollback.
    assert_eq!(batch_result2.executed, 0);

    // Verify first batch files still exist (different batch, unaffected)
    let vault_path = manager.vault_path();
    assert!(vault_path.join("atomic1.md").exists());
    assert!(vault_path.join("atomic2.md").exists());

    // Second batch: operation 0 executed before failure at operation 1
    assert!(vault_path.join("atomic3.md").exists()); // Written before failure in batch 2
}

#[tokio::test]
async fn test_async_error_path_concurrent_batch_operations() {
    let (_temp_dir, manager) = setup_test_vault().await;

    // Spawn multiple concurrent batch operations
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let tools = BatchTools::new(manager.clone());
            tokio::spawn(async move {
                let ops = vec![BatchOperation::WriteNote {
                    path: format!("concurrent_{}.md", i),
                    content: format!("# Concurrent {}", i),
                    expected_hash: None,
                }];
                tools.batch_execute(ops, "test batch").await
            })
        })
        .collect();

    // All batches should complete successfully
    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }
}

// ==================== write-substrate-layering M4a: ChangePlan translation ====================
//
// `BatchTools::plan`/`plan_move_with_links`/`plan_delete_with_stale_links` are pure builders —
// additive and dormant (no production caller yet). These assert PLAN STRUCTURE, not disk
// effects: nothing here applies the plan.

#[tokio::test]
async fn test_plan_mixed_ops_builds_expected_changes_and_preconditions() {
    let (temp_dir, manager) = setup_test_vault().await;
    tokio::fs::write(temp_dir.path().join("tagged.md"), "# Tagged\n")
        .await
        .unwrap();
    let tools = BatchTools::new(manager.clone());

    let mut frontmatter = HashMap::new();
    frontmatter.insert("status".to_string(), serde_json::json!("done"));

    let ops = vec![
        BatchOperation::CreateNote {
            path: "new.md".to_string(),
            content: "# New\n".to_string(),
            force: None,
        },
        BatchOperation::UpdateFrontmatter {
            path: "existing.md".to_string(),
            frontmatter: frontmatter.clone(),
            merge: Some(true),
            expected_hash: Some("cafebabe".to_string()),
        },
        BatchOperation::ManageTags {
            path: "tagged.md".to_string(),
            operation: "add".to_string(),
            tags: vec!["foo".to_string()],
            expected_hash: None,
        },
    ];

    // Ground truth for the two compute_*-backed arms — the translation must
    // reuse these helpers verbatim, not reimplement them.
    let mt = MetadataTools::new(manager.clone());
    let (expected_fm_content, _) = mt
        .compute_update_frontmatter("existing.md", frontmatter.into_iter().collect(), true)
        .await
        .unwrap();
    let (expected_tags_content, _) = mt
        .compute_manage_tags("tagged.md", "add", Some(&["foo".to_string()]))
        .await
        .unwrap();
    let expected_tags_content = expected_tags_content.expect("'add' produces a write");

    let plan = tools.plan(&ops).await.unwrap();

    assert_eq!(
        plan.changes,
        vec![
            Change::Upsert {
                path: "new.md".to_string(),
                content: b"# New\n".to_vec(),
            },
            Change::Upsert {
                path: "existing.md".to_string(),
                content: expected_fm_content.into_bytes(),
            },
            Change::Upsert {
                path: "tagged.md".to_string(),
                content: expected_tags_content.into_bytes(),
            },
        ]
    );
    assert_eq!(
        plan.preconditions,
        vec![
            ("new.md".to_string(), Precondition::ExpectAbsent),
            (
                "existing.md".to_string(),
                Precondition::ExpectBlob("cafebabe".to_string())
            ),
        ],
        "tagged.md carries no precondition — its op passed expected_hash: None"
    );
}

#[tokio::test]
async fn test_plan_move_with_links_rewrites_backlink_and_carries_precondition() {
    let (_temp_dir, manager) =
        setup_vault_with_files(&[("old.md", "# Old\n"), ("linker.md", "see [[old]] here\n")]).await;
    let tools = BatchTools::new(manager.clone());

    let plan = tools
        .plan_move_with_links("old.md", "new.md", None, "move old.md to new.md")
        .await
        .unwrap();

    let rewritten_linker = turbovault_tools::wikilink_rewriter::rewrite_wikilinks(
        "see [[old]] here\n",
        "old.md",
        "new.md",
    );
    assert_eq!(
        plan.changes,
        vec![
            Change::Remove {
                path: "old.md".to_string(),
            },
            Change::Upsert {
                path: "new.md".to_string(),
                content: b"# Old\n".to_vec(),
            },
            Change::Upsert {
                path: "linker.md".to_string(),
                content: rewritten_linker.clone().into_bytes(),
            },
        ]
    );
    assert_eq!(
        plan.preconditions,
        vec![
            ("new.md".to_string(), Precondition::ExpectAbsent),
            (
                "linker.md".to_string(),
                Precondition::ExpectBlob(turbovault_vault::compute_hash("see [[old]] here\n"))
            ),
        ],
        "old.md carries no precondition — expected_hash was None"
    );
}

#[tokio::test]
async fn test_plan_delete_with_stale_links_rewrites_backlink_and_carries_precondition() {
    let (_temp_dir, manager) = setup_vault_with_files(&[
        ("doomed.md", "# Doomed\n"),
        ("linker.md", "see [[doomed]] here\n"),
    ])
    .await;
    let tools = BatchTools::new(manager.clone());

    let plan = tools
        .plan_delete_with_stale_links("doomed.md", None, "delete doomed.md")
        .await
        .unwrap();

    let rewritten_linker = turbovault_tools::wikilink_rewriter::wrap_wikilinks_as_stale(
        "see [[doomed]] here\n",
        "doomed.md",
    );
    assert_eq!(
        plan.changes,
        vec![
            Change::Remove {
                path: "doomed.md".to_string(),
            },
            Change::Upsert {
                path: "linker.md".to_string(),
                content: rewritten_linker.clone().into_bytes(),
            },
        ]
    );
    assert_eq!(
        plan.preconditions,
        vec![(
            "linker.md".to_string(),
            Precondition::ExpectBlob(turbovault_vault::compute_hash("see [[doomed]] here\n"))
        )],
        "doomed.md carries no precondition — expected_hash was None"
    );
}

#[tokio::test]
async fn test_plan_rejects_intra_batch_same_path_collision() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "a.md".to_string(),
            content: "v1".to_string(),
            expected_hash: None,
        },
        BatchOperation::WriteNote {
            path: "a.md".to_string(),
            content: "v2".to_string(),
            expected_hash: None,
        },
    ];

    let err = tools.plan(&ops).await.unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("intra-batch path collision") && msg.contains("a.md"),
        "got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// TV-016 (turbovault-qim): the direct batch must REPORT what it mutated
// ---------------------------------------------------------------------------

/// (A) The ticket's repro. A direct batch is best-effort: op 0 lands, op 1
/// fails, op 2 is never attempted. The response must say exactly that —
/// `executed: 1`, `failed_at: Some(1)`, op 0 in `changes` and on disk, and a
/// record per attempted op — instead of the pre-fix `executed: 0` /
/// `failed_at: null` / empty `changes` + `records`, which claimed nothing
/// happened while va.md sat on disk.
#[tokio::test]
async fn test_batch_partial_failure_reports_the_op_that_landed() {
    let (temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::CreateNote {
            path: "va.md".to_string(),
            content: "# VA".to_string(),
            force: None,
        },
        BatchOperation::DeleteNote {
            path: "vb-absent.md".to_string(), // fails: no such file
            expected_hash: None,
            on_backlinks: None,
        },
        BatchOperation::CreateNote {
            path: "vc.md".to_string(),
            content: "# VC".to_string(),
            force: None,
        },
    ];

    let result = tools.batch_execute(ops, "tv-016 partial").await.unwrap();

    assert!(
        !result.success,
        "a batch that stopped mid-plan is not a success"
    );
    assert_eq!(result.executed, 1, "op 0 genuinely landed");
    assert_eq!(result.failed_at, Some(1), "op 1 is the op that failed");
    assert_eq!(result.total, 3);
    assert_eq!(
        result.changes,
        vec!["created va.md".to_string()],
        "changes lists the ops that applied, not the whole batch"
    );

    assert_eq!(
        result.records.len(),
        2,
        "one record per ATTEMPTED op — op 2 was never reached"
    );
    assert_eq!(result.records[0].operation_index, 0);
    assert!(result.records[0].success);
    assert!(result.records[0].error.is_none());
    assert_eq!(result.records[0].affected_files, vec!["va.md".to_string()]);
    assert_eq!(result.records[1].operation_index, 1);
    assert!(!result.records[1].success);
    assert!(
        result.records[1].error.is_some(),
        "the failing op carries its error"
    );
    assert!(
        !result.errors.is_empty(),
        "the batch still surfaces the error"
    );

    // The report has to match the disk, which is the whole point.
    assert!(temp_dir.path().join("va.md").exists());
    assert!(!temp_dir.path().join("vc.md").exists());
}

/// (B) A SUCCESSFUL batch `MoveNote` rewrites every inbound wikilink in the
/// same plan, so the linker it rewrote is a file this operation mutated —
/// `affected_files` must name it. Pre-fix it was built from the operation's
/// DECLARED paths (`BatchOperation::affected_files`), which cannot know about
/// a linker the plan discovered from the link graph.
#[tokio::test]
async fn test_batch_move_reports_backlink_rewritten_linker_in_affected_files() {
    let (temp_dir, manager) =
        setup_vault_with_files(&[("bt.md", "# Target\n"), ("bl.md", "See [[bt]] here.\n")]).await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::MoveNote {
        from: "bt.md".to_string(),
        to: "bt-renamed.md".to_string(),
        expected_hash: None,
        update_backlinks: None,
    }];

    let result = tools.batch_execute(ops, "tv-016 move").await.unwrap();

    assert!(result.success, "errors: {:?}", result.errors);
    assert_eq!(result.executed, 1);
    let affected = &result.records[0].affected_files;
    assert!(
        affected.contains(&"bl.md".to_string()),
        "the rewritten linker is a file this op mutated: {affected:?}"
    );
    assert!(affected.contains(&"bt.md".to_string()), "{affected:?}");
    assert!(
        affected.contains(&"bt-renamed.md".to_string()),
        "{affected:?}"
    );

    // The linker really was rewritten on disk — affected_files is not a lie
    // in the other direction either.
    assert_eq!(
        tokio::fs::read_to_string(temp_dir.path().join("bl.md"))
            .await
            .unwrap(),
        "See [[bt-renamed]] here.\n"
    );
}
