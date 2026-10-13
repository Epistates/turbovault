//! Content quality evaluation tools
//!
//! Evaluates note quality across four dimensions:
//! - **Readability**: Flesch-Kincaid grade level, sentence/word metrics, vocabulary diversity
//! - **Structure**: Heading hierarchy, frontmatter, tags, links
//! - **Completeness**: Word count, link density, metadata richness
//! - **Staleness**: Modification recency, freshness relative to linked notes

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use turbovault_core::prelude::*;
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

/// Quality evaluation tools
#[derive(Clone)]
pub struct QualityTools {
    pub manager: Arc<VaultManager>,
}

/// Composite quality score for a single note
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct QualityScore {
    pub path: String,
    pub overall_score: u8,
    pub readability: ReadabilityScore,
    pub structure: StructureScore,
    pub completeness: CompletenessScore,
    pub staleness: StalenessScore,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ReadabilityScore {
    pub score: u8,
    pub flesch_kincaid_grade: f64,
    pub avg_sentence_length: f64,
    pub avg_word_length: f64,
    pub vocabulary_diversity: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StructureScore {
    pub score: u8,
    pub has_title: bool,
    pub heading_count: usize,
    pub has_frontmatter: bool,
    pub tag_count: usize,
    pub has_links: bool,
    pub link_count: usize,
    pub heading_hierarchy_valid: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct CompletenessScore {
    pub score: u8,
    pub word_count: usize,
    pub link_density: f64,
    pub has_tags: bool,
    pub has_frontmatter: bool,
    pub frontmatter_keys: usize,
    pub has_outgoing_links: bool,
    pub has_incoming_links: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct StalenessScore {
    pub score: u8,
    pub days_since_modified: u64,
    pub days_since_created: u64,
    pub linked_notes_newer: usize,
}

/// Score distribution buckets
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ScoreDistribution {
    pub poor: usize,      // 0-20
    pub below_avg: usize, // 21-40
    pub average: usize,   // 41-60
    pub good: usize,      // 61-80
    pub excellent: usize, // 81-100
}

/// Dimension averages across the vault
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DimensionAverages {
    pub readability: f64,
    pub structure: f64,
    pub completeness: f64,
    pub staleness: f64,
}

/// Vault-wide quality report
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct VaultQualityReport {
    pub total_notes: usize,
    pub average_score: f64,
    pub score_distribution: ScoreDistribution,
    pub lowest_quality: Vec<QualityScore>,
    pub highest_quality: Vec<QualityScore>,
    pub dimension_averages: DimensionAverages,
    pub recommendations: Vec<String>,
}

// Scoring weights
const READABILITY_WEIGHT: f64 = 0.20;
const STRUCTURE_WEIGHT: f64 = 0.30;
const COMPLETENESS_WEIGHT: f64 = 0.30;
const STALENESS_WEIGHT: f64 = 0.20;

impl QualityTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Evaluate quality of a single note
    pub async fn evaluate_note(&self, path: &str) -> Result<QualityScore> {
        let vault_file = Arc::new(self.manager.parse_file(&PathBuf::from(path)).await?);
        let notes = [vault_file];
        let facts = self.link_facts(&notes).await;
        let facts = facts.get(&notes[0].path).copied().unwrap_or_default();
        Ok(score_note(&notes[0], path, facts, current_timestamp()))
    }

    /// What scoring each of `notes` needs from the rest of the vault: read
    /// under one short hold of the link graph, with linked notes' times
    /// from the note cache rather than each one parsed again from disk.
    async fn link_facts(&self, notes: &[Arc<VaultFile>]) -> HashMap<PathBuf, LinkFacts> {
        let modified: HashMap<PathBuf, f64> = self
            .manager
            .cached_notes()
            .await
            .iter()
            .map(|file| (file.path.clone(), file.metadata.modified_at))
            .collect();
        let graph = self.manager.link_graph_flushed().await;
        let graph = graph.read().await;
        notes
            .iter()
            .map(|file| {
                let newer_links = graph
                    .forward_link_targets(&file.path)
                    .iter()
                    .filter(|target| {
                        modified
                            .get(**target)
                            .is_some_and(|&linked| linked > file.metadata.modified_at)
                    })
                    .count();
                let facts = LinkFacts {
                    has_incoming: graph.backlink_source_count(&file.path) > 0,
                    newer_links,
                };
                (file.path.clone(), facts)
            })
            .collect()
    }

    /// Score every one of `notes`, in parallel off the async workers.
    /// `now` is the time staleness is measured from.
    async fn score_notes(&self, notes: Vec<Arc<VaultFile>>, now: f64) -> Result<Vec<QualityScore>> {
        let facts = self.link_facts(&notes).await;
        let notes: Vec<(String, Arc<VaultFile>)> = notes
            .into_iter()
            .map(|file| (self.manager.relative_path(&file.path), file))
            .collect();
        tokio::task::spawn_blocking(move || {
            notes
                .par_iter()
                .map(|(rel_path, file)| {
                    let facts = facts.get(&file.path).copied().unwrap_or_default();
                    score_note(file, rel_path, facts, now)
                })
                .collect()
        })
        .await
        .map_err(|e| Error::other(format!("quality scoring task failed: {e}")))
    }
}

/// A note's links as the rest of the vault sees them.
#[derive(Debug, Clone, Copy, Default)]
struct LinkFacts {
    /// Some other note links to it.
    has_incoming: bool,
    /// How many of the notes it links to were modified after it.
    newer_links: usize,
}

/// Score an already-parsed note. `rel_path` is the vault-relative path the
/// result reports; `facts` carries what the link graph says about it.
fn score_note(vault_file: &VaultFile, rel_path: &str, facts: LinkFacts, now: f64) -> QualityScore {
    let plain_content = to_plain_text(&vault_file.content);

    let readability = compute_readability(&plain_content);
    let structure = compute_structure(vault_file);
    let completeness = compute_completeness(vault_file, facts.has_incoming);
    let staleness = compute_staleness(vault_file, facts.newer_links, now);

    let overall = weighted_score(
        readability.score,
        structure.score,
        completeness.score,
        staleness.score,
    );

    let recommendations =
        generate_recommendations(&readability, &structure, &completeness, &staleness);

    QualityScore {
        path: rel_path.to_string(),
        overall_score: overall,
        readability,
        structure,
        completeness,
        staleness,
        recommendations,
    }
}

impl QualityTools {
    /// Generate vault-wide quality report
    pub async fn vault_quality_report(&self, bottom_n: usize) -> Result<VaultQualityReport> {
        // Cache-first: parsed notes validated against disk mtime, no re-scan.
        let files = self.manager.notes_validated().await;
        let mut scores = self.score_notes(files, current_timestamp()).await?;

        let total_notes = scores.len();
        if total_notes == 0 {
            return Ok(VaultQualityReport {
                total_notes: 0,
                average_score: 0.0,
                score_distribution: ScoreDistribution {
                    poor: 0,
                    below_avg: 0,
                    average: 0,
                    good: 0,
                    excellent: 0,
                },
                lowest_quality: vec![],
                highest_quality: vec![],
                dimension_averages: DimensionAverages {
                    readability: 0.0,
                    structure: 0.0,
                    completeness: 0.0,
                    staleness: 0.0,
                },
                recommendations: vec![],
            });
        }

        let average_score =
            scores.iter().map(|s| s.overall_score as f64).sum::<f64>() / total_notes as f64;

        let mut dist = ScoreDistribution {
            poor: 0,
            below_avg: 0,
            average: 0,
            good: 0,
            excellent: 0,
        };
        for s in &scores {
            match s.overall_score {
                0..=20 => dist.poor += 1,
                21..=40 => dist.below_avg += 1,
                41..=60 => dist.average += 1,
                61..=80 => dist.good += 1,
                _ => dist.excellent += 1,
            }
        }

        let dim_avg = DimensionAverages {
            readability: scores
                .iter()
                .map(|s| s.readability.score as f64)
                .sum::<f64>()
                / total_notes as f64,
            structure: scores.iter().map(|s| s.structure.score as f64).sum::<f64>()
                / total_notes as f64,
            completeness: scores
                .iter()
                .map(|s| s.completeness.score as f64)
                .sum::<f64>()
                / total_notes as f64,
            staleness: scores.iter().map(|s| s.staleness.score as f64).sum::<f64>()
                / total_notes as f64,
        };

        // Sort for lowest/highest
        scores.sort_by_key(|s| s.overall_score);
        let lowest = scores.iter().take(bottom_n).cloned().collect::<Vec<_>>();
        let highest = scores
            .iter()
            .rev()
            .take(bottom_n)
            .cloned()
            .collect::<Vec<_>>();

        // Vault-level recommendations
        let mut recommendations = Vec::new();
        if dim_avg.readability < 50.0 {
            recommendations.push("Vault readability is below average — consider breaking up long sentences and using simpler vocabulary".to_string());
        }
        if dim_avg.structure < 50.0 {
            recommendations.push(
                "Many notes lack proper structure — add headings, frontmatter, and tags"
                    .to_string(),
            );
        }
        if dim_avg.completeness < 50.0 {
            recommendations
                .push("Notes are sparse — add more content, links, and metadata".to_string());
        }
        if dim_avg.staleness < 50.0 {
            recommendations
                .push("Many notes are stale — review and update outdated content".to_string());
        }
        if dist.poor > total_notes / 4 {
            recommendations.push(format!(
                "{} notes scored below 20 — consider reviewing or archiving them",
                dist.poor
            ));
        }

        Ok(VaultQualityReport {
            total_notes,
            average_score,
            score_distribution: dist,
            lowest_quality: lowest,
            highest_quality: highest,
            dimension_averages: dim_avg,
            recommendations,
        })
    }

    /// Find stale notes sorted by staleness
    pub async fn find_stale_notes(
        &self,
        threshold_days: u64,
        limit: usize,
    ) -> Result<Vec<QualityScore>> {
        // Cache-first: parsed notes validated against disk mtime, no re-scan.
        // Whether a note is stale depends only on when it was modified, so
        // only the stale ones are scored.
        let now = current_timestamp();
        let files: Vec<Arc<VaultFile>> = self
            .manager
            .notes_validated()
            .await
            .into_iter()
            .filter(|file| days_since(file.metadata.modified_at, now) >= threshold_days)
            .collect();
        let mut stale = self.score_notes(files, now).await?;

        // Sort by staleness (most stale first), then by path so equal ages
        // come back in the same order every call.
        stale.sort_by(|a, b| {
            b.staleness
                .days_since_modified
                .cmp(&a.staleness.days_since_modified)
                .then_with(|| a.path.cmp(&b.path))
        });
        stale.truncate(limit);
        Ok(stale)
    }
}

fn current_timestamp() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn weighted_score(readability: u8, structure: u8, completeness: u8, staleness: u8) -> u8 {
    let score = readability as f64 * READABILITY_WEIGHT
        + structure as f64 * STRUCTURE_WEIGHT
        + completeness as f64 * COMPLETENESS_WEIGHT
        + staleness as f64 * STALENESS_WEIGHT;
    score.round().clamp(0.0, 100.0) as u8
}

/// Count syllables in a word using English heuristics
fn count_syllables(word: &str) -> usize {
    let word = word.to_lowercase();
    if word.len() <= 2 {
        return 1;
    }

    let vowels: HashSet<char> = ['a', 'e', 'i', 'o', 'u', 'y'].into();
    let mut count = 0;
    let mut prev_vowel = false;
    let chars: Vec<char> = word.chars().collect();

    for &ch in &chars {
        let is_vowel = vowels.contains(&ch);
        if is_vowel && !prev_vowel {
            count += 1;
        }
        prev_vowel = is_vowel;
    }

    // Silent e
    if word.ends_with('e') && count > 1 {
        count -= 1;
    }

    count.max(1)
}

fn compute_readability(plain_text: &str) -> ReadabilityScore {
    if plain_text.trim().is_empty() {
        return ReadabilityScore {
            score: 0,
            flesch_kincaid_grade: 0.0,
            avg_sentence_length: 0.0,
            avg_word_length: 0.0,
            vocabulary_diversity: 0.0,
        };
    }

    // Count sentences (split on sentence-ending punctuation followed by whitespace or end)
    let sentences: Vec<&str> = plain_text
        .split(['.', '!', '?'])
        .filter(|s| !s.trim().is_empty())
        .collect();
    let sentence_count = sentences.len().max(1);

    // Count words
    let words: Vec<&str> = plain_text.split_whitespace().collect();
    let word_count = words.len().max(1);

    // Count syllables
    let total_syllables: usize = words.iter().map(|w| count_syllables(w)).sum();

    // Average metrics
    let avg_sentence_length = word_count as f64 / sentence_count as f64;
    let avg_word_length = words.iter().map(|w| w.len()).sum::<usize>() as f64 / word_count as f64;

    // Vocabulary diversity (type-token ratio)
    let unique_words: HashSet<String> = words.iter().map(|w| w.to_lowercase()).collect();
    let vocabulary_diversity = unique_words.len() as f64 / word_count as f64;

    // Flesch-Kincaid Grade Level
    let fk_grade =
        0.39 * avg_sentence_length + 11.8 * (total_syllables as f64 / word_count as f64) - 15.59;
    let fk_grade = fk_grade.clamp(-3.0, 30.0);

    // Convert FK grade to 0-100 score
    // Grade 0-8 = excellent (90-100) - very readable
    // Grade 9-12 = good (70-89) - general audience
    // Grade 13-16 = fair (50-69) - college level
    // Grade >16 = poor (<50) - academic/dense
    let score = if fk_grade < 0.0 {
        95 // very simple text = highly readable
    } else if fk_grade <= 8.0 {
        90 + ((8.0 - fk_grade) * 1.25) as u8
    } else if fk_grade <= 12.0 {
        70 + ((12.0 - fk_grade) * 5.0) as u8
    } else if fk_grade <= 16.0 {
        50 + ((16.0 - fk_grade) * 5.0) as u8
    } else {
        (50.0 - (fk_grade - 16.0) * 3.0).clamp(0.0, 49.0) as u8
    };

    // Penalize very low diversity (copy-paste text)
    let diversity_penalty = if vocabulary_diversity < 0.3 { 15 } else { 0 };

    ReadabilityScore {
        score: score.saturating_sub(diversity_penalty),
        flesch_kincaid_grade: (fk_grade * 100.0).round() / 100.0,
        avg_sentence_length: (avg_sentence_length * 100.0).round() / 100.0,
        avg_word_length: (avg_word_length * 100.0).round() / 100.0,
        vocabulary_diversity: (vocabulary_diversity * 1000.0).round() / 1000.0,
    }
}

fn compute_structure(vault_file: &VaultFile) -> StructureScore {
    let has_title = vault_file.headings.iter().any(|h| h.level == 1);
    let heading_count = vault_file.headings.len();
    let has_frontmatter = vault_file.frontmatter.is_some();
    let tag_count = vault_file.tags.len();
    let link_count = vault_file.links.len();
    let has_links = link_count > 0;

    // Check heading hierarchy (no skipped levels)
    let heading_hierarchy_valid = check_heading_hierarchy(&vault_file.headings);

    let mut score = 0u8;

    // Title (20 points)
    if has_title {
        score += 20;
    }

    // Headings (15 points)
    if heading_count >= 2 {
        score += 15;
    } else if heading_count == 1 {
        score += 8;
    }

    // Hierarchy validity (10 points)
    if heading_hierarchy_valid {
        score += 10;
    }

    // Frontmatter (20 points)
    if has_frontmatter {
        score += 20;
    }

    // Tags (15 points)
    if tag_count >= 2 {
        score += 15;
    } else if tag_count == 1 {
        score += 8;
    }

    // Links (20 points)
    if link_count >= 3 {
        score += 20;
    } else if link_count >= 1 {
        score += 10;
    }

    StructureScore {
        score,
        has_title,
        heading_count,
        has_frontmatter,
        tag_count,
        has_links,
        link_count,
        heading_hierarchy_valid,
    }
}

fn check_heading_hierarchy(headings: &[turbovault_core::Heading]) -> bool {
    if headings.is_empty() {
        return true;
    }

    let mut prev_level = 0;
    for h in headings {
        if prev_level == 0 && h.level > 1 {
            return false; // First heading should be H1
        }
        if h.level > prev_level + 1 && prev_level > 0 {
            return false; // Skipped a level (e.g., H1 → H3)
        }
        prev_level = h.level;
    }
    true
}

fn compute_completeness(vault_file: &VaultFile, has_incoming_links: bool) -> CompletenessScore {
    let word_count = vault_file.content.split_whitespace().count();
    let link_count = vault_file.links.len();
    let link_density = if word_count > 0 {
        (link_count as f64 / word_count as f64) * 100.0
    } else {
        0.0
    };

    let has_tags = !vault_file.tags.is_empty();
    let has_frontmatter = vault_file.frontmatter.is_some();
    let frontmatter_keys = vault_file
        .frontmatter
        .as_ref()
        .map(|fm| fm.data.len())
        .unwrap_or(0);
    let has_outgoing_links = link_count > 0;

    let mut score = 0u8;

    // Word count (25 points)
    if word_count >= 300 {
        score += 25;
    } else if word_count >= 100 {
        score += 15;
    } else if word_count >= 30 {
        score += 8;
    }

    // Link density (20 points)
    if link_density >= 1.0 {
        score += 20;
    } else if link_density >= 0.5 {
        score += 12;
    } else if link_count > 0 {
        score += 5;
    }

    // Metadata (20 points)
    if has_frontmatter && frontmatter_keys >= 3 {
        score += 20;
    } else if has_frontmatter {
        score += 10;
    }

    // Tags (15 points)
    if has_tags {
        score += 15;
    }

    // Bidirectional links (20 points)
    if has_outgoing_links && has_incoming_links {
        score += 20;
    } else if has_outgoing_links || has_incoming_links {
        score += 10;
    }

    CompletenessScore {
        score,
        word_count,
        link_density: (link_density * 100.0).round() / 100.0,
        has_tags,
        has_frontmatter,
        frontmatter_keys,
        has_outgoing_links,
        has_incoming_links,
    }
}

/// Whole days from `then` to `now` (seconds since the epoch), never negative.
fn days_since(then: f64, now: f64) -> u64 {
    ((now - then) / 86400.0).max(0.0) as u64
}

/// `linked_notes_newer` is how many of the notes this one links to were
/// modified after it.
fn compute_staleness(
    vault_file: &VaultFile,
    linked_notes_newer: usize,
    now: f64,
) -> StalenessScore {
    let days_since_modified = days_since(vault_file.metadata.modified_at, now);
    let days_since_created = days_since(vault_file.metadata.created_at, now);

    // Score: freshly modified = 100, older = lower
    let score: u8 = if days_since_modified == 0 {
        100
    } else if days_since_modified <= 7 {
        90
    } else if days_since_modified <= 30 {
        75
    } else if days_since_modified <= 90 {
        55
    } else if days_since_modified <= 180 {
        35
    } else if days_since_modified <= 365 {
        20
    } else {
        10
    };

    // Penalize if many linked notes are newer (this note may be outdated)
    let newer_penalty: u8 = (linked_notes_newer.min(20) as u8) * 2;

    StalenessScore {
        score: score.saturating_sub(newer_penalty),
        days_since_modified,
        days_since_created,
        linked_notes_newer,
    }
}

fn generate_recommendations(
    readability: &ReadabilityScore,
    structure: &StructureScore,
    completeness: &CompletenessScore,
    staleness: &StalenessScore,
) -> Vec<String> {
    let mut recs = Vec::new();

    if readability.score < 50 {
        if readability.avg_sentence_length > 25.0 {
            recs.push("Break up long sentences for better readability".to_string());
        }
        if readability.vocabulary_diversity < 0.4 {
            recs.push("Diversify vocabulary — text may be repetitive".to_string());
        }
        if readability.flesch_kincaid_grade > 14.0 {
            recs.push("Simplify language — reading level is very high".to_string());
        }
    }

    if !structure.has_title {
        recs.push("Add a H1 title heading".to_string());
    }
    if !structure.has_frontmatter {
        recs.push("Add YAML frontmatter with metadata".to_string());
    }
    if structure.tag_count == 0 {
        recs.push("Add tags for discoverability".to_string());
    }
    if !structure.heading_hierarchy_valid {
        recs.push(
            "Fix heading hierarchy — avoid skipping levels (e.g., H1 directly to H3)".to_string(),
        );
    }

    if completeness.word_count < 50 {
        recs.push("Note is very short — consider expanding content".to_string());
    }
    if !completeness.has_outgoing_links {
        recs.push("Add outgoing links to connect this note to related topics".to_string());
    }
    if !completeness.has_incoming_links {
        recs.push("This note has no backlinks — link to it from related notes".to_string());
    }

    if staleness.days_since_modified > 180 {
        recs.push(format!(
            "Note hasn't been updated in {} days — review for accuracy",
            staleness.days_since_modified
        ));
    }
    if staleness.linked_notes_newer > 3 {
        recs.push(format!(
            "{} linked notes have been updated more recently — this note may be outdated",
            staleness.linked_notes_newer
        ));
    }

    recs.truncate(5);
    recs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Staleness reads the note's real modification time. It was always
    /// 0.0, the Unix epoch, so every note was decades stale.
    #[tokio::test]
    async fn find_stale_notes_tells_old_from_new() {
        use turbovault_core::config::{ServerConfig, VaultConfig};

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::write(tmp.path().join("fresh.md"), "# Fresh\n\nWritten today.\n").unwrap();
        let old = tmp.path().join("old.md");
        std::fs::write(&old, "# Old\n\nWritten long ago.\n").unwrap();
        let hundred_days = std::time::Duration::from_secs(100 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - hundred_days)
            .unwrap();

        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder("v", tmp.path()).build().unwrap());
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();

        let stale = QualityTools::new(manager)
            .find_stale_notes(30, 10)
            .await
            .unwrap();
        let paths: Vec<&str> = stale.iter().map(|score| score.path.as_str()).collect();
        assert_eq!(paths, ["old.md"]);
        assert_eq!(stale[0].staleness.days_since_modified, 100);
    }

    /// The link facts gathered in one pass for the whole vault are the ones
    /// scoring used to ask the manager for note by note: backlinks, forward
    /// links, and each linked note parsed again for its modification time.
    #[tokio::test]
    async fn link_facts_match_asking_note_by_note() {
        use turbovault_core::config::{ServerConfig, VaultConfig};

        let tmp = tempfile::TempDir::new().unwrap();
        let notes = [
            ("hub.md", "[[a]] [[b]] [[c]] [[a]]", 50),
            ("a.md", "[[hub]]", 10),
            ("b.md", "[[c]] [[missing]]", 80),
            ("c.md", "no links", 5),
            ("lonely.md", "nothing", 1),
        ];
        for (path, body, days_old) in notes {
            let full = tmp.path().join(path);
            std::fs::write(&full, format!("# {path}\n\n{body}\n")).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&full)
                .unwrap()
                .set_modified(SystemTime::now() - std::time::Duration::from_secs(days_old * 86_400))
                .unwrap();
        }
        let mut config = ServerConfig::new();
        config
            .vaults
            .push(VaultConfig::builder("v", tmp.path()).build().unwrap());
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();

        let tools = QualityTools::new(Arc::clone(&manager));
        let cached = manager.notes_validated().await;
        let facts = tools.link_facts(&cached).await;
        for file in &cached {
            let rel = PathBuf::from(manager.relative_path(&file.path));
            let has_incoming = !manager.get_backlinks(&rel).await.unwrap().is_empty();
            let mut newer = 0;
            for linked in manager.get_forward_links(&rel).await.unwrap() {
                if manager
                    .parse_file(&linked)
                    .await
                    .unwrap()
                    .metadata
                    .modified_at
                    > file.metadata.modified_at
                {
                    newer += 1;
                }
            }
            let found = facts[&file.path];
            assert_eq!(found.has_incoming, has_incoming, "{}", rel.display());
            assert_eq!(found.newer_links, newer, "{}", rel.display());
        }
        // a and c are newer than the hub; b is older.
        let hub = facts
            .iter()
            .find(|(path, _)| path.file_name() == Some(std::ffi::OsStr::new("hub.md")))
            .expect("hub.md has facts");
        assert_eq!(hub.1.newer_links, 2);
    }

    #[test]
    fn test_count_syllables() {
        assert_eq!(count_syllables("the"), 1);
        assert_eq!(count_syllables("hello"), 2);
        assert_eq!(count_syllables("beautiful"), 3);
        assert_eq!(count_syllables("a"), 1);
    }

    #[test]
    fn test_readability_empty() {
        let score = compute_readability("");
        assert_eq!(score.score, 0);
    }

    #[test]
    fn test_readability_simple() {
        let text = "The cat sat on the mat. The dog ran in the park.";
        let score = compute_readability(text);
        assert!(
            score.score > 50,
            "Simple text should score well: {}",
            score.score
        );
        assert!(score.avg_sentence_length < 15.0);
    }

    #[test]
    fn test_weighted_score() {
        let score = weighted_score(80, 80, 80, 80);
        assert_eq!(score, 80);

        let score = weighted_score(100, 100, 100, 100);
        assert_eq!(score, 100);

        let score = weighted_score(0, 0, 0, 0);
        assert_eq!(score, 0);
    }

    #[test]
    fn test_heading_hierarchy() {
        use turbovault_core::{Heading, SourcePosition};

        let pos = SourcePosition::new(0, 0, 0, 0);

        let good = vec![
            Heading {
                text: "H1".into(),
                level: 1,
                position: pos,
                anchor: None,
            },
            Heading {
                text: "H2".into(),
                level: 2,
                position: pos,
                anchor: None,
            },
            Heading {
                text: "H3".into(),
                level: 3,
                position: pos,
                anchor: None,
            },
        ];
        assert!(check_heading_hierarchy(&good));

        let bad = vec![
            Heading {
                text: "H1".into(),
                level: 1,
                position: pos,
                anchor: None,
            },
            Heading {
                text: "H3".into(),
                level: 3,
                position: pos,
                anchor: None,
            },
        ];
        assert!(!check_heading_hierarchy(&bad));
    }
}
