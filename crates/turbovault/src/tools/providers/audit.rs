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

/// What `audit_log` returns: a page of entries, or the trail's statistics.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
enum AuditLogPayload {
    /// The newest matching entries first.
    Entries(Vec<turbovault_audit::AuditEntry>),
    /// With `summary: true`.
    Summary(turbovault_audit::AuditStats),
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl AuditProvider {
    // ─── AUDIT TOOLS ─────────────────────────────────────────────────

    #[tool(
        description = "List the changes TurboVault made to the vault, newest first: each operation's id, type, path, time and content hashes. Use an id with diff_notes (operation_id), rollback_preview or rollback_note. With `summary: true`, return the trail's statistics instead: operations of each type, snapshot storage used, the time span covered and whether the entries' hash chain is intact. Not available on Git-backed vaults, whose history is in Git.",
        title = "Audit Log",
        tags = ["read", "audit"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<AuditLogPayload>,
    )]
    async fn audit_log(
        &self,
        #[description(
            "Only operations on paths containing this text, such as `projects/` or `Plan.md`."
        )]
        path: Option<String>,
        #[description(
            "Only operations of this type: `CREATE`, `UPDATE`, `DELETE`, `MOVE` or `ROLLBACK`."
        )]
        operation: Option<String>,
        #[description(
            "Return the trail's statistics instead of its entries. Default false; the filters and paging don't apply to it."
        )]
        summary: Option<bool>,
        #[description("Most entries to return. Default 50, maximum 1000.")] limit: Option<usize>,
        #[description(
            "How many of the newest matching entries to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("audit_log").await?;
        let start = std::time::Instant::now();
        let vault_name = self.get_active_vault_name().await?;
        let audit_tools = self.get_audit_tools().await?;

        if summary == Some(true) {
            let stats = audit_tools.stats().await.map_err(to_mcp_error)?;
            return StandardResponse::new(
                &vault_name,
                "audit_log",
                AuditLogPayload::Summary(stats),
            )
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["audit_log", "explain_vault"])
            .to_json();
        }

        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_AUDIT_ENTRIES);
        let mut filter = AuditFilter::new().with_limit(if request.limit == 0 {
            0
        } else {
            request.fetch()
        });
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
                    return Err(McpError::invalid_params(format!(
                        "Unknown operation type: {}. Use CREATE, UPDATE, DELETE, MOVE, or ROLLBACK",
                        op
                    )));
                }
            };
            filter = filter.with_operation(op_type);
        }

        let entries = audit_tools.query_log(&filter).await.map_err(to_mcp_error)?;
        let (entries, page) = request.slice_ranked(entries);
        StandardResponse::new(&vault_name, "audit_log", AuditLogPayload::Entries(entries))
            .with_page(page)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["rollback_preview", "diff_notes"])
            .to_json()
    }

    #[tool(
        description = "Show what rollback_note would do for an operation, without doing it: the diff between the note now and the content it would be restored to, and any warnings. Not available on Git-backed vaults.",
        title = "Preview Rollback",
        tags = ["read", "audit"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<turbovault_audit::RollbackPreview>,
    )]
    async fn rollback_preview(
        &self,
        #[description("The `id` of the audit_log entry to undo.")] operation_id: String,
    ) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("rollback_preview").await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let audit_tools = self.get_audit_tools().await?;
        let vault_path = manager.vault_path().clone();
        let result = audit_tools
            .rollback_preview(&operation_id, &vault_path)
            .await
            .map_err(to_mcp_error)?;
        let mut response = StandardResponse::new(&vault_name, "rollback_preview", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["rollback_note", "audit_log"]);
        for w in &result.warnings {
            response = response.with_warning(w.clone());
        }
        response.to_json()
    }

    #[tool(
        description = "Undo one operation from the audit trail: restore the note to its content before it, or remove a note it created. Refuses if the note changed since that operation, since the later change would be lost; check rollback_preview, then pass force to roll back anyway (a client that supports elicitation asks the user to confirm). A rollback is itself recorded, so it can be rolled back. Not available on Git-backed vaults.",
        title = "Roll Back Note",
        tags = ["write", "audit"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<turbovault_audit::RollbackResult>,
    )]
    async fn rollback_note(
        &self,
        #[description("The `id` of the audit_log entry to undo.")] operation_id: String,
        #[description(
            "Roll back even though the note changed since that operation, discarding the later change. Default false."
        )]
        force: Option<bool>,
        ctx: &RequestContext,
    ) -> McpResult<serde_json::Value> {
        self.refuse_audit_on_git_backend("rollback_note").await?;
        let start = std::time::Instant::now();
        let (vault_name, manager) = self.get_vault_pair().await?;
        if force == Some(true) {
            self.confirm_destructive(
                ctx,
                format!(
                    "Roll back operation `{operation_id}` in the vault `{vault_name}` even though the note changed since? The later changes will be lost."
                ),
            )
            .await?;
        }
        let audit_tools = self.get_audit_tools().await?;
        let result = audit_tools
            .rollback(&manager, &operation_id, force.unwrap_or(false))
            .await
            .map_err(|error| match error.category() {
                // A note that changed since the operation: the refusal
                // already says to check rollback_preview and pass force, and
                // the generic "re-read and retry with expected_hash" doesn't
                // apply to a tool that takes no hash.
                ErrorCategory::Conflict => McpError::invalid_params(error.to_string())
                    .with_data(serde_json::json!({ "category": "conflict" })),
                _ => to_mcp_error(error),
            })?;

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
            tracing::warn!(
                "Failed to refresh manager state after rollback of {}: {}",
                result.path,
                error
            );
        }

        StandardResponse::new(&vault_name, "rollback_note", &result)
            .with_duration(start.elapsed().as_millis() as u64)
            .with_next_steps(&["read_note", "audit_log"])
            .to_json()
    }
}
