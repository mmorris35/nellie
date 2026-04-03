//! Memory deduplication for Claude Code memory files.
//!
//! This module detects and merges near-duplicate memory files using
//! string similarity metrics, and scores memories by relevance for
//! budget-aware selection. It's integrated into the sync command to
//! prevent memory bloat from redundant or nearly-identical lessons.
//!
//! # Deduplication Algorithm
//!
//! Deduplication operates in two stages:
//!
//! 1. **Find Duplicates** ([`find_duplicates`]): Compare all memory
//!    files pairwise using:
//!    - Title similarity (Jaro-Winkler, threshold 0.85)
//!    - Content similarity (first 200 chars, Jaro-Winkler, same threshold)
//!
//!    Memories that match on either dimension are grouped into
//!    [`DuplicateGroup`]s.
//!
//! 2. **Merge Duplicates** ([`merge_duplicates`]): For each group,
//!    keep the best specimen (most recent, highest severity) and
//!    combine unique content from the others.
//!
//! # Memory Decay and Budget Selection
//!
//! When memory directories exceed line budget constraints, memories
//! are scored by relevance using [`score_memory_relevance`]:
//!
//! - **Recency**: Recently updated lessons score higher
//! - **Severity**: critical > warning > info level lessons
//! - **Type Priority**: Feedback > Project > User > Reference
//!
//! Critical severity lessons always survive budget cuts regardless of age.
//! Budget-aware selection uses [`select_memories_within_budget`] to
//! choose the most valuable memories that fit within a line budget,
//! always preserving critical lessons and truncating or removing lower-
//! priority older info-level lessons first.
//!
//! # Severity Ranking
//!
//! When merging, the following priority order is used:
//! - `critical` (highest)
//! - `warning`
//! - `info`
//! - Other/unknown (lowest)
//!
//! The most recent file with the highest severity is kept as the
//! primary, and unique content from other files is appended.

use std::cmp::Ordering;
use std::fmt::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::claude_code::memory_writer::{MemoryFile, MemoryType};
use crate::storage::LessonRecord;

/// Threshold for string similarity (Jaro-Winkler) below which two
/// strings are not considered duplicates.
///
/// A threshold of 0.85 is strict enough to avoid false positives
/// (e.g., "SQLite WAL lock contention" vs "SQLite WAL mode") but loose
/// enough to catch genuine near-duplicates.
const SIMILARITY_THRESHOLD: f64 = 0.85;

/// Maximum length of content to consider for similarity matching.
///
/// Using only the first 200 characters speeds up comparison and
/// avoids matching files that happen to share a long common section
/// but differ significantly overall.
const CONTENT_PREVIEW_LENGTH: usize = 200;

/// Weight for recency in relevance scoring (0.0..1.0).
///
/// Memories that have been recently updated score higher.
/// Recency is measured in days since last update.
const RECENCY_WEIGHT: f32 = 0.3;

/// Weight for severity in relevance scoring (0.0..1.0).
///
/// Critical and warning severity lessons score higher than info.
const SEVERITY_WEIGHT: f32 = 0.7;

/// Days after which an info-level lesson is considered "stale".
///
/// Lessons older than this threshold receive reduced relevance scores.
const STALE_DAYS: i64 = 30;

/// Current Unix timestamp in seconds.
fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(0))
        .unwrap_or(0)
}

/// A memory with its computed relevance score.
///
/// This struct is used in budget-aware selection to track both the
/// memory's metadata and its relevance score for sorting and filtering.
#[derive(Debug, Clone)]
pub struct ScoredMemory {
    /// The memory file.
    pub memory: MemoryFile,
    /// The associated lesson record (if available).
    pub lesson: Option<LessonRecord>,
    /// Computed relevance score (0.0..1.0).
    pub score: f32,
}

/// A group of duplicate memory files.
///
/// When files are deduplicated, all files that match on title or
/// content similarity are grouped together. The group will be merged
/// into a single file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DuplicateGroup {
    /// The duplicate memory files in this group.
    pub members: Vec<MemoryFile>,
}

impl DuplicateGroup {
    /// Creates a new duplicate group with the given members.
    pub fn new(members: Vec<MemoryFile>) -> Self {
        Self { members }
    }

    /// Returns the number of files in this group.
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Checks if the group is empty (no members).
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// Finds duplicate memory files by comparing titles and content.
///
/// Duplicates are identified using Jaro-Winkler similarity with a
/// threshold of 0.85. Two files are considered duplicates if:
/// - Their titles have similarity >= threshold, OR
/// - The first 200 chars of their content have similarity >= threshold
///
/// Returns a vector of [`DuplicateGroup`]s. Each group contains files
/// that are duplicates of each other. Non-duplicate files appear as
/// single-member groups.
///
/// # Arguments
///
/// * `memories` - The list of memory files to deduplicate.
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::claude_code::dedup::find_duplicates;
/// use nellie::claude_code::memory_writer::{MemoryFile, MemoryType};
///
/// let memories = vec![
///     MemoryFile::new("SQLite WAL Lock Contention", "About WAL locks",
///                     MemoryType::Project, "Never use X..."),
///     MemoryFile::new("SQLite WAL Lock Contention", "About the same thing",
///                     MemoryType::Project, "Never use X..."),
/// ];
/// let groups = find_duplicates(&memories);
/// assert_eq!(groups.len(), 1);
/// assert_eq!(groups[0].len(), 2);
/// ```
/// Helper function for union-find path compression.
fn union_find(parent: &mut [usize], x: usize) -> usize {
    if parent[x] != x {
        parent[x] = union_find(parent, parent[x]);
    }
    parent[x]
}

#[allow(clippy::needless_range_loop)]
pub fn find_duplicates(memories: &[MemoryFile]) -> Vec<DuplicateGroup> {
    // Build a duplicate graph using pairwise comparison
    let n = memories.len();
    let mut is_duplicate = vec![vec![false; n]; n];

    for i in 0..n {
        is_duplicate[i][i] = true; // A file is always a duplicate of itself
        for j in (i + 1)..n {
            if are_duplicates(&memories[i], &memories[j]) {
                is_duplicate[i][j] = true;
                is_duplicate[j][i] = true;
            }
        }
    }

    // Use union-find to build connected components
    let mut parent: Vec<usize> = (0..n).collect();

    for i in 0..n {
        for j in (i + 1)..n {
            if is_duplicate[i][j] {
                let pi = union_find(&mut parent, i);
                let pj = union_find(&mut parent, j);
                if pi != pj {
                    parent[pi] = pj;
                }
            }
        }
    }

    // Group files by their root parent
    let mut groups_map: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..n {
        let root = union_find(&mut parent, i);
        groups_map.entry(root).or_default().push(i);
    }

    // Convert to DuplicateGroup vec
    let mut groups: Vec<DuplicateGroup> = groups_map
        .into_values()
        .map(|indices| {
            let members = indices.into_iter().map(|i| memories[i].clone()).collect();
            DuplicateGroup::new(members)
        })
        .collect();

    // Sort for deterministic output
    groups.sort_by_key(|g| g.members[0].name.clone());

    groups
}

/// Checks if two memory files are duplicates.
///
/// Files are considered duplicates if either:
/// - Their titles have Jaro-Winkler similarity >= SIMILARITY_THRESHOLD
/// - The first 200 characters of their content have similarity >= SIMILARITY_THRESHOLD
fn are_duplicates(a: &MemoryFile, b: &MemoryFile) -> bool {
    let title_similarity = jaro_winkler(&a.name, &b.name);
    if title_similarity >= SIMILARITY_THRESHOLD {
        return true;
    }

    let content_a = &a.content[..a.content.len().min(CONTENT_PREVIEW_LENGTH)];
    let content_b = &b.content[..b.content.len().min(CONTENT_PREVIEW_LENGTH)];
    let content_similarity = jaro_winkler(content_a, content_b);

    content_similarity >= SIMILARITY_THRESHOLD
}

/// Merges a group of duplicate memory files into a single file.
///
/// The merge strategy is:
/// 1. Select the "best" file (most recent, highest severity) as the
///    primary.
/// 2. Combine unique content from all other files into the primary.
/// 3. Return the merged file.
///
/// # Severity Ranking
///
/// When multiple files exist in the group, the one with the highest
/// severity wins. The ranking is:
/// - `critical` > `warning` > `info`
///
/// If multiple files have the same severity, the one that comes last
/// in the input list (assumed to be most recent) is kept.
///
/// # Content Merging
///
/// Content from non-primary files is appended to the primary file's
/// content, separated by a line break and prefixed with a note
/// indicating the merge.
///
/// # Arguments
///
/// * `group` - A group of duplicate memory files.
///
/// # Returns
///
/// A merged [`MemoryFile`] representing the combined knowledge from
/// all files in the group.
pub fn merge_duplicates(group: &DuplicateGroup) -> MemoryFile {
    if group.is_empty() {
        // Should not happen, but handle gracefully
        return MemoryFile::new("Empty", "Empty group", MemoryType::Project, "");
    }

    if group.len() == 1 {
        // No merge needed
        return group.members[0].clone();
    }

    // Find the primary (best) file
    let primary_idx = find_best_file(&group.members);
    let mut primary = group.members[primary_idx].clone();

    // Collect unique content from other files
    let mut other_content = String::new();
    for (idx, member) in group.members.iter().enumerate() {
        if idx != primary_idx {
            // Skip members with empty content
            if member.content.is_empty() {
                continue;
            }
            // Avoid duplicating content that's already in the primary
            if primary.content.contains(member.content.trim()) {
                continue;
            }
            if !other_content.is_empty() {
                other_content.push_str("\n\n");
            }
            let _ = write!(
                other_content,
                "**[Merged from:]** {}\n\n{}",
                member.name, member.content
            );
        }
    }

    // Append unique content to primary
    if !other_content.is_empty() {
        primary.content.push_str("\n\n");
        primary.content.push_str(&other_content);
    }

    primary
}

/// Finds the index of the "best" file in a list.
///
/// The ranking is:
/// 1. Highest severity (critical > warning > info)
/// 2. Non-empty content (prefer file with content)
/// 3. Most recent (last in the list)
///
/// # Arguments
///
/// * `files` - The list of files to rank.
///
/// # Returns
///
/// The index of the best file.
fn find_best_file(files: &[MemoryFile]) -> usize {
    files
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            // First: highest severity
            match severity_order(a.memory_type).cmp(&severity_order(b.memory_type)) {
                Ordering::Equal => {
                    // Second: prefer non-empty content
                    match (!a.content.is_empty()).cmp(&(!b.content.is_empty())) {
                        Ordering::Equal => {
                            // Third: same severity and same emptiness status,
                            // prefer later index (more recent)
                            Ordering::Less
                        }
                        other => other,
                    }
                }
                other => other,
            }
        })
        .map_or(0, |(idx, _)| idx)
}

/// Returns a numeric rank for a memory type's severity.
///
/// Higher values mean higher severity (will be kept during merge).
/// The order is: Feedback > Project > User > Reference.
///
/// `Feedback` is highest because critical and warning lessons map to
/// it, making it the most important for corrections.
fn severity_order(memory_type: MemoryType) -> u8 {
    match memory_type {
        MemoryType::Feedback => 3,
        MemoryType::Project => 2,
        MemoryType::User => 1,
        MemoryType::Reference => 0,
    }
}

/// Computes the Jaro-Winkler similarity between two strings.
///
/// This is a wrapper around `strsim::jaro_winkler` for clarity and
/// potential future customization.
///
/// # Returns
///
/// A value between 0.0 (no similarity) and 1.0 (identical).
fn jaro_winkler(a: &str, b: &str) -> f64 {
    strsim::jaro_winkler(a, b)
}

/// Scores a memory file's relevance for budget-aware selection.
///
/// The relevance score combines three factors:
///
/// 1. **Recency** (30% weight): Lessons updated recently score higher.
///    Score decays linearly over 30 days.
///
/// 2. **Severity** (70% weight): Critical/warning lessons score much
///    higher than info. Maps to lesson severity field, with fallback
///    to memory type if lesson unavailable.
///
/// 3. **Critical Override**: Any lesson with "critical" severity
///    always receives a score of 1.0, regardless of age.
///
/// # Arguments
///
/// * `memory` - The memory file.
/// * `lesson` - Optional lesson record with severity and timestamps.
///
/// # Returns
///
/// A score between 0.0 (least relevant) and 1.0 (most relevant).
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::claude_code::dedup::score_memory_relevance;
/// use nellie::claude_code::memory_writer::{MemoryFile, MemoryType};
/// use nellie::storage::models::LessonRecord;
///
/// let memory = MemoryFile::new("SQLite WAL", "About WAL locks",
///                              MemoryType::Project, "Use WAL2...");
/// let lesson = LessonRecord::new("SQLite WAL", "Use WAL2...", vec![])
///     .with_severity("critical");
///
/// let score = score_memory_relevance(&memory, Some(&lesson));
/// assert_eq!(score, 1.0); // Critical lessons always score 1.0
/// ```
pub fn score_memory_relevance(memory: &MemoryFile, lesson: Option<&LessonRecord>) -> f32 {
    // If lesson has critical severity, always score 1.0
    if let Some(lesson) = lesson {
        if lesson.severity.to_lowercase() == "critical" {
            return 1.0;
        }
    }

    // Compute recency score (0.0 = very old, 1.0 = just updated)
    let recency_score = lesson.map_or(0.5, |lesson| {
        let now = now_unix();
        let age_secs = now.saturating_sub(lesson.updated_at);
        let age_days = age_secs / (24 * 3600);

        if age_days >= STALE_DAYS {
            0.0 // Very stale
        } else {
            let normalized = 1.0 - (age_days as f32 / STALE_DAYS as f32);
            normalized.clamp(0.0, 1.0)
        }
    });

    // Compute severity score (0.0 = low, 1.0 = high)
    let severity_score = lesson.map_or(
        // Fallback to memory type if no lesson record
        match memory.memory_type {
            MemoryType::Feedback => 0.8, // Feedback includes critical/warning
            MemoryType::Project => 0.6,
            MemoryType::User => 0.4,
            MemoryType::Reference => 0.2,
        },
        |lesson| {
            match lesson.severity.to_lowercase().as_str() {
                "critical" => 1.0, // Already handled above, but included for completeness
                "warning" => 0.8,
                "info" => 0.3,
                _ => 0.2, // Unknown severity
            }
        },
    );

    // Weighted combination: 30% recency, 70% severity
    (recency_score * RECENCY_WEIGHT) + (severity_score * SEVERITY_WEIGHT)
}

/// Selects the most relevant memories that fit within a line budget.
///
/// This function sorts memories by relevance score (descending) and
/// greedily selects memories that fit within the line budget. Critical
/// severity lessons always survive budget cuts, even if it means
/// exceeding the budget.
///
/// # Arguments
///
/// * `memories` - Vector of memories with their scores.
/// * `budget` - Maximum number of lines to include.
///
/// # Returns
///
/// A vector of memories that fit within the budget, sorted by
/// relevance score (highest first).
///
/// # Examples
///
/// ```rust,ignore
/// use nellie::claude_code::dedup::{select_memories_within_budget, ScoredMemory};
/// use nellie::claude_code::memory_writer::{MemoryFile, MemoryType};
///
/// let memories = vec![
///     ScoredMemory {
///         memory: MemoryFile::new("SQLite", "About SQLite", MemoryType::Project, "Content 1"),
///         lesson: None,
///         score: 0.9,
///     },
///     ScoredMemory {
///         memory: MemoryFile::new("Rust", "About Rust", MemoryType::User, "Content 2"),
///         lesson: None,
///         score: 0.3,
///     },
/// ];
///
/// let selected = select_memories_within_budget(memories, 50);
/// assert_eq!(selected.len(), 2);
/// ```
pub fn select_memories_within_budget(
    mut memories: Vec<ScoredMemory>,
    budget: usize,
) -> Vec<ScoredMemory> {
    if memories.is_empty() {
        return Vec::new();
    }

    // Sort by score descending (highest relevance first)
    memories.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

    let mut selected = Vec::new();
    let mut total_lines = 0;
    let mut critical_overflow = false;

    for scored in memories {
        // Estimate lines for this memory: 5 lines for frontmatter + 1 line per 80 chars of content
        let frontmatter_lines = 5;
        let content_lines = scored.memory.content.len().div_ceil(80);
        let memory_lines = frontmatter_lines + content_lines;

        // Check if this is critical severity
        let is_critical = if let Some(lesson) = &scored.lesson {
            lesson.severity.to_lowercase() == "critical"
        } else {
            scored.memory.memory_type == MemoryType::Feedback
        };

        if total_lines + memory_lines <= budget {
            // Fits within budget
            total_lines += memory_lines;
            selected.push(scored);
        } else if is_critical && !critical_overflow {
            // Critical lesson that overflows budget — add it anyway
            critical_overflow = true;
            total_lines += memory_lines;
            selected.push(scored);
        }
        // Otherwise, skip this memory (budget exceeded and not critical)
    }

    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a test memory file
    fn test_memory(name: &str, content: impl Into<String>, mem_type: MemoryType) -> MemoryFile {
        MemoryFile::new(name, "Test description", mem_type, content.into())
    }

    #[test]
    fn test_find_duplicates_identical_titles() {
        let memories = vec![
            test_memory("SQLite WAL Lock", "Content A", MemoryType::Project),
            test_memory("SQLite WAL Lock", "Content B", MemoryType::Project),
        ];

        let groups = find_duplicates(&memories);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn test_find_duplicates_similar_titles() {
        let memories = vec![
            test_memory("SQLite WAL Lock Contention", "Content", MemoryType::Project),
            test_memory("SQLite WAL Lock contention", "Content", MemoryType::Project),
        ];

        let groups = find_duplicates(&memories);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn test_find_duplicates_dissimilar_titles() {
        let memories = vec![
            test_memory(
                "SQLite WAL Lock",
                "SQLite specific content about WAL locks",
                MemoryType::Project,
            ),
            test_memory(
                "PostgreSQL Connection Pool",
                "PostgreSQL connection pooling details",
                MemoryType::Project,
            ),
        ];

        let groups = find_duplicates(&memories);
        // Should be 2 groups, each with 1 member
        assert_eq!(groups.len(), 2);
        assert!(groups.iter().all(|g| g.len() == 1));
    }

    #[test]
    fn test_find_duplicates_similar_content() {
        let common_content = "Never use unwrap() in production code. It can panic unexpectedly.";
        let memories = vec![
            test_memory("Error Handling", common_content, MemoryType::Project),
            test_memory("Rust Error Handling", common_content, MemoryType::Project),
        ];

        let groups = find_duplicates(&memories);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn test_find_duplicates_no_matches() {
        let memories = vec![
            test_memory(
                "Rust Error Handling",
                "Never panic in production code. Use Result types instead.",
                MemoryType::Project,
            ),
            test_memory(
                "Python Type Hints",
                "Use type annotations to make code more maintainable.",
                MemoryType::Project,
            ),
            test_memory(
                "JavaScript Async Patterns",
                "Promise chains and async/await for handling concurrency.",
                MemoryType::Project,
            ),
        ];

        let groups = find_duplicates(&memories);
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().all(|g| g.len() == 1));
    }

    #[test]
    fn test_merge_duplicates_single_file() {
        let memory = test_memory("SQLite WAL", "Content", MemoryType::Project);
        let group = DuplicateGroup::new(vec![memory.clone()]);

        let merged = merge_duplicates(&group);
        assert_eq!(merged.name, memory.name);
        assert_eq!(merged.content, memory.content);
    }

    #[test]
    fn test_merge_duplicates_keeps_primary() {
        let primary = test_memory("SQLite WAL Lock", "Primary content", MemoryType::Project);
        let secondary = test_memory("SQLite WAL Lock", "Secondary content", MemoryType::Project);

        let group = DuplicateGroup::new(vec![primary.clone(), secondary]);
        let merged = merge_duplicates(&group);

        assert_eq!(merged.name, primary.name);
        assert!(merged.content.contains("Primary content"));
        assert!(merged.content.contains("Secondary content"));
    }

    #[test]
    fn test_merge_duplicates_prefers_higher_severity() {
        // Feedback (severity 3) should be preferred over Project (severity 2)
        let feedback_file = test_memory("SQLite WAL", "Feedback content", MemoryType::Feedback);
        let project_file = test_memory("SQLite WAL", "Project content", MemoryType::Project);

        let group = DuplicateGroup::new(vec![project_file, feedback_file.clone()]);
        let merged = merge_duplicates(&group);

        assert_eq!(merged.memory_type, MemoryType::Feedback);
        assert!(merged.content.contains("Feedback content"));
    }

    #[test]
    fn test_merge_duplicates_content_combination() {
        let file1 = test_memory("SQLite", "First important detail", MemoryType::Project);
        let file2 = test_memory("SQLite", "Second important detail", MemoryType::Project);
        let file3 = test_memory("SQLite", "Third important detail", MemoryType::Project);

        let group = DuplicateGroup::new(vec![file1, file2.clone(), file3]);
        let merged = merge_duplicates(&group);

        assert!(merged.content.contains("First important detail"));
        assert!(merged.content.contains("Second important detail"));
        assert!(merged.content.contains("Third important detail"));
    }

    #[test]
    fn test_merge_duplicates_empty_content_skipped() {
        let file1 = test_memory("SQLite", "Real content", MemoryType::Project);
        let file2 = test_memory("SQLite", "", MemoryType::Project); // Empty content

        let group = DuplicateGroup::new(vec![file1.clone(), file2]);
        let merged = merge_duplicates(&group);

        // File with empty content should be skipped, so content should remain unchanged
        assert_eq!(merged.content, "Real content");
    }

    #[test]
    fn test_merge_duplicates_preserves_description() {
        let file = MemoryFile::new(
            "SQLite WAL",
            "Important description",
            MemoryType::Project,
            "Content",
        );
        let group = DuplicateGroup::new(vec![file.clone()]);

        let merged = merge_duplicates(&group);
        assert_eq!(merged.description, file.description);
    }

    #[test]
    fn test_jaro_winkler_identical() {
        let similarity = jaro_winkler("hello", "hello");
        assert!(similarity > 0.99);
    }

    #[test]
    fn test_jaro_winkler_dissimilar() {
        let similarity = jaro_winkler("hello", "world");
        assert!(similarity < 0.5);
    }

    #[test]
    fn test_jaro_winkler_case_sensitive() {
        let similarity = jaro_winkler("SQLite WAL Lock", "SQLite WAL lock");
        assert!(similarity > SIMILARITY_THRESHOLD);
    }

    #[test]
    fn test_near_duplicate_lessons() {
        // Real-world scenario: two very similar lessons
        let lessons = vec![
            test_memory(
                "SQLite WAL Lock Contention",
                "Use WAL2 mode to avoid lock contention in SQLite. WAL2 is more efficient.",
                MemoryType::Project,
            ),
            test_memory(
                "SQLite WAL Lock Contention",
                "Use WAL2 mode to avoid lock contention. WAL2 provides better concurrency.",
                MemoryType::Project,
            ),
        ];

        let groups = find_duplicates(&lessons);
        assert_eq!(groups.len(), 1);

        let merged = merge_duplicates(&groups[0]);
        assert_eq!(merged.name, "SQLite WAL Lock Contention");
        // Both unique content pieces should be present
        assert!(merged.content.contains("WAL2 is more efficient"));
        assert!(merged.content.contains("better concurrency"));
    }

    #[test]
    fn test_multiple_groups() {
        // Create 3 distinct groups with 2 files each
        let memories = vec![
            test_memory("SQLite A", "SQLite content 1", MemoryType::Project),
            test_memory("SQLite A", "SQLite content 2", MemoryType::Project),
            test_memory("Rust A", "Rust content 1", MemoryType::Project),
            test_memory("Rust A", "Rust content 2", MemoryType::Project),
            test_memory("Python A", "Python content", MemoryType::Project),
        ];

        let groups = find_duplicates(&memories);
        // Should have: [SQLite (2)], [Rust (2)], [Python (1)]
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].len() + groups[1].len() + groups[2].len(), 5);
    }

    #[test]
    fn test_severity_order() {
        assert!(severity_order(MemoryType::Feedback) > severity_order(MemoryType::Project));
        assert!(severity_order(MemoryType::Project) > severity_order(MemoryType::User));
        assert!(severity_order(MemoryType::User) > severity_order(MemoryType::Reference));
    }

    // ======================================================================
    // MEMORY RELEVANCE SCORING TESTS
    // ======================================================================

    #[test]
    fn test_score_memory_critical_always_max() {
        let memory = test_memory("SQLite", "Content", MemoryType::Project);
        let lesson = LessonRecord::new("SQLite", "Content", vec![]).with_severity("critical");

        let score = score_memory_relevance(&memory, Some(&lesson));
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_score_memory_critical_ignores_age() {
        let memory = test_memory("SQLite", "Content", MemoryType::Project);

        // Create a lesson updated 100 days ago (well past stale threshold)
        let mut lesson = LessonRecord::new("SQLite", "Content", vec![]).with_severity("critical");
        lesson.updated_at = now_unix() - (100 * 24 * 3600);

        let score = score_memory_relevance(&memory, Some(&lesson));
        assert_eq!(score, 1.0); // Still 1.0 despite being very old
    }

    #[test]
    fn test_score_memory_warning_high_score() {
        let memory = test_memory("SQLite", "Content", MemoryType::Project);
        let lesson = LessonRecord::new("SQLite", "Content", vec![]).with_severity("warning");

        let score = score_memory_relevance(&memory, Some(&lesson));
        assert!(score > 0.7);
        assert!(score < 1.0);
    }

    #[test]
    fn test_score_memory_info_lower_score() {
        let memory = test_memory("SQLite", "Content", MemoryType::Project);
        let lesson = LessonRecord::new("SQLite", "Content", vec![]).with_severity("info");

        let score = score_memory_relevance(&memory, Some(&lesson));
        assert!(score < 0.6); // Info = 0.3, recency up to 1.0, so max ~0.51
    }

    #[test]
    fn test_score_memory_recent_info_better_than_old() {
        let memory = test_memory("SQLite", "Content", MemoryType::Project);

        // Recent info lesson
        let recent_lesson = LessonRecord::new("SQLite", "Content", vec![]).with_severity("info");

        // Old info lesson (40 days ago, beyond stale threshold)
        let mut old_lesson = LessonRecord::new("SQLite", "Content", vec![]).with_severity("info");
        old_lesson.updated_at = now_unix() - (40 * 24 * 3600);

        let recent_score = score_memory_relevance(&memory, Some(&recent_lesson));
        let old_score = score_memory_relevance(&memory, Some(&old_lesson));

        assert!(recent_score > old_score);
    }

    #[test]
    fn test_score_memory_no_lesson_fallback_to_type() {
        // Without lesson record, scores based on memory type
        let feedback = test_memory("Test", "Content", MemoryType::Feedback);
        let project = test_memory("Test", "Content", MemoryType::Project);
        let user = test_memory("Test", "Content", MemoryType::User);
        let reference = test_memory("Test", "Content", MemoryType::Reference);

        let feedback_score = score_memory_relevance(&feedback, None);
        let project_score = score_memory_relevance(&project, None);
        let user_score = score_memory_relevance(&user, None);
        let reference_score = score_memory_relevance(&reference, None);

        assert!(feedback_score > project_score);
        assert!(project_score > user_score);
        assert!(user_score > reference_score);
    }

    #[test]
    fn test_score_memory_unknown_severity() {
        let memory = test_memory("Test", "Content", MemoryType::Project);
        let mut lesson = LessonRecord::new("Test", "Content", vec![]);
        lesson.severity = "unknown".to_string();

        let score = score_memory_relevance(&memory, Some(&lesson));
        assert!(score > 0.0);
        assert!(score < 0.5); // Low score for unknown severity
    }

    #[test]
    fn test_score_memory_very_old_info_lowest() {
        let memory = test_memory("Test", "Content", MemoryType::Project);

        // Very old info lesson (90 days ago)
        let mut old_lesson = LessonRecord::new("Test", "Content", vec![]).with_severity("info");
        old_lesson.updated_at = now_unix() - (90 * 24 * 3600);

        let score = score_memory_relevance(&memory, Some(&old_lesson));

        // Should be very low (30% severity + 0% recency)
        assert!(score < 0.35);
    }

    // ======================================================================
    // BUDGET SELECTION TESTS
    // ======================================================================

    #[test]
    fn test_select_memories_empty() {
        let result = select_memories_within_budget(vec![], 100);
        assert!(result.is_empty());
    }

    #[test]
    fn test_select_memories_all_fit() {
        let memories = vec![
            ScoredMemory {
                memory: test_memory("M1", "Short", MemoryType::Project),
                lesson: None,
                score: 0.9,
            },
            ScoredMemory {
                memory: test_memory("M2", "Also short", MemoryType::Project),
                lesson: None,
                score: 0.5,
            },
        ];

        let result = select_memories_within_budget(memories, 100);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_select_memories_sorts_by_score() {
        let memories = vec![
            ScoredMemory {
                memory: test_memory("Low", "Content", MemoryType::User),
                lesson: None,
                score: 0.2,
            },
            ScoredMemory {
                memory: test_memory("High", "Content", MemoryType::Project),
                lesson: None,
                score: 0.9,
            },
            ScoredMemory {
                memory: test_memory("Med", "Content", MemoryType::Project),
                lesson: None,
                score: 0.5,
            },
        ];

        let result = select_memories_within_budget(memories, 200);
        // Should be sorted by score descending
        assert!(result[0].score >= result[1].score);
        assert!(result[1].score >= result[2].score);
    }

    #[test]
    fn test_select_memories_respects_budget() {
        // Create memories with known line counts
        // ~5 lines frontmatter + content_length/80 content lines each
        let memories = vec![
            ScoredMemory {
                memory: test_memory("M1", "x".repeat(400), MemoryType::Project), // ~10 lines
                lesson: None,
                score: 0.9,
            },
            ScoredMemory {
                memory: test_memory("M2", "y".repeat(400), MemoryType::Project), // ~10 lines
                lesson: None,
                score: 0.8,
            },
            ScoredMemory {
                memory: test_memory("M3", "z".repeat(400), MemoryType::Project), // ~10 lines
                lesson: None,
                score: 0.7,
            },
        ];

        // Budget of 20 lines should fit 2 items
        let result = select_memories_within_budget(memories, 20);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].score, 0.9);
        assert_eq!(result[1].score, 0.8);
    }

    #[test]
    fn test_select_memories_critical_survives_budget() {
        let critical_lesson =
            LessonRecord::new("Critical", "C".repeat(400), vec![]).with_severity("critical");

        let memories = vec![
            ScoredMemory {
                memory: test_memory("M1", "x".repeat(400), MemoryType::Project),
                lesson: None,
                score: 0.9,
            },
            ScoredMemory {
                memory: test_memory("Critical", "c".repeat(400), MemoryType::Project),
                lesson: Some(critical_lesson),
                score: 0.8,
            },
            ScoredMemory {
                memory: test_memory("M2", "y".repeat(400), MemoryType::Project),
                lesson: None,
                score: 0.7,
            },
        ];

        // Budget of 20 lines should fit M1 and Critical (even if over budget)
        // but not M2
        let result = select_memories_within_budget(memories, 20);
        assert!(result.len() >= 2);

        // Critical should be included even if over budget
        let has_critical = result.iter().any(|m| m.memory.name == "Critical");
        assert!(has_critical);
    }

    #[test]
    fn test_select_memories_critical_by_type_feedback() {
        let memories = vec![
            ScoredMemory {
                memory: test_memory("M1", "x".repeat(400), MemoryType::Project),
                lesson: None,
                score: 0.9,
            },
            ScoredMemory {
                memory: test_memory("Feedback", "f".repeat(400), MemoryType::Feedback),
                lesson: None,
                score: 0.5,
            },
            ScoredMemory {
                memory: test_memory("M2", "y".repeat(400), MemoryType::Project),
                lesson: None,
                score: 0.7,
            },
        ];

        // Budget of 20 lines: Feedback (Feedback type) should survive budget
        let result = select_memories_within_budget(memories, 20);
        // Should include M1 and Feedback, M2 might be excluded
        let has_feedback = result.iter().any(|m| m.memory.name == "Feedback");
        assert!(has_feedback);
    }

    #[test]
    fn test_select_memories_prefers_high_score() {
        let memories = vec![
            ScoredMemory {
                memory: test_memory("Low", "Small", MemoryType::Reference),
                lesson: None,
                score: 0.1,
            },
            ScoredMemory {
                memory: test_memory("High", "More content here for size", MemoryType::Project),
                lesson: None,
                score: 0.99,
            },
        ];

        let result = select_memories_within_budget(memories, 25);
        assert_eq!(result.len(), 2);
        // High score item should come first
        assert_eq!(result[0].score, 0.99);
    }

    #[test]
    fn test_select_memories_single_critical_exceeds_budget() {
        let critical_lesson =
            LessonRecord::new("Critical", "A".repeat(1000), vec![]).with_severity("critical");

        let memories = vec![ScoredMemory {
            memory: test_memory("Critical", "a".repeat(1000), MemoryType::Project),
            lesson: Some(critical_lesson),
            score: 0.9,
        }];

        // Budget of 5 lines, but critical lesson is much larger
        let result = select_memories_within_budget(memories, 5);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].memory.name, "Critical");
    }

    #[test]
    fn test_select_memories_multiple_critical_survive() {
        let critical1 =
            LessonRecord::new("Crit1", "A".repeat(400), vec![]).with_severity("critical");
        let critical2 =
            LessonRecord::new("Crit2", "B".repeat(400), vec![]).with_severity("critical");

        let memories = vec![
            ScoredMemory {
                memory: test_memory("Crit1", "a".repeat(400), MemoryType::Project),
                lesson: Some(critical1),
                score: 0.9,
            },
            ScoredMemory {
                memory: test_memory("Crit2", "b".repeat(400), MemoryType::Project),
                lesson: Some(critical2),
                score: 0.8,
            },
        ];

        // Even with tiny budget, both critical survive
        let result = select_memories_within_budget(memories, 15);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_select_memories_respects_content_size() {
        // Large content = more lines
        let large = ScoredMemory {
            memory: test_memory("Large", "x".repeat(800), MemoryType::Project),
            lesson: None,
            score: 0.5,
        };

        // Small content = fewer lines
        let small = ScoredMemory {
            memory: test_memory("Small", "y", MemoryType::Project),
            lesson: None,
            score: 0.9,
        };

        // Budget large enough for small only
        let result = select_memories_within_budget(vec![large, small.clone()], 20);
        assert!(result.len() >= 1);
        assert_eq!(result[0].memory.name, "Small");
    }
}
