//! Open Knowledge Format (OKF) support.
//!
//! [OKF](https://github.com/GoogleCloudPlatform/knowledge-catalog) is an open,
//! vendor-neutral format for representing knowledge as a directory of markdown
//! files with YAML frontmatter. It is deliberately close to the substrate
//! TurboVault already operates on (markdown + frontmatter + cross-links + index
//! files), so this module layers OKF *semantics* on top of the existing
//! [`Frontmatter`] / [`VaultFile`] model rather than introducing a parallel one.
//!
//! What this module provides:
//! - Frontmatter accessors for the OKF-recommended fields (`type`, `title`,
//!   `description`, `resource`, `timestamp`).
//! - [`concept_id`] — the OKF identity of a document (its bundle-relative path
//!   minus the `.md` suffix).
//! - Reserved-filename detection (`index.md`, `log.md`).
//! - [`normalize_link_target`] — turns an OKF cross-link target
//!   (`/tables/orders.md`, `./customers.md`) into the form the link graph
//!   resolves against.
//! - [`Citation`] — the `# Citations` convention type (parsing lives in the
//!   parser crate; the shared type lives here).
//! - [`check_concept`] — per-document conformance per OKF v0.1 §9.
//!
//! See the spec for details. OKF is intentionally permissive: unknown `type`
//! values, extra frontmatter keys, and broken cross-links are all valid.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::models::Frontmatter;

/// OKF frontmatter accessors layered over the generic [`Frontmatter`] map.
///
/// These read the small set of OKF-recommended keys. Only `type` is required
/// by the spec; the rest are optional.
impl Frontmatter {
    /// Read a string-valued frontmatter field, trimming surrounding whitespace.
    ///
    /// Returns `None` for missing keys, non-string values, or empty strings.
    fn okf_str_field(&self, key: &str) -> Option<String> {
        let s = self.data.get(key)?.as_str()?.trim();
        if s.is_empty() {
            None
        } else {
            Some(s.to_string())
        }
    }

    /// The OKF `type` — the only required field. Identifies the kind of concept
    /// (e.g. `BigQuery Table`, `Playbook`, `Reference`). Not registered
    /// centrally; consumers must tolerate unknown values.
    pub fn okf_type(&self) -> Option<String> {
        self.okf_str_field("type")
    }

    /// The OKF `title` — human-readable display name.
    pub fn okf_title(&self) -> Option<String> {
        self.okf_str_field("title")
    }

    /// The OKF `description` — one-line summary, used for index entries and
    /// search snippets.
    pub fn okf_description(&self) -> Option<String> {
        self.okf_str_field("description")
    }

    /// The OKF `resource` — canonical URI for the underlying asset, if any.
    pub fn okf_resource(&self) -> Option<String> {
        self.okf_str_field("resource")
    }

    /// The OKF `timestamp` — ISO 8601 datetime of last meaningful change (raw
    /// string as authored).
    pub fn okf_timestamp(&self) -> Option<String> {
        self.okf_str_field("timestamp")
    }

    /// True if this frontmatter carries a non-empty OKF `type`, the minimum bar
    /// for an OKF-conformant concept document (§9).
    pub fn is_okf_concept(&self) -> bool {
        self.okf_type().is_some()
    }
}

/// A reserved OKF filename with defined meaning at any level of the hierarchy.
///
/// Reserved files MUST NOT be used for concept documents (spec §3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReservedFile {
    /// `index.md` — directory listing for progressive disclosure (§6).
    Index,
    /// `log.md` — chronological update history (§7).
    Log,
}

impl ReservedFile {
    /// The on-disk filename for this reserved file.
    pub fn filename(self) -> &'static str {
        match self {
            ReservedFile::Index => "index.md",
            ReservedFile::Log => "log.md",
        }
    }
}

/// Classify a path's filename as an OKF reserved file, if it is one.
///
/// Matching is case-insensitive, mirroring TurboVault's link resolution.
pub fn reserved_file(path: &Path) -> Option<ReservedFile> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "index.md" => Some(ReservedFile::Index),
        "log.md" => Some(ReservedFile::Log),
        _ => None,
    }
}

/// Compute the OKF **concept ID** for a document: its path within the bundle
/// with the `.md` suffix removed and `/` separators.
///
/// `bundle_root` is the bundle (vault) root; `path` may be absolute (under the
/// root) or already bundle-relative. For example, with a root of `/vault`,
/// `/vault/tables/users.md` has concept ID `tables/users`.
///
/// Falls back to the file stem when `path` is not under `bundle_root`.
pub fn concept_id(bundle_root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(bundle_root).unwrap_or(path);
    let s = rel.to_string_lossy().replace('\\', "/");
    let s = s.trim_start_matches('/');
    s.strip_suffix(".md").unwrap_or(s).to_string()
}

/// Normalize an OKF cross-link / citation target into the path-shaped form the
/// link graph resolves against.
///
/// Handles the two OKF link forms (spec §5):
/// - **Bundle-relative** (`/tables/orders.md`) — leading `/` stripped.
/// - **Relative** (`./other.md`, `../x.md`) — `.`/`..` segments dropped.
///
/// Any `#heading` / `#^block` fragment is removed, the `.md` suffix is stripped,
/// and the result is lowercased into `/`-joined path components for
/// suffix-matching. Returns `None` for external URLs, pure anchors, or empty
/// targets (nothing a vault file could resolve to).
///
/// # Examples
/// ```
/// use turbovault_core::okf::normalize_link_target;
///
/// assert_eq!(normalize_link_target("/tables/orders.md"), Some(vec!["tables".into(), "orders".into()]));
/// assert_eq!(normalize_link_target("./customers.md#schema"), Some(vec!["customers".into()]));
/// assert_eq!(normalize_link_target("https://example.com"), None);
/// assert_eq!(normalize_link_target("#section"), None);
/// ```
pub fn normalize_link_target(target: &str) -> Option<Vec<String>> {
    // External links and pure anchors never resolve to a vault file.
    if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("mailto:")
        || target.starts_with('#')
    {
        return None;
    }

    // Drop any heading/block fragment.
    let path_part = target.split('#').next().unwrap_or("").trim();
    if path_part.is_empty() {
        return None;
    }

    let parts: Vec<String> = path_part
        .split(['/', '\\'])
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .map(|seg| {
            let lower = seg.to_lowercase();
            lower.strip_suffix(".md").unwrap_or(&lower).to_string()
        })
        .collect();

    if parts.is_empty() { None } else { Some(parts) }
}

/// A citation backing a claim in a concept body (spec §8).
///
/// Citations are numbered markdown links listed under a `# Citations` heading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Citation {
    /// The `[N]` ordinal, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
    /// The link text (the cited source's display name).
    pub text: String,
    /// The citation target — an external URL or a bundle/relative path.
    pub url: String,
}

/// Per-document OKF conformance result (spec §9).
///
/// A document is conformant when it has parseable frontmatter carrying a
/// non-empty `type`. Reserved files (`index.md`/`log.md`) are exempt from the
/// `type` requirement — they are structural, not concepts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConceptConformance {
    /// Whether the document is conformant.
    pub conformant: bool,
    /// Whether a frontmatter block was present and parseable.
    pub has_frontmatter: bool,
    /// Whether a non-empty `type` field was present.
    pub has_type: bool,
    /// The reserved-file kind, if this path is a reserved file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reserved: Option<ReservedFile>,
    /// Human-readable issues explaining any non-conformance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// Check a single document for OKF v0.1 conformance (§9).
///
/// `frontmatter` is the already-parsed frontmatter (or `None` if absent /
/// unparseable). `path` determines reserved-file exemption.
pub fn check_concept(frontmatter: Option<&Frontmatter>, path: &Path) -> ConceptConformance {
    let reserved = reserved_file(path);
    let has_frontmatter = frontmatter.is_some();
    let has_type = frontmatter.is_some_and(Frontmatter::is_okf_concept);
    let mut issues = Vec::new();

    // Reserved files are structural; they are conformant without a `type`.
    if reserved.is_some() {
        return ConceptConformance {
            conformant: true,
            has_frontmatter,
            has_type,
            reserved,
            issues,
        };
    }

    if !has_frontmatter {
        issues.push("missing parseable YAML frontmatter block".to_string());
    } else if !has_type {
        issues.push("frontmatter is missing a non-empty `type` field".to_string());
    }

    ConceptConformance {
        conformant: issues.is_empty(),
        has_frontmatter,
        has_type,
        reserved,
        issues,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SourcePosition;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn fm(pairs: &[(&str, serde_json::Value)]) -> Frontmatter {
        let mut data = HashMap::new();
        for (k, v) in pairs {
            data.insert((*k).to_string(), v.clone());
        }
        Frontmatter {
            data,
            position: SourcePosition::start(),
        }
    }

    #[test]
    fn accessors_read_recommended_fields() {
        let f = fm(&[
            ("type", serde_json::json!("BigQuery Table")),
            ("title", serde_json::json!("Customer Orders")),
            ("description", serde_json::json!("One row per order.")),
            (
                "resource",
                serde_json::json!("https://console.cloud.google.com/x"),
            ),
            ("timestamp", serde_json::json!("2026-05-28T14:30:00Z")),
        ]);
        assert_eq!(f.okf_type().as_deref(), Some("BigQuery Table"));
        assert_eq!(f.okf_title().as_deref(), Some("Customer Orders"));
        assert_eq!(f.okf_description().as_deref(), Some("One row per order."));
        assert_eq!(
            f.okf_resource().as_deref(),
            Some("https://console.cloud.google.com/x")
        );
        assert_eq!(f.okf_timestamp().as_deref(), Some("2026-05-28T14:30:00Z"));
        assert!(f.is_okf_concept());
    }

    #[test]
    fn empty_and_missing_fields_are_none() {
        let f = fm(&[("type", serde_json::json!("   "))]);
        assert_eq!(f.okf_type(), None);
        assert!(!f.is_okf_concept());
        let g = fm(&[]);
        assert_eq!(g.okf_title(), None);
    }

    #[test]
    fn reserved_files_detected_case_insensitively() {
        assert_eq!(
            reserved_file(Path::new("/v/index.md")),
            Some(ReservedFile::Index)
        );
        assert_eq!(
            reserved_file(Path::new("/v/sub/LOG.md")),
            Some(ReservedFile::Log)
        );
        assert_eq!(reserved_file(Path::new("/v/orders.md")), None);
    }

    #[test]
    fn concept_id_strips_root_and_suffix() {
        let root = PathBuf::from("/vault");
        assert_eq!(
            concept_id(&root, &PathBuf::from("/vault/tables/users.md")),
            "tables/users"
        );
        assert_eq!(
            concept_id(&root, &PathBuf::from("tables/users.md")),
            "tables/users"
        );
    }

    #[test]
    fn normalize_targets() {
        assert_eq!(
            normalize_link_target("/tables/orders.md"),
            Some(vec!["tables".to_string(), "orders".to_string()])
        );
        assert_eq!(
            normalize_link_target("./customers.md#schema"),
            Some(vec!["customers".to_string()])
        );
        assert_eq!(
            normalize_link_target("../shared/glossary.md"),
            Some(vec!["shared".to_string(), "glossary".to_string()])
        );
        assert_eq!(normalize_link_target("https://example.com"), None);
        assert_eq!(normalize_link_target("#anchor"), None);
        assert_eq!(normalize_link_target(""), None);
        // Backslash separators normalize the same as `/` (consistent with concept_id).
        assert_eq!(
            normalize_link_target("\\tables\\orders.md"),
            Some(vec!["tables".to_string(), "orders".to_string()])
        );
    }

    #[test]
    fn conformance_requires_type_for_concepts() {
        let ok = check_concept(
            Some(&fm(&[("type", serde_json::json!("Playbook"))])),
            Path::new("/v/playbooks/x.md"),
        );
        assert!(ok.conformant);

        let no_type = check_concept(Some(&fm(&[])), Path::new("/v/x.md"));
        assert!(!no_type.conformant);
        assert!(no_type.has_frontmatter);
        assert!(!no_type.has_type);
        assert_eq!(no_type.issues.len(), 1);

        let no_fm = check_concept(None, Path::new("/v/x.md"));
        assert!(!no_fm.conformant);
        assert!(!no_fm.has_frontmatter);
    }

    #[test]
    fn reserved_files_are_conformant_without_type() {
        let idx = check_concept(None, Path::new("/v/index.md"));
        assert!(idx.conformant);
        assert_eq!(idx.reserved, Some(ReservedFile::Index));
        assert!(idx.issues.is_empty());
    }
}
