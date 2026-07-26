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

#[turbomcp::server(name = "obsidian-vault", version = "1.6.0")]
impl BatchProvider {
    // ==================== Batch Operations ====================

    /// Execute a validated batch of file operations.
    #[tool(
        description = "Execute multiple file operations. With write_backend=git, the complete batch is one atomic commit with per-path CAS preconditions: every operation applies or none do.",
        usage = "Use the Git backend for all-or-nothing batches and cross-process concurrency safety. Pass expected_hash on guarded operations and an optional commit_message. The direct backend remains sequential and refuses Git-only operations or batch CAS preconditions.",
        performance = "Git batches build one isolated tree and advance one ref regardless of operation count.",
        related = ["write_note", "delete_note", "move_note"],
        examples = [
            r#"[{"type":"write","path":"note1.md","content":"..."}]"#,
            r#"[{"type":"delete","path":"old.md"},{"type":"write","path":"new.md","content":"..."}]"#,
            r#"[{"type":"move","from":"a.md","to":"b.md"},{"type":"write","path":"index.md","content":"..."}]"#
        ],
        tags = ["write", "batch"],
        destructive = true,
    )]
    async fn batch_execute(
        &self,
        operations: Vec<BatchOperation>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;

        if operations.is_empty() {
            return Err(McpError::internal(
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
        let mut response = StandardResponse::new(
            vault_name,
            "batch_execute",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_success(result.success)
        .with_count(op_count)
        .with_meta("execution_mode", serde_json::json!(execution_mode))
        .with_next_step("quick_health_check");

        if execution_mode == "sequential_direct" && !result.success {
            response = response.with_warning(
                "Direct batch execution is sequential: operations completed before the failure were not rolled back. Configure write_backend=git for all-or-nothing batches.",
            );
        }

        response.to_json()
    }
}
