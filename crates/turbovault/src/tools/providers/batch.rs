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

#[turbomcp::server(name = "obsidian-vault", version = "1.5.0")]
impl BatchProvider {
    // ==================== Batch Operations ====================

    /// Execute a validated batch of file operations, stopping on first failure.
    #[tool(
        description = "Execute multiple file operations sequentially after conflict validation; stops at the first failure without rolling back earlier operations",
        usage = "Use to reduce round trips for independent file operations. This is fail-fast, not an all-or-nothing transaction: inspect data.success, data.failed_at, and data.changes because earlier operations remain applied when a later operation fails. Not idempotent.",
        performance = "Depends on operation count and types. Operations run sequentially.",
        related = ["write_note", "delete_note", "move_note"],
        examples = [
            r#"[{"type":"write","path":"note1.md","content":"..."}]"#,
            r#"[{"type":"delete","path":"old.md"},{"type":"write","path":"new.md","content":"..."}]"#,
            r#"[{"type":"move","from":"a.md","to":"b.md"},{"type":"write","path":"index.md","content":"..."}]"#
        ],
        tags = ["write", "batch"],
        destructive = true,
    )]
    async fn batch_execute(&self, operations: Vec<BatchOperation>) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;

        if operations.is_empty() {
            return Err(McpError::internal(
                "Batch operations list cannot be empty".to_string(),
            ));
        }

        let op_count = operations.len();
        let tools = BatchTools::new(manager);
        let result = tools
            .batch_execute(operations)
            .await
            .map_err(to_mcp_error)?;

        self.invalidate_similarity_cache().await;
        self.invalidate_search_cache().await;
        let response = StandardResponse::new(
            vault_name,
            "batch_execute",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_success(result.success)
        .with_count(op_count)
        .with_meta("execution_mode", serde_json::json!("sequential_fail_fast"))
        .with_warning("Batch operations completed before a failure are not rolled back.")
        .with_next_step("quick_health_check");

        response.to_json()
    }
}
