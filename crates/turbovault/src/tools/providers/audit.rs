//! AuditProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct AuditProvider(CoreToolHandler);

impl AuditProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for AuditProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl AuditProvider {
    // ─── AUDIT TOOLS ─────────────────────────────────────────────────

    #[tool(
        description = "View operation history for the active vault with optional filters by path, operation type (CREATE/UPDATE/DELETE/MOVE), and result limit",
        tags = ["read", "audit"],
        read_only = true,
    )]
    async fn audit_log(
        &self,
        path: Option<String>,
        operation: Option<String>,
        limit: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("audit_log").await?;
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let audit_tools = self.get_audit_tools().await?;

        let mut filter = AuditFilter::new().with_limit(limit.unwrap_or(50));
        if let Some(p) = path {
            filter = filter.with_path(p);
        }
        if let Some(op) = operation {
            let op_type = match op.to_uppercase().as_str() {
                "CREATE" => OperationType::Create,
                "UPDATE" => OperationType::Update,
                "DELETE" => OperationType::Delete,
                "MOVE" => OperationType::Move,
                "ROLLBACK" => OperationType::Rollback,
                _ => {
                    return Err(McpError::internal(format!(
                        "Unknown operation type: {}. Use CREATE, UPDATE, DELETE, MOVE, or ROLLBACK",
                        op
                    )));
                }
            };
            filter = filter.with_operation(op_type);
        }

        let entries = audit_tools.query_log(&filter).await.map_err(to_mcp_error)?;
        let count = entries.len();
        StandardResponse::new(
            &vault_name,
            "audit_log",
            serde_json::to_value(&entries).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["rollback_preview", "diff_note_version", "audit_stats"])
        .to_json()
    }

    #[tool(
        description = "Preview what a rollback would change without applying it (dry run). Shows unified diff between current content and rollback target",
        tags = ["read", "audit"],
        read_only = true,
    )]
    async fn rollback_preview(&self, operation_id: String) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("rollback_preview").await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;
        let vault_path = manager.vault_path().clone();
        let result = audit_tools
            .rollback_preview(&operation_id, &vault_path)
            .await
            .map_err(to_mcp_error)?;
        let mut response = StandardResponse::new(
            &vault_name,
            "rollback_preview",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["rollback_note", "audit_log"]);
        for w in &result.warnings {
            response = response.with_warning(w.clone());
        }
        response.to_json()
    }

    #[tool(
        description = "Restore a note to its state before a specific operation (identified by operation_id from audit_log)",
        tags = ["write", "audit"],
        destructive = true,
    )]
    async fn rollback_note(&self, operation_id: String) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("rollback_note").await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;
        let vault_path = manager.vault_path().clone();
        let result = audit_tools
            .rollback_execute(&operation_id, &vault_path)
            .await
            .map_err(to_mcp_error)?;

        // A rollback restores prior content or removes a created note; either
        // way the path must be re-read by anything tracking vault state.
        self.after_write_one(
            &vault_name,
            VaultChange::Modified {
                path: result.path.clone(),
            },
            WriteAttribution::host("rollback_note"),
        )
        .await;

        // Synchronize disk, parsed cache, and graph after either a restore or delete rollback.
        let restored_path = std::path::PathBuf::from(&result.path);
        if let Err(error) = manager.refresh_file_state(&restored_path).await {
            log::warn!(
                "Failed to refresh manager state after rollback of {}: {}",
                result.path,
                error
            );
        }

        StandardResponse::new(
            &vault_name,
            "rollback_note",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["read_note", "audit_log"])
        .to_json()
    }

    #[tool(
        description = "Get audit trail statistics including operation counts by type, total snapshot storage used, and time range of recorded operations",
        tags = ["read", "audit"],
        read_only = true,
    )]
    async fn audit_stats(&self) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("audit_stats").await?;
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let audit_tools = self.get_audit_tools().await?;
        let result = audit_tools.stats().await.map_err(to_mcp_error)?;
        StandardResponse::new(
            &vault_name,
            "audit_stats",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_duration(start.elapsed().as_millis() as u64)
        .with_next_steps(&["audit_log", "explain_vault"])
        .to_json()
    }
}
