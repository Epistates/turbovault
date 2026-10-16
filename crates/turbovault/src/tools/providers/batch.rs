//! BatchProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct BatchProvider(CoreToolHandler);

impl BatchProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for BatchProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl BatchProvider {
    // ==================== Batch Operations ====================

    /// Execute a validated batch of file operations.
    #[tool(
        description = "Run several note operations (create, write, edit, delete, move, frontmatter and tag changes) in one call. With write_backend=git, the batch is one atomic commit: every operation applies or none do. On a plain-folder vault they apply in order, and one that fails leaves the earlier ones applied; the result says which. A batch that deletes or replaces notes asks the user to confirm first when the client supports it.",
        title = "Batch Execute",
        tags = ["write", "batch"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<turbovault_tools::BatchResult>,
    )]
    async fn batch_execute(
        &self,
        #[description(
            "The operations, in order. Each is an object with `type` (`CreateNote`, `WriteNote`, `EditNote`, `DeleteNote`, `MoveNote`, `UpdateLinks`, `UpdateFrontmatter`, `ManageTags` or `CreateFromTemplate`) and that operation's fields, which mirror the single-note tools."
        )]
        operations: Vec<BatchOperation>,
        #[description(
            "Commit message for the batch on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the operations."
        )]
        commit_message: Option<String>,
        ctx: &RequestContext,
    ) -> McpResult<serde_json::Value> {
        let mut stages = Stages::new(ctx, 2);
        stages.next("Checking the operations").await?;
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;

        if operations.is_empty() {
            return Err(McpError::invalid_params(
                "Batch operations list cannot be empty".to_string(),
            ));
        }

        // A batch asks before it deletes or replaces a note, as delete_note
        // and move_note do on their own.
        if let Some(action) = destructive_action(&operations, &manager, &vault_name) {
            self.confirm_destructive(ctx, action).await?;
        }

        let op_count = operations.len();
        let message = self
            .resolve_commit_message(commit_message, || derive_batch_message(&operations))
            .await?;
        // Derive the per-operation change BEFORE the batch consumes the typed
        // operations. The result's records only carry an operation name and
        // affected paths, which is not enough to tell a creation from a
        // deletion.
        let planned: Vec<VaultChange> = operations.iter().map(batch_operation_change).collect();
        // The last point a cancellation can stop the batch: past here it
        // applies, all of it on a Git-backed vault.
        stages.next("Applying the operations").await?;
        let result = BatchTools::new(manager)
            .batch_execute(operations, &message)
            .await
            .map_err(to_mcp_error)?;
        stages.finish().await;

        // Report only what landed. A git batch is all-or-nothing so every
        // record succeeds, but a legacy batch stops at the first failure with
        // earlier operations already applied.
        let applied = result
            .records
            .iter()
            .filter(|record| record.success)
            .filter_map(|record| planned.get(record.operation_index).cloned());
        self.after_write(
            &vault_name,
            applied,
            WriteAttribution::host("batch_execute"),
        )
        .await;
        let execution_mode = if self
            .multi_vault_mgr
            .get_active_vault_config()
            .await
            .map(|config| config.write_backend == WriteBackend::Git)
            .unwrap_or(false)
        {
            "atomic_git_commit"
        } else {
            "sequential_direct"
        };
        let success = result.success;
        let mut response = StandardResponse::new(vault_name, "batch_execute", result)
            .with_success(success)
            .with_count(op_count)
            .with_meta("execution_mode", serde_json::json!(execution_mode))
            .with_next_step("vault_health");

        if execution_mode == "sequential_direct" && !success {
            response = response.with_warning(
                "Direct batch execution is sequential: operations completed before the failure were not rolled back. Configure write_backend=git for all-or-nothing batches.",
            );
        }

        response.to_json()
    }
}

/// The question to ask before a batch that deletes notes or moves one onto a
/// note that's there, naming them; `None` when it does neither. A move
/// replaces a note on the same terms as move_note: its destination guard
/// allows it and something is at the destination.
fn destructive_action(
    operations: &[BatchOperation],
    manager: &VaultManager,
    vault_name: &str,
) -> Option<String> {
    let mut deleted = Vec::new();
    let mut replaced = Vec::new();
    for operation in operations {
        match operation {
            BatchOperation::DeleteNote { path, .. } => deleted.push(path.as_str()),
            BatchOperation::MoveNote {
                to,
                dest_expected_hash,
                ..
            } if dest_expected_hash
                .as_deref()
                .is_some_and(|token| token != "absent")
                && manager
                    .resolve_path(Path::new(to))
                    .is_ok_and(|resolved| resolved.exists()) =>
            {
                replaced.push(to.as_str())
            }
            _ => {}
        }
    }

    let mut parts = Vec::new();
    if !deleted.is_empty() {
        parts.push(format!("delete {}", name_paths(&deleted)));
    }
    if !replaced.is_empty() {
        parts.push(format!(
            "replace {} by moving notes onto them",
            name_paths(&replaced)
        ));
    }
    if parts.is_empty() {
        return None;
    }
    Some(format!(
        "Run a batch that will {} in the vault `{vault_name}`?",
        parts.join(" and ")
    ))
}

/// Up to ten paths in backticks, then how many more there are.
fn name_paths(paths: &[&str]) -> String {
    const SHOWN: usize = 10;
    let mut named = paths
        .iter()
        .take(SHOWN)
        .map(|path| format!("`{path}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > SHOWN {
        named.push_str(&format!(" and {} more", paths.len() - SHOWN));
    }
    named
}
