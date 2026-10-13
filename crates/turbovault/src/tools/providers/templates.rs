//! TemplateProvider MCP capabilities.

use std::ops::Deref;

use turbovault_tools::TemplateDefinition;

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

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl TemplateProvider {
    // ==================== Templates (LLM Note Creation) ====================

    /// List available templates
    #[tool(
        description = "List the note templates create_from_template can use, each with its id, name, fields (which are required, their types and defaults) and the note it produces. Give template_id to get just that one.",
        title = "List Templates",
        tags = ["read", "template"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<Vec<TemplateDefinition>>,
    )]
    async fn list_templates(
        &self,
        #[description("One template's id, to get only that template. Omit to list them all.")]
        template_id: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let templates = match template_id {
            Some(template_id) => vec![engine.get_template(&template_id).ok_or_else(|| {
                McpError::new(
                    turbomcp_core::error::ErrorKind::ResourceNotFound,
                    format!(
                        "Template {template_id} not found. Call list_templates without an id to see them"
                    ),
                )
            })?],
            None => engine.list_templates(),
        };

        let count = templates.len();
        StandardResponse::new(vault_name, "list_templates", templates)
            .with_count(count)
            .with_next_step("create_from_template")
            .to_json()
    }

    /// Create note from template
    #[tool(
        description = "Create a note from a template, filling in its fields and recording the template in the note's frontmatter. Refuses to replace an existing note unless expected_hash says so.",
        title = "Create Note from Template",
        tags = ["write", "template"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<turbovault_tools::templates::CreatedNoteInfo>,
    )]
    async fn create_from_template(
        &self,
        #[description("The template's id, as list_templates shows it.")] template_id: String,
        #[description(
            "Vault-relative path for the new note, including the extension, such as `meetings/2026-10-13.md`."
        )]
        file_path: String,
        #[description(
            "The field values as a JSON object string, such as `{\"title\": \"Weekly sync\", \"date\": \"2026-10-13\"}`. list_templates lists the fields."
        )]
        fields: String, // JSON string
        #[description(
            "Default `absent`, so an existing note is never replaced. Pass the existing note's `hash` from read_note to replace that version, or `blind` to replace whatever is there."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);

        // Parse fields JSON
        let field_values: HashMap<String, String> = serde_json::from_str(&fields)
            .map_err(|e| McpError::invalid_params(format!("Invalid fields JSON: {}", e)))?;

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
        let response = StandardResponse::new(vault_name, "create_from_template", result)
            .with_next_step("read_note")
            .with_next_step("find_notes_from_template");

        response.to_json()
    }

    /// Find notes created from template
    #[tool(
        description = "List the notes created from a template, found by the template id create_from_template records in their frontmatter.",
        title = "Find Notes from Template",
        tags = ["read", "template"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<Vec<String>>,
    )]
    async fn find_notes_from_template(
        &self,
        #[description("The template's id, as list_templates shows it.")] template_id: String,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = TemplateEngine::new(manager);
        let notes = engine
            .find_notes_from_template(&template_id)
            .await
            .map_err(to_mcp_error)?;

        let count = notes.len();
        let response =
            StandardResponse::new(vault_name, "find_notes_from_template", notes).with_count(count);

        response.to_json()
    }
}
