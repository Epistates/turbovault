//! TemplateProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct TemplateProvider(CoreToolHandler);

impl TemplateProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for TemplateProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "2.0.0")]
impl TemplateProvider {
    // ==================== Templates (LLM Note Creation) ====================

    /// List available templates
    #[tool(
        description = "List all available note templates in the active vault",
        tags = ["read", "template"],
        read_only = true,
    )]
    async fn list_templates(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let templates = engine.list_templates();

        let count = templates.len();
        let response =
            StandardResponse::new(vault_name, "list_templates", serde_json::json!(templates))
                .with_count(count);

        response.to_json()
    }

    /// Get template details
    #[tool(
        description = "Get detailed information about a specific template including fields and preview",
        tags = ["read", "template"],
        read_only = true,
    )]
    async fn get_template(&self, template_id: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let template = engine
            .get_template(&template_id)
            .ok_or_else(|| McpError::internal(format!("Template {} not found", template_id)))?;

        let response = StandardResponse::new(
            vault_name,
            "get_template",
            serde_json::to_value(&template).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("create_from_template");

        response.to_json()
    }

    /// Create note from template
    #[tool(
        description = "Create a new note from a template with field substitution and frontmatter",
        tags = ["write", "template"],
        destructive = true,
    )]
    async fn create_from_template(
        &self,
        template_id: String,
        file_path: String,
        fields: String, // JSON string
        expected_hash: Option<String>,
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);

        // Parse fields JSON
        let field_values: HashMap<String, String> = serde_json::from_str(&fields)
            .map_err(|e| McpError::invalid_request(format!("Invalid fields JSON: {}", e)))?;

        let message = self
            .resolve_commit_message(commit_message, || {
                format!("create_from_template {template_id} -> {file_path}")
            })
            .await?;
        let result = engine
            .create_from_template(
                &template_id,
                &file_path,
                field_values,
                // Sentinel-or-oid `expected_hash` (qae.6.4); omitted → the
                // strict-create default `ExpectAbsent`.
                turbovault_core::Precondition::from_wire(
                    expected_hash.as_deref(),
                    turbovault_core::Precondition::ExpectAbsent,
                ),
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::Created {
                path: file_path.clone(),
            },
            WriteAttribution::host("create_from_template"),
        )
        .await;
        let response = StandardResponse::new(
            vault_name,
            "create_from_template",
            serde_json::to_value(&result).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("read_note")
        .with_next_step("find_notes_from_template");

        response.to_json()
    }

    /// Find notes created from template
    #[tool(
        description = "Find all notes created from a specific template via frontmatter tracking",
        tags = ["read", "template"],
        read_only = true,
    )]
    async fn find_notes_from_template(&self, template_id: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let notes = engine
            .find_notes_from_template(&template_id)
            .await
            .map_err(to_mcp_error)?;

        let count = notes.len();
        let response = StandardResponse::new(
            vault_name,
            "find_notes_from_template",
            serde_json::json!(notes),
        )
        .with_count(count);

        response.to_json()
    }
}
