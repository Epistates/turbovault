//! FileProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct FileProvider(CoreToolHandler);

impl FileProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for FileProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Response payload for a partial `read_note`.
///
/// [`SliceResult`] is flattened in, so its `skip_serializing_if` attributes
/// still apply: a line slice carries `returned_lines` and no section fields, a
/// section slice the reverse. Note the absence of `hash` — see `read_note`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct PartialReadPayload {
    path: String,
    uri: String,
    #[serde(flatten)]
    slice: SliceResult,
}

/// Response payload for a whole `read_note`, with the `hash` a later write
/// passes back as `expected_hash`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct WholeReadPayload {
    path: String,
    content: String,
    hash: String,
    uri: String,
}

/// What `read_note` returns: the whole note, or the part asked for.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
enum ReadNotePayload {
    Whole(WholeReadPayload),
    Part(PartialReadPayload),
}

/// Response payload for `write_note`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct WrittenPayload {
    path: String,
    /// Always `written`.
    status: &'static str,
    bytes: usize,
    mode: String,
}

/// Response payload for `delete_note`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct DeletedPayload {
    path: String,
    /// Always `deleted`.
    status: &'static str,
    /// Notes whose links to it were struck through.
    link_sources_updated: Vec<String>,
}

/// Response payload for `move_note` and `move_file`.
#[derive(serde::Serialize, schemars::JsonSchema)]
pub(super) struct MovedPayload {
    pub(super) from: String,
    pub(super) to: String,
    /// Always `moved`.
    pub(super) status: &'static str,
    /// Notes whose links were rewritten to follow it. `move_file` leaves
    /// links alone and omits this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) link_sources_updated: Option<Vec<String>>,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl FileProvider {
    // ==================== File Operations ====================

    /// Read the contents of a note, optionally only part of it
    #[tool(
        description = "Read a note's markdown, whole or in part. A whole read also returns its `hash`, which write_note, edit_note and the other write tools take as expected_hash so a change made since is not overwritten. Use at most one selector (head_lines, tail_lines, or the section ones); a partial read returns no hash.",
        title = "Read Note",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ReadNotePayload>,
    )]
    async fn read_note(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("Return only the first this many lines.")] head_lines: Option<usize>,
        #[description("Return only the last this many lines.")] tail_lines: Option<usize>,
        #[description(
            "Return sections that start at headings of this level, 1 to 6. Combine with heading_equals or last_sections to narrow them."
        )]
        heading_level: Option<u8>,
        #[description(
            "Return the section under the heading with exactly this text, without its `#` markers, such as `Next steps`. Case-sensitive."
        )]
        heading_equals: Option<String>,
        #[description(
            "Return only the last this many of the selected sections, such as the latest entries of a log."
        )]
        last_sections: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = FileTools::new(manager);
        let content = tools.read_file(&path).await.map_err(to_mcp_error)?;
        let uri = obsidian_uri(&vault_name, &path);

        // `SliceSpec::first_sections` stays available to library callers; it is
        // simply not exposed as a tool parameter. `heading_equals` already
        // reaches a named section anywhere in the file, and omitting the count
        // returns every match, so the only phrasing lost is "the first N
        // sections".
        let spec = SliceSpec {
            head_lines,
            tail_lines,
            heading_level,
            heading_equals,
            last_sections,
            ..Default::default()
        };
        // Contradictory selectors are the caller's mistake, not a server fault.
        let slice =
            slice_content(&content, &spec).map_err(|e| McpError::invalid_params(e.to_string()))?;

        // A partial read deliberately returns no `hash`. That token asserts the
        // caller has seen this file's current state, which is false when only
        // part of it was returned: handing it to `write_note` would let a
        // whole-file overwrite satisfy its precondition and silently destroy
        // the unread remainder. `edit_note` needs no token, and a caller that
        // really wants to replace the file can re-read it in full.
        if let Some(slice) = slice {
            let sections_returned = slice.sections.len();
            let mut response = StandardResponse::new(
                &vault_name,
                "read_note",
                ReadNotePayload::Part(PartialReadPayload {
                    path: path.clone(),
                    uri,
                    slice,
                }),
            );
            if sections_returned > 0 {
                response = response.with_count(sections_returned);
            }
            return response
                .with_next_steps(&["edit_note", "get_backlinks"])
                .to_json();
        }

        let hash = self.hash_for_active_backend(&content).await?;
        StandardResponse::new(
            &vault_name,
            "read_note",
            ReadNotePayload::Whole(WholeReadPayload {
                path,
                content,
                hash,
                uri,
            }),
        )
        .with_read_next_steps()
        .to_json()
    }

    /// Write or update a note with optional mode (overwrite, append, prepend)
    #[tool(
        description = "Create a note, replace one, or add text to its start or end. Replacing an existing note needs its expected_hash from read_note, so a change made since isn't lost; creating needs none. For small changes inside a note, edit_note is safer. On a Git-backed vault the write is one commit.",
        title = "Write Note",
        tags = ["write"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<WrittenPayload>,
    )]
    async fn write_note(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`. Missing folders are created."
        )]
        path: String,
        #[description("The markdown to write, frontmatter included if the note has any.")]
        content: String,
        #[description(
            "`overwrite` (default) writes the whole note; `append` and `prepend` add the content at the end or start of an existing note."
        )]
        mode: Option<String>,
        #[description(
            "The note's `hash` from your last read_note. Required to overwrite an existing note, and the write is refused if the note changed since. `blind` overwrites without checking; `absent` insists the note doesn't exist yet."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let write_mode = WriteMode::from_str_opt(mode.as_deref()).map_err(to_mcp_error)?;
        let prepared = self
            .prepare_complete_note_write(&path, commit_message, "write_note")
            .await?;
        let vault_name = prepared.vault_name;
        let manager = prepared.manager;
        let message = prepared.message;
        let files = FileTools::new(manager.clone());
        // Captured before the write so the change feed can report a creation
        // versus a modification. `prepared.manager` has already moved.
        let existed = CoreToolHandler::path_exists(&manager, &path).await;

        // Sentinel-or-oid `expected_hash` (qae.6.4), replacing the retired
        // `force`. The default when the param is omitted is mode-dependent: a full
        // Overwrite creates-by-default (`ExpectAbsent`, the turbovault-947
        // no-clobber backstop on both substrates); append/prepend are in-place
        // (`ExpectExists`). `expected_hash="blind"` is the explicit blind overwrite.
        let default = match write_mode {
            WriteMode::Overwrite => turbovault_core::Precondition::ExpectAbsent,
            _ => turbovault_core::Precondition::ExpectExists,
        };
        let precondition =
            turbovault_core::Precondition::from_wire(expected_hash.as_deref(), default);

        // Friendly pre-check for the create-by-default no-clobber case. A
        // create-on-present is the unified "changed underneath us" refusal
        // (`ConcurrencyError`), same kind as the substrate's `ExpectAbsent`
        // backstop — surfaced here with an actionable message.
        if precondition == turbovault_core::Precondition::ExpectAbsent
            && write_mode == WriteMode::Overwrite
            && tokio::fs::try_exists(manager.vault_path().join(&path))
                .await
                .unwrap_or(false)
        {
            return Err(to_mcp_error(turbovault_core::Error::concurrency_error(
                format!(
                    "'{path}' exists — pass expected_hash to overwrite, or expected_hash=\"blind\" for a blind overwrite."
                ),
            )));
        }
        files
            .write_file_with_mode(&path, &content, write_mode, precondition, &message)
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::written(&path, existed),
            WriteAttribution::host("write_note"),
        )
        .await;
        StandardResponse::new(
            vault_name,
            "write_note",
            WrittenPayload {
                path,
                status: "written",
                bytes: content.len(),
                mode: mode.unwrap_or_else(|| "overwrite".to_string()),
            },
        )
        .with_write_next_steps()
        .to_json()
    }

    /// Edit note using SEARCH/REPLACE blocks
    #[tool(
        description = "Change parts of a note with SEARCH/REPLACE blocks, leaving the rest as it is. Safer than write_note for small changes, since only the matched text changes. Each SEARCH text must match once in the note; close matches (whitespace, indentation) are accepted and reported as warnings.",
        title = "Edit Note",
        tags = ["write"],
        read_only = false,
        destructive = true,
        idempotent = false,
        open_world = false,
        output_schema = StandardResponse<turbovault_vault::EditResult>,
    )]
    async fn edit_note(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description(
            "One or more blocks of the form `<<<<<<< SEARCH`, the exact text to find, `=======`, the text to put instead, `>>>>>>> REPLACE`, each marker on its own line. To edit text containing a `=======` line, make all three markers 8 or more characters long."
        )]
        edits: String,
        #[description(
            "The note's `hash` from your last read_note; the edit is refused if the note changed since. Omit to edit whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description("Report what the edit would change without writing it. Default false.")]
        dry_run: Option<bool>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
        commit_message: Option<String>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let dry_run = dry_run.unwrap_or(false);
        let message = self
            .resolve_commit_message(commit_message, || format!("edit_note {path}"))
            .await?;
        let result = FileTools::new(manager)
            .edit_file(
                &path,
                &edits,
                turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                dry_run,
                &message,
            )
            .await
            .map_err(to_mcp_error)?;

        self.after_write_one(
            &vault_name,
            VaultChange::Modified { path: path.clone() },
            WriteAttribution::host("edit_note"),
        )
        .await;
        StandardResponse::new(vault_name, "edit_note", result)
            .with_next_steps(&["read_note", "write_note"])
            .to_json()
    }

    /// Delete a note (confirmation-protected)
    #[tool(
        description = "Delete a note. On a Git-backed vault it refuses when other notes link to it, unless on_backlinks is `rewrite-stale-callout` (strike those links through in the same commit) or force is true (leave them broken). On a plain-folder vault force defaults to true. get_backlinks shows what links to it.",
        title = "Delete Note",
        tags = ["write", "delete"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<DeletedPayload>,
    )]
    async fn delete_note(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `drafts/Old.md`."
        )]
        path: String,
        #[description("The same path again, to confirm the deletion.")] confirm_path: String,
        #[description(
            "The note's `hash` from your last read_note; the delete is refused if the note changed since. Omit to delete whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the path."
        )]
        commit_message: Option<String>,
        #[description(
            "Delete even when other notes link to it, leaving their links broken. Default false on a Git-backed vault, true on a plain-folder vault."
        )]
        force: Option<bool>,
        #[description(
            "`rewrite-stale-callout` to strike through the links to it in other notes, as part of the same delete."
        )]
        on_backlinks: Option<String>,
    ) -> McpResult<serde_json::Value> {
        // Safety: confirm_path must match path exactly
        if path != confirm_path {
            return Err(McpError::invalid_params(format!(
                "Confirmation failed: confirm_path '{}' does not match path '{}'. Both must be identical to proceed with deletion.",
                confirm_path, path
            )));
        }

        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        let is_git = self.active_vault_is_git().await?;
        let message = self
            .resolve_commit_message(commit_message, || format!("delete_note {path}"))
            .await?;
        let force = force.unwrap_or(!is_git);
        let files = FileTools::new(manager.clone());
        let backlinks = if force {
            Vec::new()
        } else {
            inbound_backlinks(&manager, &path).await?
        };

        let updated_sources = if backlinks.is_empty() || force {
            files
                .delete_file(
                    &path,
                    turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
            Vec::new()
        } else if on_backlinks.as_deref() == Some("rewrite-stale-callout") {
            BatchTools::new(manager)
                .delete_file_with_link_rewrite_to_stale(&path, expected_hash.as_deref(), &message)
                .await
                .map_err(to_mcp_error)?
        } else {
            return Err(McpError::invalid_params(format!(
                "delete_note refused: '{path}' has {} inbound backlink(s): {}. Pass on_backlinks='rewrite-stale-callout' or force=true.",
                backlinks.len(),
                backlinks
                    .iter()
                    .take(5)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        };

        // The deletion itself plus every note whose links were rewritten to a
        // stale-link callout in the same operation.
        let changes = std::iter::once(VaultChange::Deleted { path: path.clone() }).chain(
            updated_sources.iter().map(|source| VaultChange::Modified {
                path: source.clone(),
            }),
        );
        self.after_write(&vault_name, changes, WriteAttribution::host("delete_note"))
            .await;
        StandardResponse::new(
            vault_name,
            "delete_note",
            DeletedPayload {
                path,
                status: "deleted",
                link_sources_updated: updated_sources,
            },
        )
        .with_next_step("quick_health_check")
        .to_json()
    }

    /// Move or rename a note
    #[tool(
        description = "Move or rename a note. Git-backed vaults update inbound links (wikilinks and Markdown links) atomically by default; set update_backlinks=false for a rename-only operation. A note already at the destination is never replaced unless dest_expected_hash says so. For attachments and other files, use move_file.",
        title = "Move Note",
        tags = ["write"],
        read_only = false,
        destructive = true,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<MovedPayload>,
    )]
    async fn move_note(
        &self,
        #[description(
            "Vault-relative path of the note to move, including the extension, such as `inbox/Plan.md`."
        )]
        from: String,
        #[description(
            "Vault-relative path to move it to, including the extension, such as `projects/Plan.md`. Missing folders are created."
        )]
        to: String,
        #[description(
            "The note's `hash` from your last read_note; the move is refused if it changed since. Omit to move whatever is there now."
        )]
        expected_hash: Option<String>,
        #[description(
            "What must be at the destination. Default `absent`, so an existing note is never replaced; a note's `hash` allows replacing that exact version, and `blind` replaces anything."
        )]
        dest_expected_hash: Option<String>,
        #[description(
            "Commit message on a Git-backed vault. Required there when the vault sets git.require_commit_message; otherwise derived from the paths."
        )]
        commit_message: Option<String>,
        #[description(
            "Rewrite links to the note in other notes so they follow it. Default true on a Git-backed vault, false on a plain-folder vault."
        )]
        update_backlinks: Option<bool>,
    ) -> McpResult<serde_json::Value> {
        let vault_name = self.get_active_vault_name().await?;
        let manager = self.get_active_vault_manager().await?;
        let is_git = self.active_vault_is_git().await?;
        let message = self
            .resolve_commit_message(commit_message, || format!("move_note {from} -> {to}"))
            .await?;
        let update_backlinks = update_backlinks.unwrap_or(is_git);
        let updated_sources = if update_backlinks {
            // `move_file_with_link_updates` flushes the reindex queue itself so
            // the backlink resolution reads a coherent link graph.
            BatchTools::new(manager)
                .move_file_with_link_updates(
                    &from,
                    &to,
                    expected_hash.as_deref(),
                    dest_expected_hash.as_deref(),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?
        } else {
            FileTools::new(manager)
                .move_file(
                    &from,
                    &to,
                    turbovault_core::Precondition::for_in_place(expected_hash.as_deref()),
                    // Sentinel-or-oid dest precondition (qae.6.4); omitted →
                    // `ExpectAbsent`, the clobber guard.
                    turbovault_core::Precondition::from_wire(
                        dest_expected_hash.as_deref(),
                        turbovault_core::Precondition::ExpectAbsent,
                    ),
                    &message,
                )
                .await
                .map_err(to_mcp_error)?;
            Vec::new()
        };

        let changes = std::iter::once(VaultChange::Renamed {
            from: from.clone(),
            to: to.clone(),
        })
        .chain(updated_sources.iter().map(|source| VaultChange::Modified {
            path: source.clone(),
        }));
        self.after_write(&vault_name, changes, WriteAttribution::host("move_note"))
            .await;
        let response = StandardResponse::new(
            vault_name,
            "move_note",
            MovedPayload {
                from,
                to,
                status: "moved",
                link_sources_updated: Some(updated_sources),
            },
        )
        .with_next_steps(&["get_backlinks", "get_forward_links"]);
        if update_backlinks {
            response.to_json()
        } else {
            response
                .with_warning(
                    "Links pointing to the old path were not updated and may now be broken.",
                )
                .to_json()
        }
    }
}

/// Vault-relative source paths whose wikilinks target `path` (inbound
/// backlinks), resolved through the manager's self-flushing link graph
/// (`get_backlinks`). Backs `delete_note`'s refuse-by-default listing — the
/// M4d replacement for the old `WriteTools::list_inbound_backlinks`.
///
/// Passes the gate undebounced first: a link added in Obsidian a moment ago
/// is exactly what the refusal is there to protect.
async fn inbound_backlinks(manager: &VaultManager, path: &str) -> McpResult<Vec<String>> {
    manager.ensure_fresh_now().await;
    let full = manager
        .get_backlinks(std::path::Path::new(path))
        .await
        .map_err(to_mcp_error)?;
    Ok(full
        .into_iter()
        .map(|p| manager.relative_path(&p))
        .collect())
}
