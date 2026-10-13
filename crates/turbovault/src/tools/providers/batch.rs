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
        description = "Run several note operations (create, write, edit, delete, move, frontmatter and tag changes) in one call. With write_backend=git, the batch is one atomic commit: every operation applies or none do. On a plain-folder vault they apply in order, and one that fails leaves the earlier ones applied; the result says which.",
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
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;

        if operations.is_empty() {
            return Err(McpError::invalid_params(
                "Batch operations list cannot be empty".to_string(),
            ));
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
        let result = BatchTools::new(manager)
            .batch_execute(operations, &message)
            .await
            .map_err(to_mcp_error)?;

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
            .with_next_step("quick_health_check");

        if execution_mode == "sequential_direct" && !success {
            response = response.with_warning(
                "Direct batch execution is sequential: operations completed before the failure were not rolled back. Configure write_backend=git for all-or-nothing batches.",
            );
        }

        response.to_json()
    }
}
