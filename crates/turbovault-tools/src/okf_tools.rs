//! Open Knowledge Format (OKF) tools.
//!
//! Two capabilities that make TurboVault a first-class OKF *consumer* and
//! *maintainer*:
//!
//! - [`OkfTools::validate`] — checks a vault (or subtree) for OKF v0.1
//!   conformance (spec §9) and surfaces each concept's OKF metadata (`type`,
//!   `title`, `description`, `resource`, `timestamp`, citation count). Designed
//!   to double as a CI gate: the report is non-conformant if any concept lacks
//!   a parseable frontmatter `type`.
//! - [`OkfTools::generate_index`] — synthesizes/refreshes `index.md` files for
//!   progressive disclosure (spec §6), enumerating each directory's concepts
//!   and subdirectories using their frontmatter `description`.
//!
//! Both operate on the existing vault model — OKF is layered semantics over
//! markdown + frontmatter, not a separate store.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use turbovault_core::Result;
use turbovault_core::okf::{self, ReservedFile};
use turbovault_parser::parse_citations;
use turbovault_vault::VaultManager;

/// OKF metadata and conformance for a single document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkfConceptInfo {
    /// Vault-relative path.
    pub path: String,
    /// OKF concept ID (bundle-relative path minus `.md`).
    pub concept_id: String,
    /// Whether the document is OKF-conformant.
    pub conformant: bool,
    /// Reserved-file kind (`index`/`log`), if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserved: Option<ReservedFile>,
    /// OKF `type` — the only required field.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    /// Number of citations under a `# Citations` heading (spec §8).
    pub citation_count: usize,
    /// Conformance issues, if any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// Vault-wide OKF conformance report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkfValidateReport {
    /// Total markdown documents examined.
    pub total: usize,
    /// Documents that are OKF-conformant.
    pub conformant: usize,
    /// Documents that are not conformant (the CI gate fails when > 0).
    pub non_conformant: usize,
    /// Non-reserved concept documents.
    pub concepts: usize,
    /// Reserved files (`index.md` / `log.md`).
    pub reserved_files: usize,
    /// Count of concepts per `type` value (bundle's type vocabulary).
    pub type_distribution: BTreeMap<String, usize>,
    /// Vault-relative paths of non-conformant documents (quick CI summary).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub non_conformant_paths: Vec<String>,
    /// Per-document detail.
    pub files: Vec<OkfConceptInfo>,
}

/// One generated/previewed index file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneratedIndex {
    /// Vault-relative path of the `index.md`.
    pub path: String,
    /// Number of entries (concepts + subdirectories) listed.
    pub entries: usize,
    /// Whether the file was written (false in dry-run, or if unchanged).
    pub written: bool,
}

/// Result of an index-generation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenerateIndexReport {
    /// Indexes generated or previewed, one per directory.
    pub indexes: Vec<GeneratedIndex>,
    /// Total entries across all generated indexes.
    pub total_entries: usize,
    /// Whether this was a dry run (nothing written).
    pub dry_run: bool,
}

/// Result of appending an entry to a `log.md` (spec §7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntryResult {
    /// Vault-relative path of the `log.md`.
    pub path: String,
    /// The date section the entry was filed under (`YYYY-MM-DD`).
    pub date: String,
    /// Whether the `log.md` file was newly created.
    pub created_file: bool,
    /// Whether a new date section was created for this entry.
    pub created_section: bool,
}

/// OKF tooling over a vault.
pub struct OkfTools {
    manager: Arc<VaultManager>,
}

impl OkfTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    fn rel(&self, path: &Path) -> String {
        self.manager.relative_path(path)
    }

    /// Validate the vault (or a subtree) for OKF v0.1 conformance.
    ///
    /// `subtree`, when given, is a vault-relative directory; only documents
    /// under it are examined.
    pub async fn validate(&self, subtree: Option<&str>) -> Result<OkfValidateReport> {
        // Cache-first: parsed notes validated against disk mtime, no re-scan.
        let files = self.manager.vault_files_validated().await;
        let root = self.manager.vault_path();
        let filter_prefix = subtree.map(|s| root.join(s));

        let mut infos: Vec<OkfConceptInfo> = Vec::new();
        let mut type_distribution: BTreeMap<String, usize> = BTreeMap::new();

        for vault_file in &files {
            let path = &vault_file.path;
            if let Some(prefix) = &filter_prefix
                && !path.starts_with(prefix)
            {
                continue;
            }

            let fm = vault_file.frontmatter.as_ref();
            let conformance = okf::check_concept(fm, path);

            let type_ = fm.and_then(|f| f.okf_type());
            if let (Some(t), None) = (&type_, conformance.reserved) {
                *type_distribution.entry(t.clone()).or_insert(0) += 1;
            }

            infos.push(OkfConceptInfo {
                path: self.rel(path),
                concept_id: okf::concept_id(root, path),
                conformant: conformance.conformant,
                reserved: conformance.reserved,
                type_,
                title: fm.and_then(|f| f.okf_title()),
                description: fm.and_then(|f| f.okf_description()),
                resource: fm.and_then(|f| f.okf_resource()),
                timestamp: fm.and_then(|f| f.okf_timestamp()),
                citation_count: parse_citations(&vault_file.content).len(),
                issues: conformance.issues,
            });
        }

        // Stable, path-sorted output (cache iteration order is unspecified).
        infos.sort_by(|a, b| a.path.cmp(&b.path));

        let total = infos.len();
        let conformant = infos.iter().filter(|i| i.conformant).count();
        let reserved_files = infos.iter().filter(|i| i.reserved.is_some()).count();
        let non_conformant_paths: Vec<String> = infos
            .iter()
            .filter(|i| !i.conformant)
            .map(|i| i.path.clone())
            .collect();

        Ok(OkfValidateReport {
            total,
            conformant,
            non_conformant: total - conformant,
            concepts: total - reserved_files,
            reserved_files,
            type_distribution,
            non_conformant_paths,
            files: infos,
        })
    }

    /// Generate or refresh `index.md` files for progressive disclosure.
    ///
    /// - `directory`: vault-relative directory to index. `None` = the bundle
    ///   root.
    /// - `recursive`: also index every subdirectory.
    /// - `dry_run`: compute the indexes but do not write them.
    pub async fn generate_index(
        &self,
        directory: Option<&str>,
        recursive: bool,
        dry_run: bool,
    ) -> Result<GenerateIndexReport> {
        // Cache-first: parsed notes (validated against disk mtime) instead of a
        // fresh scan + re-parse of every file.
        let validated = self.manager.vault_files_validated().await;
        let root = self.manager.vault_path().clone();
        let base = match directory {
            Some(d) => root.join(d),
            None => root.clone(),
        };

        // Prefetch (title, description) per concept so index rendering needs no
        // further I/O.
        let meta: HashMap<PathBuf, ConceptMeta> = validated
            .iter()
            .map(|vf| {
                let fm = vf.frontmatter.as_ref();
                (
                    vf.path.clone(),
                    ConceptMeta {
                        title: fm.and_then(|f| f.okf_title()),
                        description: fm.and_then(|f| f.okf_description()),
                    },
                )
            })
            .collect();

        // Map each directory to its direct concept files (non-reserved .md) and
        // the set of its direct subdirectories.
        let mut dir_concepts: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
        let mut dir_subdirs: BTreeMap<PathBuf, BTreeSet<PathBuf>> = BTreeMap::new();

        for vf in &validated {
            let path = &vf.path;
            let Some(parent) = path.parent() else {
                continue;
            };
            if okf::reserved_file(path).is_none() {
                dir_concepts
                    .entry(parent.to_path_buf())
                    .or_default()
                    .push(path.clone());
            }
            // Register every ancestor directory up to root as a subdir of its parent.
            let mut cur = parent.to_path_buf();
            while cur.starts_with(&root) && cur != root {
                let Some(grandparent) = cur.parent() else {
                    break;
                };
                dir_subdirs
                    .entry(grandparent.to_path_buf())
                    .or_default()
                    .insert(cur.clone());
                if grandparent == root {
                    break;
                }
                cur = grandparent.to_path_buf();
            }
        }

        // Which directories to index.
        let mut target_dirs: BTreeSet<PathBuf> = BTreeSet::new();
        let all_dirs: BTreeSet<PathBuf> = dir_concepts
            .keys()
            .chain(dir_subdirs.keys())
            .chain(dir_subdirs.values().flatten())
            .cloned()
            .collect();
        for dir in &all_dirs {
            let include = if recursive {
                dir.starts_with(&base)
            } else {
                *dir == base
            };
            if include {
                target_dirs.insert(dir.clone());
            }
        }
        // Ensure the base directory is considered even if it has no entries yet.
        if recursive || target_dirs.is_empty() {
            target_dirs.insert(base.clone());
        }

        let mut indexes = Vec::new();
        let mut total_entries = 0usize;

        for dir in &target_dirs {
            let concepts = dir_concepts.get(dir).cloned().unwrap_or_default();
            let subdirs = dir_subdirs.get(dir).cloned().unwrap_or_default();
            if concepts.is_empty() && subdirs.is_empty() {
                continue;
            }

            let content = Self::render_index(dir, &concepts, &subdirs, &meta);
            let entries = concepts.len() + subdirs.len();
            total_entries += entries;

            let index_abs = dir.join("index.md");
            let index_rel = self.rel(&index_abs);

            let mut written = false;
            if !dry_run {
                let existing = self.manager.read_file(&index_abs).await.ok();
                if existing.as_deref() != Some(content.as_str()) {
                    self.manager.write_file(&index_abs, &content, None).await?;
                    written = true;
                }
            }

            indexes.push(GeneratedIndex {
                path: index_rel,
                entries,
                written,
            });
        }

        indexes.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(GenerateIndexReport {
            indexes,
            total_entries,
            dry_run,
        })
    }

    /// Render an `index.md` body for a directory (spec §6 — no frontmatter),
    /// using prefetched concept metadata so no per-file I/O is needed.
    fn render_index(
        dir: &Path,
        concepts: &[PathBuf],
        subdirs: &BTreeSet<PathBuf>,
        meta: &HashMap<PathBuf, ConceptMeta>,
    ) -> String {
        // Heading is the directory's own name (the bundle's folder name at root).
        let heading = dir.file_name().and_then(|n| n.to_str()).unwrap_or("Index");

        let mut out = format!("# {}\n", heading);

        // Concept entries, sorted by display title.
        let mut concept_entries: Vec<(String, String, Option<String>)> = Vec::new();
        for path in concepts {
            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_string();
            let stem_title = || {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&file_name)
                    .to_string()
            };
            let (title, description) = match meta.get(path) {
                Some(m) => (
                    m.title.clone().unwrap_or_else(stem_title),
                    m.description.clone(),
                ),
                None => (stem_title(), None),
            };
            concept_entries.push((title, file_name, description));
        }
        concept_entries.sort_by_key(|e| e.0.to_lowercase());

        if !concept_entries.is_empty() {
            out.push_str("\n## Notes\n\n");
            for (title, link, description) in &concept_entries {
                let title = escape_link_text(title);
                match description {
                    Some(d) => {
                        out.push_str(&format!("* [{}]({}) - {}\n", title, link, one_line(d)))
                    }
                    None => out.push_str(&format!("* [{}]({})\n", title, link)),
                }
            }
        }

        // Subdirectory entries.
        if !subdirs.is_empty() {
            let mut sub_entries: Vec<(String, String)> = subdirs
                .iter()
                .filter_map(|s| {
                    s.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| (n.to_string(), format!("{}/", n)))
                })
                .collect();
            sub_entries.sort_by_key(|e| e.0.to_lowercase());

            out.push_str("\n## Subdirectories\n\n");
            for (name, link) in &sub_entries {
                out.push_str(&format!("* [{}]({})\n", name, link));
            }
        }

        out
    }

    /// Append an entry to a directory's `log.md` (spec §7).
    ///
    /// - `directory`: vault-relative directory whose `log.md` to update. `None`
    ///   = the bundle root.
    /// - `kind`: the leading bold word (`Update`, `Creation`, `Deprecation`, …).
    ///   Defaults to `Update`.
    /// - `text`: the entry prose.
    /// - `date`: ISO `YYYY-MM-DD`. Defaults to today (local time).
    ///
    /// Entries are filed newest-first: a new date becomes the first `##`
    /// section; an existing date gains another bullet.
    pub async fn append_log_entry(
        &self,
        directory: Option<&str>,
        kind: Option<&str>,
        text: &str,
        date: Option<&str>,
    ) -> Result<LogEntryResult> {
        let date = match date {
            Some(d) => {
                chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| {
                    turbovault_core::Error::parse_error(format!(
                        "invalid date '{d}' — expected ISO YYYY-MM-DD"
                    ))
                })?;
                d.to_string()
            }
            None => chrono::Local::now().format("%Y-%m-%d").to_string(),
        };
        let kind = kind.unwrap_or("Update");

        let log_rel = match directory {
            Some(d) if !d.is_empty() && d != "." => format!("{}/log.md", d.trim_end_matches('/')),
            _ => "log.md".to_string(),
        };
        let log_path = std::path::PathBuf::from(&log_rel);

        // Read the existing log, distinguishing "absent" (create fresh) from
        // "present but unreadable" (propagate — never clobber a log we failed to
        // read). resolve_path enforces the vault boundary.
        let resolved = self.manager.resolve_path(&log_path)?;
        let existing = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(turbovault_core::Error::io(e)),
        };
        let (content, created_file, created_section) =
            build_log_content(&existing, &date, kind, text);
        self.manager.write_file(&log_path, &content, None).await?;

        Ok(LogEntryResult {
            path: log_rel,
            date,
            created_file,
            created_section,
        })
    }
}

/// Title + description prefetched for a concept, used to render index entries.
struct ConceptMeta {
    title: Option<String>,
    description: Option<String>,
}

/// Escape a string for use as markdown link text, so a `]`/`[` in a title
/// can't break the generated `[title](link)` entry.
fn escape_link_text(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

/// Collapse a (possibly multi-line) description to a single line, so it can't
/// break the `* [title](link) - description` bullet.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Pure builder for `log.md` content: insert `entry` under a `## {date}`
/// section, newest-first. Returns `(content, created_file, created_section)`.
fn build_log_content(existing: &str, date: &str, kind: &str, text: &str) -> (String, bool, bool) {
    let entry = format!("* **{}**: {}", kind, text);

    if existing.trim().is_empty() {
        let content = format!("# Update Log\n\n## {}\n\n{}\n", date, entry);
        return (content, true, true);
    }

    let date_heading = format!("## {}", date);
    let mut out: Vec<String> = existing.lines().map(|s| s.to_string()).collect();
    let trailing_newline = existing.ends_with('\n');

    if let Some(idx) = out.iter().position(|l| l.trim() == date_heading) {
        // Existing date section: append the bullet at its end (before the next
        // heading), skipping trailing blank lines.
        let mut end = out.len();
        for (j, line) in out.iter().enumerate().skip(idx + 1) {
            if line.trim_start().starts_with("# ") || line.trim_start().starts_with("## ") {
                end = j;
                break;
            }
        }
        let mut insert_at = end;
        while insert_at > idx + 1 && out[insert_at - 1].trim().is_empty() {
            insert_at -= 1;
        }
        out.insert(insert_at, entry);
        return (join_lines(&out, trailing_newline), false, false);
    }

    // No section for this date: insert it as the first `##` section, right after
    // the document title (and its blank line), so newest sits on top.
    let title_idx = out
        .iter()
        .position(|l| l.trim_start().starts_with("# ") && !l.trim_start().starts_with("## "));
    let insert_pos = match title_idx {
        Some(t) => {
            let mut p = t + 1;
            if out.get(p).map(|l| l.trim().is_empty()).unwrap_or(false) {
                p += 1;
            }
            p
        }
        None => 0,
    };
    for (k, line) in [date_heading, String::new(), entry, String::new()]
        .into_iter()
        .enumerate()
    {
        out.insert(insert_pos + k, line);
    }
    (join_lines(&out, trailing_newline), false, true)
}

fn join_lines(lines: &[String], trailing_newline: bool) -> String {
    let mut s = lines.join("\n");
    if trailing_newline {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_manager(vault_dir: &Path) -> Arc<VaultManager> {
        use turbovault_core::{ServerConfig, VaultConfig};
        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder("test", vault_dir).build().unwrap());
        Arc::new(VaultManager::new(config).unwrap())
    }

    #[tokio::test]
    async fn validate_flags_missing_type() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("tables")).unwrap();
        std::fs::write(
            temp.path().join("tables/orders.md"),
            "---\ntype: BigQuery Table\ntitle: Orders\ndescription: One row per order.\n---\n# Schema\n\n# Citations\n\n[1] [src](https://x.example)\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("loose.md"),
            "---\ntitle: No type here\n---\n# Body\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("index.md"), "# Index\n").unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = OkfTools::new(manager);

        let report = tools.validate(None).await.unwrap();
        assert_eq!(report.total, 3);
        assert_eq!(report.non_conformant, 1);
        assert_eq!(report.non_conformant_paths, vec!["loose.md".to_string()]);
        assert_eq!(report.reserved_files, 1); // index.md
        assert_eq!(
            report.type_distribution.get("BigQuery Table").copied(),
            Some(1)
        );

        let orders = report
            .files
            .iter()
            .find(|f| f.path == "tables/orders.md")
            .unwrap();
        assert!(orders.conformant);
        assert_eq!(orders.concept_id, "tables/orders");
        assert_eq!(orders.type_.as_deref(), Some("BigQuery Table"));
        assert_eq!(orders.citation_count, 1);
    }

    #[tokio::test]
    async fn validate_subtree_filter() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("tables")).unwrap();
        std::fs::write(
            temp.path().join("tables/orders.md"),
            "---\ntype: Table\n---\n# x\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("root.md"), "---\ntype: Note\n---\n# y\n").unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = OkfTools::new(manager);

        let report = tools.validate(Some("tables")).await.unwrap();
        assert_eq!(report.total, 1);
        assert_eq!(report.files[0].path, "tables/orders.md");
    }

    #[tokio::test]
    async fn generate_index_dry_run_lists_entries() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("tables")).unwrap();
        std::fs::write(
            temp.path().join("tables/orders.md"),
            "---\ntype: Table\ntitle: Orders\ndescription: One per order.\n---\n# x\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("tables/customers.md"),
            "---\ntype: Table\ntitle: Customers\n---\n# y\n",
        )
        .unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = OkfTools::new(manager);

        // Root index (non-recursive): one subdirectory entry, no concepts.
        let report = tools.generate_index(None, false, true).await.unwrap();
        assert!(report.dry_run);
        let root_index = report
            .indexes
            .iter()
            .find(|i| i.path == "index.md")
            .unwrap();
        assert_eq!(root_index.entries, 1); // the `tables/` subdirectory
        assert!(!root_index.written);

        // Nothing should have been written in dry-run.
        assert!(!temp.path().join("index.md").exists());
    }

    #[tokio::test]
    async fn generate_index_recursive_writes_files() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("tables")).unwrap();
        std::fs::write(
            temp.path().join("tables/orders.md"),
            "---\ntype: Table\ntitle: Orders\ndescription: One per order.\n---\n# x\n",
        )
        .unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = OkfTools::new(manager);

        let report = tools.generate_index(None, true, false).await.unwrap();
        assert!(!report.dry_run);

        // tables/index.md should now list Orders with its description.
        let tables_index = std::fs::read_to_string(temp.path().join("tables/index.md")).unwrap();
        assert!(tables_index.contains("# tables"));
        assert!(tables_index.contains("* [Orders](orders.md) - One per order."));

        // Re-running should be idempotent (no rewrite when content is unchanged).
        let rerun = tools.generate_index(None, true, false).await.unwrap();
        let tables = rerun
            .indexes
            .iter()
            .find(|i| i.path == "tables/index.md")
            .unwrap();
        assert!(!tables.written);
    }

    #[test]
    fn index_entry_escapes_title_and_flattens_description() {
        assert_eq!(
            escape_link_text("Orders [archived]"),
            "Orders \\[archived\\]"
        );
        assert_eq!(
            one_line("line one\n  line two\t three"),
            "line one line two three"
        );
    }

    #[test]
    fn build_log_creates_file_when_empty() {
        let (content, created_file, created_section) =
            build_log_content("", "2026-06-13", "Creation", "Established the bundle.");
        assert!(created_file);
        assert!(created_section);
        assert!(content.starts_with("# Update Log\n"));
        assert!(content.contains("## 2026-06-13"));
        assert!(content.contains("* **Creation**: Established the bundle."));
    }

    #[test]
    fn build_log_appends_to_existing_date_section() {
        let existing = "# Update Log\n\n## 2026-06-13\n\n* **Update**: First.\n";
        let (content, created_file, created_section) =
            build_log_content(existing, "2026-06-13", "Update", "Second.");
        assert!(!created_file);
        assert!(!created_section);
        // Both bullets under the same date, in order.
        let first = content.find("First.").unwrap();
        let second = content.find("Second.").unwrap();
        assert!(first < second);
        assert_eq!(content.matches("## 2026-06-13").count(), 1);
    }

    #[test]
    fn build_log_inserts_new_date_newest_first() {
        let existing = "# Update Log\n\n## 2026-06-10\n\n* **Update**: Old.\n";
        let (content, _, created_section) =
            build_log_content(existing, "2026-06-13", "Update", "New.");
        assert!(created_section);
        // The new date section comes before the old one (newest-first).
        let new_pos = content.find("## 2026-06-13").unwrap();
        let old_pos = content.find("## 2026-06-10").unwrap();
        assert!(new_pos < old_pos);
    }

    #[tokio::test]
    async fn append_log_entry_writes_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = OkfTools::new(manager);

        let result = tools
            .append_log_entry(None, Some("Creation"), "Bootstrapped.", Some("2026-06-13"))
            .await
            .unwrap();
        assert_eq!(result.path, "log.md");
        assert!(result.created_file);

        let written = std::fs::read_to_string(temp.path().join("log.md")).unwrap();
        assert!(written.contains("## 2026-06-13"));
        assert!(written.contains("* **Creation**: Bootstrapped."));

        // A second entry on the same day appends under the same section.
        tools
            .append_log_entry(None, None, "Refined.", Some("2026-06-13"))
            .await
            .unwrap();
        let written = std::fs::read_to_string(temp.path().join("log.md")).unwrap();
        assert_eq!(written.matches("## 2026-06-13").count(), 1);
        assert!(written.contains("* **Update**: Refined."));
    }

    #[tokio::test]
    async fn append_log_entry_rejects_bad_date() {
        let temp = tempfile::TempDir::new().unwrap();
        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = OkfTools::new(manager);

        let err = tools
            .append_log_entry(None, None, "x", Some("June 13"))
            .await;
        assert!(err.is_err());
    }
}
