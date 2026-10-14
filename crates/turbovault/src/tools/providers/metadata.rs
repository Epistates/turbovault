//! MetadataProvider MCP capabilities.

use std::collections::BTreeMap;
use std::ops::Deref;

use turbovault_tools::NoteInfo;

use super::super::*;
use super::files::MovedPayload;

// The metadata tools build their results as JSON in `MetadataTools`; these
// types are the schemas those results are advertised with, and the output
// schema test holds the two together.

/// What `query_metadata` returns.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct MetadataQueryResult {
    query: String,
    /// How many notes matched, across every page.
    matched: usize,
    /// This page of the matches.
    files: Vec<MetadataMatch>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct MetadataMatch {
    path: String,
    metadata: serde_json::Map<String, serde_json::Value>,
}

/// What `update_frontmatter` returns.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct FrontmatterUpdate {
    path: String,
    status: String,
    merge: bool,
    keys_set: Vec<String>,
}

/// What `manage_tags` returns: the frontmatter tags left after the change.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct TagsChanged {
    path: String,
    operation: String,
    tags: Vec<String>,
    /// `updated`, or `no_frontmatter` for a remove from a note without any.
    status: String,
}

/// One note as `get_notes_info` reports it.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct NoteDetails {
    #[serde(flatten)]
    info: NoteInfo,
    /// The frontmatter values asked for with `frontmatter_keys`, each `null`
    /// when the note has no such key. Absent unless asked for, and for a note
    /// that doesn't exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    frontmatter: Option<BTreeMap<String, serde_json::Value>>,
    /// The note's tags, when asked for with `include_tags`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tags: Option<NoteTags>,
}

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct NoteTags {
    /// The frontmatter `tags` list.
    frontmatter_tags: Vec<String>,
    /// `#tags` written in the body.
    inline_tags: Vec<String>,
    all_tags: Vec<String>,
}

#[derive(Clone)]
pub(super) struct MetadataProvider(CoreToolHandler);

impl MetadataProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for MetadataProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl MetadataProvider {
    // ==================== Metadata Operations ====================

    /// Query files by metadata pattern
    #[tool(
        description = "Find notes whose frontmatter matches one simple condition, with each match's frontmatter. For several conditions, joins or sorting use query_frontmatter_sql; to find notes by keyword, tag or a field's text, use search.",
        title = "Query Metadata",
        tags = ["read", "frontmatter"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<MetadataQueryResult>,
    )]
    async fn query_metadata(
        &self,
        #[description(
            "One condition: `status: \"draft\"` (equals), `tags: contains(\"work\")` (text contains), `priority > 3` or `priority < 3` (numbers)."
        )]
        pattern: String,
        #[description("Most notes to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many matches to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let results = tools.query_metadata(&pattern).await.map_err(to_mcp_error)?;

        let mut result_data =
            serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let page = request.slice_field(&mut result_data, "files", false);

        let response = StandardResponse::new(vault_name, "query_metadata", result_data)
            .with_page(page)
            .with_meta("pattern", serde_json::json!(pattern));

        response.to_json()
    }

    /// Update frontmatter of a note without touching content
    #[tool(
        description = "Change a note's YAML frontmatter and leave its body untouched. By default the given keys are merged into the existing frontmatter; set merge to false to replace it entirely. For tags, manage_tags adds or removes single tags.",
        title = "Update Frontmatter",
        tags = ["write", "frontmatter"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<FrontmatterUpdate>,
    )]
    async fn update_frontmatter(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description(
            "The frontmatter keys and values to write, such as `{\"status\": \"done\", \"tags\": [\"work\"]}`. A key set to null is written as null."
        )]
        frontmatter: HashMap<String, serde_json::Value>,
        #[description(
            "True (default) to merge these keys into the existing frontmatter; false to replace the whole frontmatter with them."
        )]
        merge: Option<bool>,
        #[description(
            "The note's `hash` from your last read_note; the change is refused if the note changed since. Omit to change whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager);
        let fm_map: serde_json::Map<String, serde_json::Value> = frontmatter.into_iter().collect();
        let message = self
            .resolve_commit_message(commit_message, || format!("update_frontmatter {path}"))
            .await?;
        let result = tools
            .update_frontmatter(
                &path,
                fm_map,
                merge.unwrap_or(true),
                // Sentinel-or-oid `expected_hash` (qae.6.4); omitted → the
                // in-place default `ExpectExists`.
                turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::Modified { path: path.clone() },
            WriteAttribution::host("update_frontmatter"),
        )
        .await;
        StandardResponse::new(vault_name, "update_frontmatter", result)
            .with_next_steps(&["read_note", "query_metadata"])
            .to_json()
    }

    /// Add or remove tags on a note
    #[tool(
        description = "Add or remove tags in a note's frontmatter `tags` list, creating the list if needed. Inline #tags in the body are left alone. To see a note's tags, use get_notes_info with include_tags.",
        title = "Manage Tags",
        tags = ["write", "frontmatter"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<TagsChanged>,
    )]
    async fn manage_tags(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("`add` or `remove`.")] operation: String,
        #[description(
            "Tags to add or remove, without `#`, such as `[\"work\", \"project/alpha\"]`."
        )]
        tags: Vec<String>,
        #[description(
            "The note's `hash` from your last read_note; the change is refused if the note changed since. Omit to change whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        if operation != "add" && operation != "remove" {
            return Err(McpError::invalid_params(format!(
                "operation must be `add` or `remove`, not {operation:?}. To list a note's tags, use get_notes_info with include_tags"
            )));
        }
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = MetadataTools::new(manager.clone());

        // A remove from a note without frontmatter changes nothing, and must
        // not demand a commit message on a require-message vault.
        let (maybe_content, result) = tools
            .compute_manage_tags(&path, &operation, Some(&tags))
            .await
            .map_err(to_mcp_error)?;

        if let Some(content) = maybe_content {
            let message = self
                .resolve_commit_message(commit_message, || {
                    format!("manage_tags {operation} {path}")
                })
                .await?;
            manager
                .write_file(
                    std::path::Path::new(&path),
                    &content,
                    // Sentinel-or-oid `expected_hash` (qae.6.4); omitted →
                    // in-place default `ExpectExists`.
                    turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
            self.after_write_one(
                &vault_name,
                VaultChange::Modified { path: path.clone() },
                WriteAttribution::host("manage_tags"),
            )
            .await;
        }
        StandardResponse::new(vault_name, "manage_tags", result)
            .with_next_steps(&["update_frontmatter", "get_notes_info"])
            .to_json()
    }

    /// Lightweight facts about several notes, without reading their content.
    #[tool(
        description = "Check several notes at once without reading them: whether each exists, its size, when it was modified and whether it has frontmatter. Optionally also chosen frontmatter values (`frontmatter_keys`) and the note's tags (`include_tags`). Use read_note for a note's full content and frontmatter.",
        title = "Get Notes Info",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<Vec<NoteDetails>>,
    )]
    async fn get_notes_info(
        &self,
        #[description(
            "Vault-relative paths of the notes, including extensions, such as `[\"projects/Plan.md\", \"Inbox.md\"]`."
        )]
        paths: Vec<String>,
        #[description(
            "Frontmatter keys to read from each note, such as `[\"status\", \"author.name\"]` (dots for nested keys). Omit for none."
        )]
        frontmatter_keys: Option<Vec<String>>,
        #[description(
            "Also list each note's tags: its frontmatter tags and inline #tags. Default false."
        )]
        include_tags: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let infos = FileTools::new(manager.clone())
            .get_notes_info(&paths)
            .await
            .map_err(to_mcp_error)?;
        let metadata = MetadataTools::new(manager);
        let keys = frontmatter_keys.unwrap_or_default();
        let include_tags = include_tags.unwrap_or(false);

        let mut results = Vec::with_capacity(infos.len());
        for info in infos {
            let (frontmatter, tags) = if info.exists {
                let frontmatter = if keys.is_empty() {
                    None
                } else {
                    let mut values = BTreeMap::new();
                    for key in &keys {
                        // A key the note doesn't have is `null`, so one
                        // missing key doesn't fail a lookup across notes.
                        let value = match metadata.get_metadata_value(&info.path, key).await {
                            Ok(value) => value
                                .get("value")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                            Err(error) if error.category() == ErrorCategory::NotFound => {
                                serde_json::Value::Null
                            }
                            Err(error) => return Err(to_mcp_error(error)),
                        };
                        values.insert(key.clone(), value);
                    }
                    Some(values)
                };
                let tags = if include_tags {
                    let (_, listed) = metadata
                        .compute_manage_tags(&info.path, "list", None)
                        .await
                        .map_err(to_mcp_error)?;
                    Some(
                        serde_json::from_value::<NoteTags>(listed)
                            .map_err(|error| McpError::internal(error.to_string()))?,
                    )
                } else {
                    None
                };
                (frontmatter, tags)
            } else {
                (None, None)
            };
            results.push(NoteDetails {
                info,
                frontmatter,
                tags,
            });
        }

        let count = results.len();
        StandardResponse::new(vault_name, "get_notes_info", results)
            .with_count(count)
            .with_next_step("read_note")
            .to_json()
    }

    /// Move any file within vault (binary-safe, confirmation-protected)
    #[tool(
        description = "Move or rename any file in the vault, such as an image or PDF, byte for byte. Links to it are not updated; to move a note and update the links to it, use move_note. A file already at the destination is replaced.",
        title = "Move File",
        tags = ["write"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<MovedPayload>,
    )]
    async fn move_file(
        &self,
        #[description(
            "Vault-relative path of the file to move, including the extension, such as `attachments/photo.png`."
        )]
        from: String,
        #[description("Vault-relative path to move it to, including the extension.")] to: String,
        #[description("The same path as `from`, repeated to confirm the move.")]
        confirm_from: String,
        #[description("The same path as `to`, repeated to confirm the move.")] confirm_to: String,
        #[description(
            "The file's hash from your last read; the move is refused if it changed since. Omit to move whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the paths."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        // Safety: confirmations must match
        if from != confirm_from {
            return Err(McpError::invalid_params(format!(
                "Confirmation failed: confirm_from '{}' does not match from '{}'. Both must be identical.",
                confirm_from, from
            )));
        }
        if to != confirm_to {
            return Err(McpError::invalid_params(format!(
                "Confirmation failed: confirm_to '{}' does not match to '{}'. Both must be identical.",
                confirm_to, to
            )));
        }

        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        let message = self
            .resolve_commit_message(commit_message, || format!("move_file {from} -> {to}"))
            .await?;
        FileTools::new(manager)
            .move_file(
                &from,
                &to,
                turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                turbovault_core::Precondition::Blind,
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::Renamed {
                from: from.clone(),
                to: to.clone(),
            },
            WriteAttribution::host("move_file"),
        )
        .await;
        StandardResponse::new(
            vault_name,
            "move_file",
            MovedPayload {
                from,
                to,
                status: "moved",
                link_sources_updated: None,
            },
        )
        .to_json()
    }
}
