//! Grounding primitives — data an external LLM judge consumes.
//!
//! The competitor's evaluation harness scores enrichment with LLM-judge metrics
//! (hallucination-free, redundancy, contradiction, disambiguation). TurboVault
//! deliberately does **not** run a judge in-process; instead it surfaces the raw
//! material a judge needs to score those dimensions, computed deterministically:
//!
//! - **claims** — candidate factual statements extracted from the prose body.
//! - **citations** — the sources declared under `# Citations` (spec §8).
//! - **structural signals** — presence of `# Schema` / `# Examples` sections.
//! - **coverage flags** — e.g. a note that makes claims but cites nothing is a
//!   hallucination-risk candidate worth a judge's attention.
//!
//! These are intentionally heuristic (sentence-level extraction, not semantic
//! parsing). They are inputs to grounding evaluation, not a grounding verdict.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use turbovault_core::okf::Citation;
use turbovault_core::prelude::*;
use turbovault_parser::{ContentBlock, parse_blocks, parse_citations};
use turbovault_vault::VaultManager;

/// Maximum claims returned per note (keeps responses bounded).
const MAX_CLAIMS: usize = 200;
/// Minimum word count for a sentence to count as a candidate claim.
const MIN_CLAIM_WORDS: usize = 5;

/// Per-note grounding analysis — the material a judge scores.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroundingAnalysis {
    /// Vault-relative path.
    pub path: String,
    /// Number of candidate factual claims extracted from the body.
    pub claim_count: usize,
    /// Number of citations declared under `# Citations`.
    pub citation_count: usize,
    /// Whether the note declares any citations.
    pub has_citations: bool,
    /// Makes claims but cites nothing — a hallucination-risk candidate.
    pub uncited: bool,
    /// Whether a `# Schema` section is present (structural enrichment signal).
    pub has_schema_section: bool,
    /// Whether an `# Examples` section is present.
    pub has_examples_section: bool,
    /// Whether claims were truncated to [`MAX_CLAIMS`].
    pub claims_truncated: bool,
    /// Extracted candidate claims (declarative prose sentences).
    pub claims: Vec<String>,
    /// Declared citations.
    pub citations: Vec<Citation>,
    /// How to turn this data into a grounding verdict with an external judge.
    pub guidance: Vec<String>,
}

/// A note that asserts claims without citing any source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UngroundedNote {
    pub path: String,
    pub claim_count: usize,
}

/// Vault-wide ungrounded-note report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UngroundedReport {
    /// Notes scanned.
    pub total_notes: usize,
    /// Notes that make claims but declare no citations.
    pub ungrounded_count: usize,
    /// The ungrounded notes, most claims first (capped by the requested limit).
    pub notes: Vec<UngroundedNote>,
}

/// Grounding analysis over a vault.
pub struct GroundingTools {
    manager: Arc<VaultManager>,
}

impl GroundingTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    fn rel(&self, path: &std::path::Path) -> String {
        path.strip_prefix(self.manager.vault_path())
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Analyze a single note's grounding.
    pub async fn analyze_note(&self, path: &str) -> Result<GroundingAnalysis> {
        let file_path = std::path::PathBuf::from(path);
        let vault_file = self.manager.parse_file(&file_path).await?;
        let body = &vault_file.content;

        let all_claims = extract_claims(body);
        let claim_count = all_claims.len();
        let claims_truncated = claim_count > MAX_CLAIMS;
        let claims: Vec<String> = all_claims.into_iter().take(MAX_CLAIMS).collect();

        let citations = parse_citations(body);
        let citation_count = citations.len();
        let has_citations = citation_count > 0;

        // Parse headings once for both section checks.
        let headings = turbovault_parser::parse_headings(body);
        let has_section = |name: &str| {
            headings
                .iter()
                .any(|h| h.text.trim().eq_ignore_ascii_case(name))
        };
        let has_schema_section = has_section("schema");
        let has_examples_section = has_section("examples");
        let uncited = claim_count > 0 && citation_count == 0;

        let mut guidance = vec![
            "Feed `claims` + `citations` (and the cited sources) to an LLM judge to score hallucination-free grounding: the fraction of claims supported by a cited source.".to_string(),
            "Compare `claims` across related notes to score contradiction (conflicting join keys, enums, definitions) and redundancy (claims that only restate schema).".to_string(),
        ];
        if uncited {
            guidance.push(
                "This note makes claims but cites no source — prioritize it for grounding review."
                    .to_string(),
            );
        }

        Ok(GroundingAnalysis {
            path: self.rel(&file_path),
            claim_count,
            citation_count,
            has_citations,
            uncited,
            has_schema_section,
            has_examples_section,
            claims_truncated,
            claims,
            citations,
            guidance,
        })
    }

    /// Scan the vault for notes that make claims but cite no source.
    pub async fn find_ungrounded_notes(&self, limit: usize) -> Result<UngroundedReport> {
        // Cache-first: parsed notes validated against disk mtime, no re-scan.
        let files = self.manager.vault_files_validated().await;
        let mut total_notes = 0usize;
        let mut ungrounded: Vec<UngroundedNote> = Vec::new();

        for vault_file in &files {
            total_notes += 1;
            let body = &vault_file.content;
            if parse_citations(body).is_empty() {
                let claim_count = extract_claims(body).len();
                if claim_count > 0 {
                    ungrounded.push(UngroundedNote {
                        path: self.rel(&vault_file.path),
                        claim_count,
                    });
                }
            }
        }

        ungrounded.sort_by(|a, b| b.claim_count.cmp(&a.claim_count).then(a.path.cmp(&b.path)));
        let ungrounded_count = ungrounded.len();
        ungrounded.truncate(limit);

        Ok(UngroundedReport {
            total_notes,
            ungrounded_count,
            notes: ungrounded,
        })
    }
}

/// Extract candidate factual claims (declarative sentences) from prose blocks.
///
/// Walks paragraphs, blockquotes, and list items (skipping code, tables, and
/// headings — they aren't prose claims), splits their plain text into
/// sentences, and keeps those with at least [`MIN_CLAIM_WORDS`] words.
fn extract_claims(body: &str) -> Vec<String> {
    let mut prose = String::new();
    for block in parse_blocks(body) {
        collect_prose(&block, &mut prose);
    }

    let mut claims = Vec::new();
    for sentence in split_sentences(&prose) {
        let s = sentence.trim();
        let words = s.split_whitespace().count();
        if words >= MIN_CLAIM_WORDS
            && s.chars().any(|c| c.is_alphabetic())
            && !s.starts_with("http://")
            && !s.starts_with("https://")
        {
            claims.push(s.to_string());
        }
    }
    claims
}

/// Append the plain-text prose of claim-bearing blocks to `out`.
fn collect_prose(block: &ContentBlock, out: &mut String) {
    match block {
        ContentBlock::Paragraph { .. }
        | ContentBlock::Blockquote { .. }
        | ContentBlock::List { .. } => {
            let text = block.to_plain_text();
            if !text.trim().is_empty() {
                out.push_str(text.trim());
                out.push('\n');
            }
        }
        // Headings, code, tables, images, rules, details: not prose claims.
        _ => {}
    }
}

/// Split text into sentences on `.`/`?`/`!` boundaries (newlines also separate).
fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == '\n' {
            if !current.trim().is_empty() {
                sentences.push(std::mem::take(&mut current));
            } else {
                current.clear();
            }
            continue;
        }
        current.push(c);
        if matches!(c, '.' | '?' | '!') {
            // Sentence boundary when followed by whitespace/end (avoids "3.5", "e.g").
            let next_is_break = chars.get(i + 1).map(|n| n.is_whitespace()).unwrap_or(true);
            if next_is_break && !current.trim().is_empty() {
                sentences.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.trim().is_empty() {
        sentences.push(current);
    }
    sentences
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn make_manager(vault_dir: &Path) -> Arc<VaultManager> {
        use turbovault_core::{ServerConfig, VaultConfig};
        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder("test", vault_dir).build().unwrap());
        Arc::new(VaultManager::new(config).unwrap())
    }

    #[test]
    fn extracts_prose_sentences_as_claims() {
        let body = "# Schema\n\nThe orders table has one row per completed order. It is joined to customers on customer_id.\n\n```sql\nSELECT 1\n```\n";
        let claims = extract_claims(body);
        assert_eq!(claims.len(), 2);
        assert!(claims[0].contains("one row per completed order"));
        // Code block content is not a claim.
        assert!(!claims.iter().any(|c| c.contains("SELECT")));
    }

    #[test]
    fn short_fragments_are_not_claims() {
        let body = "Hello world.\n\nThis sentence is long enough to count as a claim here.\n";
        let claims = extract_claims(body);
        assert_eq!(claims.len(), 1);
    }

    #[tokio::test]
    async fn analyze_note_flags_uncited_claims() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("uncited.md"),
            "---\ntype: Table\n---\n# Schema\n\nThe orders table holds one row per completed order in USD.\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("cited.md"),
            "---\ntype: Table\n---\n# Notes\n\nThe customers table holds one row per registered customer account.\n\n# Citations\n\n[1] [src](https://x.example)\n",
        )
        .unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = GroundingTools::new(manager);

        let uncited = tools.analyze_note("uncited.md").await.unwrap();
        assert!(uncited.claim_count >= 1);
        assert_eq!(uncited.citation_count, 0);
        assert!(uncited.uncited);
        assert!(uncited.has_schema_section);

        let cited = tools.analyze_note("cited.md").await.unwrap();
        assert_eq!(cited.citation_count, 1);
        assert!(!cited.uncited);
    }

    #[tokio::test]
    async fn find_ungrounded_lists_only_uncited_claim_notes() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp.path().join("uncited.md"),
            "---\ntype: Table\n---\nThe orders table holds one row per completed order today.\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("cited.md"),
            "The customers table holds one row per registered account here.\n\n# Citations\n\n[1] [s](https://x.example)\n",
        )
        .unwrap();
        std::fs::write(
            temp.path().join("empty.md"),
            "---\ntype: Table\n---\n# Just a heading\n",
        )
        .unwrap();

        let manager = make_manager(temp.path());
        manager.initialize().await.unwrap();
        let tools = GroundingTools::new(manager);

        let report = tools.find_ungrounded_notes(10).await.unwrap();
        assert_eq!(report.total_notes, 3);
        assert_eq!(report.ungrounded_count, 1);
        assert_eq!(report.notes[0].path, "uncited.md");
    }
}
