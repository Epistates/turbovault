//! Workflow prompts: the vault's common multi-step jobs, each written as the
//! instructions an agent needs to do it with TurboVault's tools.
//!
//! A prompt is a user message naming the tools to call, in order, and what to
//! do with what they return. It does no work itself: the agent runs the
//! tools, so it sees every result and can stop or adjust along the way.

use serde_json::Value;
use turbomcp::prelude::*;

/// Values `period` takes in `review_period`.
pub(crate) const PERIODS: &[&str] = &["daily", "weekly", "monthly"];

/// Values `focus` takes in `triage_vault`.
pub(crate) const FOCUSES: &[&str] = &["all", "links", "orphans", "structure"];

/// Every workflow prompt the vault offers.
pub(crate) fn prompts() -> Vec<Prompt> {
    let mut triage = Prompt::new(
        "triage_vault",
        "Survey the vault's health and propose the most valuable fixes: broken links, \
         orphaned and dead-end notes, and structure.",
    )
    .with_optional_arg(
        "focus",
        "What to look at: `all` (default), `links`, `orphans` or `structure`.",
    );
    triage.title = Some("Triage the vault".to_string());

    let mut repair = Prompt::new(
        "repair_broken_links",
        "Find links that point to notes that don't exist and fix each one: repoint it to the \
         note that was meant, or create the missing note when it should exist.",
    )
    .with_optional_arg(
        "note",
        "Vault-relative path of one note to repair, such as `projects/Plan.md`. Omit to work \
         through the whole vault.",
    );
    repair.title = Some("Repair broken links".to_string());

    let mut review = Prompt::new(
        "review_period",
        "Review what changed in the vault over a period and write up what moved forward, \
         what is stale and what needs attention.",
    )
    .with_required_arg("period", "`daily`, `weekly` or `monthly`.")
    .with_optional_arg(
        "folder",
        "Vault-relative folder to limit the review to, such as `projects`.",
    );
    review.title = Some("Review a period".to_string());

    let mut summarize = Prompt::new(
        "summarize_note",
        "Summarize a note in the context of the vault: what it says, what links to it and \
         what it links to.",
    )
    .with_required_arg(
        "path",
        "Vault-relative path of the note, such as `projects/Plan.md`.",
    );
    summarize.title = Some("Summarize a note".to_string());

    let mut ground = Prompt::new(
        "ground_draft",
        "Check a draft against the vault: find the notes that support or contradict each \
         claim, cite them with wikilinks, and flag claims the vault has nothing on.",
    )
    .with_required_arg("draft", "The draft text to check.")
    .with_optional_arg(
        "folder",
        "Vault-relative folder to draw sources from, such as `research`.",
    );
    ground.title = Some("Ground a draft".to_string());

    vec![triage, repair, review, summarize, ground]
}

/// Whether `name` is one of the vault's workflow prompts.
pub(crate) fn is_core_prompt(name: &str) -> bool {
    matches!(
        name,
        "triage_vault"
            | "repair_broken_links"
            | "review_period"
            | "summarize_note"
            | "ground_draft"
    )
}

/// Build the prompt `name` with `arguments`.
pub(crate) fn get(name: &str, arguments: Option<&Value>) -> McpResult<PromptResult> {
    let argument = |key: &str| -> Option<String> {
        arguments
            .and_then(|arguments| arguments.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let required = |key: &str| -> McpResult<String> {
        argument(key).ok_or_else(|| {
            McpError::invalid_params(format!("prompt {name} needs the argument `{key}`"))
        })
    };
    let one_of = |key: &str, value: String, allowed: &[&str]| -> McpResult<String> {
        if allowed.contains(&value.as_str()) {
            Ok(value)
        } else {
            Err(McpError::invalid_params(format!(
                "`{key}` must be one of {}, not {value:?}",
                allowed.join(", ")
            )))
        }
    };

    let (description, text) = match name {
        "triage_vault" => {
            let focus = one_of(
                "focus",
                argument("focus").unwrap_or_else(|| "all".to_string()),
                FOCUSES,
            )?;
            let steps = match focus.as_str() {
                "links" => {
                    "1. Call vault_health for the overall picture.\n\
                            2. Call graph_report with kind `broken_links` and group the results by the missing target.\n\
                            3. For each group, use search to find the note that was probably meant."
                }
                "orphans" => {
                    "1. Call vault_health for the overall picture.\n\
                              2. Call graph_report with kind `isolated_clusters`, then with kind `dead_ends`.\n\
                              3. For the most important orphans, call suggest_links to find where they belong."
                }
                "structure" => {
                    "1. Call explain_vault for the folder structure and hub notes.\n\
                                2. Call graph_report with kind `hubs`, then with kind `centrality`, to see what the vault is organized around.\n\
                                3. Look for folders that hold unrelated notes, and hubs that are missing."
                }
                _ => {
                    "1. Call get_vault_context, then vault_health.\n\
                      2. Call graph_report with kinds `broken_links`, `dead_ends` and `isolated_clusters`.\n\
                      3. Call explain_vault for the structure and the hub notes."
                }
            };
            (
                "Survey the vault and propose fixes",
                format!(
                    "Triage this Obsidian vault, focusing on {focus}.\n\n{steps}\n\n\
                     Then propose the five fixes that would help most, most valuable first, \
                     each with the notes it touches and the tool call that would make it \
                     (edit_note, move_note, write_note). Don't change anything until I agree."
                ),
            )
        }
        "repair_broken_links" => {
            let scope = match argument("note") {
                Some(note) => {
                    format!(
                        "Call graph_report with kind `broken_links` and keep only those whose source is `{note}`."
                    )
                }
                None => "Call graph_report with kind `broken_links` and work through every \
                         page of results (meta.pagination.next_offset)."
                    .to_string(),
            };
            (
                "Fix links to missing notes",
                format!(
                    "Repair broken links in this Obsidian vault.\n\n\
                     1. {scope}\n\
                     2. For each broken link, check its suggestions, then search for the note \
                     that was meant.\n\
                     3. If an existing note is clearly the target, read the linking note with \
                     read_note and fix the link with edit_note, passing the hash read_note \
                     returned. Keep the link's display text.\n\
                     4. If the target should exist but doesn't, list it for me instead of \
                     creating it.\n\n\
                     Report each link you fixed and each you left, with the reason."
                ),
            )
        }
        "review_period" => {
            let period = one_of("period", required("period")?, PERIODS)?;
            let days = match period.as_str() {
                "daily" => 1,
                "weekly" => 7,
                _ => 30,
            };
            let scope = argument("folder")
                .map(|folder| format!(" in `{folder}`"))
                .unwrap_or_default();
            (
                "Review recent changes",
                format!(
                    "Write a {period} review of this Obsidian vault{scope}.\n\n\
                     1. Call audit_log to see the notes changed in the last {days} day(s).\n\
                     2. Read the most significant ones with read_note.\n\
                     3. Call find_stale_notes for notes that were active and have gone quiet.\n\
                     4. Call graph_report with kind `broken_links` for anything the changes left broken.\n\n\
                     Summarize what moved forward, what is stale, and the three things that \
                     most need attention, linking each note with [[wikilinks]]."
                ),
            )
        }
        "summarize_note" => {
            let path = required("path")?;
            (
                "Summarize a note with its links",
                format!(
                    "Summarize the note `{path}` in the context of this Obsidian vault.\n\n\
                     1. Call read_note for `{path}`.\n\
                     2. Call get_links for it, once with direction `incoming` and once with `outgoing`.\n\
                     3. Call get_related_notes for notes connected to it more than one link away.\n\n\
                     Write a short summary of what the note says, then what links to it and \
                     why, and what it builds on. Link every note you mention with [[wikilinks]]."
                ),
            )
        }
        "ground_draft" => {
            let draft = required("draft")?;
            let scope = argument("folder")
                .map(|folder| format!(", using notes in `{folder}`"))
                .unwrap_or_default();
            (
                "Check a draft against the vault",
                format!(
                    "Ground this draft against my Obsidian vault{scope}.\n\n\
                     For each claim in it, call search (and find_similar with the claim as \
                     `query`) to find notes that support or contradict it, and read the \
                     best matches with read_note. Then return the draft with a [[wikilink]] \
                     citation after each supported claim, and a list of the claims the vault \
                     contradicts or has nothing on.\n\n\
                     Draft:\n\n{draft}"
                ),
            )
        }
        _ => return Err(McpError::prompt_not_found(name)),
    };
    Ok(PromptResult::user(text).with_description(description))
}

/// The most values one completion answer carries (the spec's limit).
const MAX_COMPLETION_VALUES: usize = 100;

/// `values` matching `prefix` case-insensitively, at most the spec's limit,
/// and how many matched in all.
fn complete_from(values: &[&str], prefix: &str) -> (Vec<String>, usize) {
    let prefix = prefix.to_lowercase();
    let matched: Vec<String> = values
        .iter()
        .filter(|value| value.to_lowercase().starts_with(&prefix))
        .map(|value| (*value).to_string())
        .collect();
    let total = matched.len();
    (
        matched.into_iter().take(MAX_COMPLETION_VALUES).collect(),
        total,
    )
}

/// A `completion/complete` result.
fn completion((values, total): (Vec<String>, usize)) -> Value {
    serde_json::json!({
        "completion": {
            "hasMore": total > values.len(),
            "total": total,
            "values": values,
        }
    })
}

impl super::CoreToolHandler {
    /// Complete an argument of one of the vault's own prompts or of the note
    /// resource template. `None` when the reference is someone else's (a
    /// plugin's), so the caller can route it on.
    pub(super) async fn complete_core(&self, params: &Value) -> Option<McpResult<Value>> {
        let reference = params.get("ref")?;
        let argument = params.get("argument");
        let name = argument
            .and_then(|argument| argument.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let value = argument
            .and_then(|argument| argument.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        match reference.get("type").and_then(Value::as_str)? {
            "ref/prompt" => {
                let prompt = reference.get("name").and_then(Value::as_str)?;
                if !is_core_prompt(prompt) {
                    return None;
                }
                Some(Ok(completion(
                    self.complete_prompt_argument(name, value).await,
                )))
            }
            "ref/resource" => {
                let uri = reference.get("uri").and_then(Value::as_str)?;
                if uri != super::note_resources::NOTE_URI_TEMPLATE {
                    return None;
                }
                let vault = params
                    .get("context")
                    .and_then(|context| context.get("arguments"))
                    .and_then(|arguments| arguments.get("vault"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                Some(Ok(completion(
                    self.complete_note_variable(name, value, vault).await,
                )))
            }
            _ => None,
        }
    }

    async fn complete_prompt_argument(&self, name: &str, value: &str) -> (Vec<String>, usize) {
        match name {
            "period" => complete_from(PERIODS, value),
            "focus" => complete_from(FOCUSES, value),
            "path" | "note" | "folder" => {
                let Ok(vault) = self.get_active_vault_name().await else {
                    return (Vec::new(), 0);
                };
                // Activating the vault publishes its notes to the hub.
                if self.get_active_vault_manager().await.is_err() {
                    return (Vec::new(), 0);
                }
                if name == "folder" {
                    self.resource_hub
                        .complete_folder(&vault, value, MAX_COMPLETION_VALUES)
                } else {
                    self.resource_hub
                        .complete_path(&vault, value, MAX_COMPLETION_VALUES)
                }
            }
            _ => (Vec::new(), 0),
        }
    }

    async fn complete_note_variable(
        &self,
        name: &str,
        value: &str,
        vault: Option<String>,
    ) -> (Vec<String>, usize) {
        match name {
            "vault" => {
                let names: Vec<String> = self
                    .multi_vault_mgr
                    .list_vaults()
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .map(|vault| vault.name)
                    .collect();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                complete_from(&names, value)
            }
            "path" => {
                let vault = match vault {
                    Some(vault) => vault,
                    None => match self.get_active_vault_name().await {
                        Ok(vault) => vault,
                        Err(_) => return (Vec::new(), 0),
                    },
                };
                if self.manager_for_vault(&vault).await.is_err() {
                    return (Vec::new(), 0);
                }
                self.resource_hub
                    .complete_path(&vault, value, MAX_COMPLETION_VALUES)
            }
            _ => (Vec::new(), 0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_prompt_builds_and_names_only_real_arguments() {
        for prompt in prompts() {
            assert!(is_core_prompt(&prompt.name), "{}", prompt.name);
            assert!(prompt.title.is_some(), "{}", prompt.name);
            let mut arguments = serde_json::Map::new();
            for argument in prompt.arguments.unwrap_or_default() {
                let value = match argument.name.as_str() {
                    "period" => "weekly",
                    "focus" => "links",
                    _ => "projects/Plan.md",
                };
                arguments.insert(argument.name, json!(value));
            }
            let result = get(&prompt.name, Some(&Value::Object(arguments))).unwrap();
            let text = result.messages[0]
                .content
                .as_text()
                .unwrap_or_default()
                .to_string();
            assert!(!text.is_empty(), "{}", prompt.name);
            assert!(!text.contains('\u{2014}'), "{}: no em-dashes", prompt.name);
        }
    }

    #[test]
    fn missing_and_invalid_arguments_are_invalid_params() {
        assert!(get("summarize_note", None).is_err());
        assert!(get("review_period", Some(&json!({"period": "hourly"}))).is_err());
        assert!(get("triage_vault", None).is_ok());
        assert!(get("nope", None).is_err());
    }
}
