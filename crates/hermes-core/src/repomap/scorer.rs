//! Personalized PageRank ranking over file-level symbol reference graph.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::extractor::{discover_source_files, extract_file_tags, RepoTag, TagKind};

const DAMPING: f32 = 0.85;
const ITERATIONS: usize = 20;

/// A ranked repo map: tag set plus per-file importance scores.
#[derive(Debug, Clone)]
pub struct MinimalRepoMap {
    /// All extracted tags (defs and refs), unsorted.
    pub tags: Vec<RepoTag>,
    /// Per-file PageRank score, normalized so scores sum to ~1.0.
    pub file_scores: Vec<(PathBuf, f32)>,
}

impl MinimalRepoMap {
    /// Definition tags sorted by file score (desc) then file path.
    pub fn ranked_definitions(&self) -> Vec<&RepoTag> {
        let score: HashMap<&Path, f32> = self
            .file_scores
            .iter()
            .map(|(p, s)| (p.as_path(), *s))
            .collect();
        let mut defs: Vec<&RepoTag> = self
            .tags
            .iter()
            .filter(|t| t.kind == TagKind::Definition)
            .collect();
        defs.sort_by(|a, b| {
            let sa = score.get(a.rel_path.as_path()).copied().unwrap_or(0.0);
            let sb = score.get(b.rel_path.as_path()).copied().unwrap_or(0.0);
            sb.partial_cmp(&sa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.rel_path.cmp(&b.rel_path))
                .then_with(|| a.line.cmp(&b.line))
        });
        defs
    }
}

/// Build a repo map for `root`.
///
/// `chat_files` (relative paths) seed personalization — files the user is
/// actively editing receive higher rank so their neighbors promote.
/// Tolerant: unreadable or unparseable files are skipped.
pub fn rank_and_render(root: &Path, chat_files: &[PathBuf]) -> MinimalRepoMap {
    let files = discover_source_files(root);
    build_map(root, &files, chat_files)
}

/// Build a repo map from an explicit file list (useful in tests and for
/// incremental updates).
pub fn build_map(root: &Path, files: &[PathBuf], chat_files: &[PathBuf]) -> MinimalRepoMap {
    let mut tags = Vec::new();
    for f in files {
        tags.extend(extract_file_tags(root, f));
    }
    let file_scores = score_files(&tags, chat_files);
    MinimalRepoMap { tags, file_scores }
}

/// Personalized PageRank over the bipartite file↔name graph, collapsed to
/// file-level edges: an edge fileA→fileB exists when fileA references a name
/// that fileB defines.
fn score_files(tags: &[RepoTag], chat_files: &[PathBuf]) -> Vec<(PathBuf, f32)> {
    // name → files defining it
    let mut definitions: HashMap<&str, HashSet<&Path>> = HashMap::new();
    // file → names referenced
    let mut references: HashMap<&Path, HashSet<&str>> = HashMap::new();
    // ensure every file appears in score output
    let mut files: HashSet<&Path> = HashSet::new();

    for tag in tags {
        files.insert(tag.rel_path.as_path());
        match tag.kind {
            TagKind::Definition => {
                definitions
                    .entry(tag.name.as_str())
                    .or_default()
                    .insert(tag.rel_path.as_path());
            }
            TagKind::Reference => {
                references
                    .entry(tag.rel_path.as_path())
                    .or_default()
                    .insert(tag.name.as_str());
            }
        }
    }

    // file → outgoing neighbors with weights
    let mut edges: HashMap<&Path, HashMap<&Path, f32>> = HashMap::new();
    for (src, names) in &references {
        // count refs per source for normalization
        let mut defs_count = 0usize;
        for name in names {
            if let Some(def_files) = definitions.get(name) {
                defs_count += def_files.len();
            }
        }
        if defs_count == 0 {
            continue;
        }
        let out = edges.entry(*src).or_default();
        for name in names {
            if let Some(def_files) = definitions.get(name) {
                for dst in def_files {
                    if dst == src {
                        continue; // skip self-loops
                    }
                    *out.entry(*dst).or_insert(0.0) += 1.0 / defs_count as f32;
                }
            }
        }
    }

    // personalization vector
    let chat_set: HashSet<&Path> = chat_files.iter().map(|p| p.as_path()).collect();
    let n = files.len().max(1) as f32;
    let personalization = |path: &Path| -> f32 {
        if chat_set.contains(path) {
            4.0 / n // boost chat-edited files
        } else {
            1.0 / n
        }
    };

    // power iteration
    let mut scores: HashMap<&Path, f32> = files.iter().map(|p| (*p, 1.0 / n)).collect();
    for _ in 0..ITERATIONS {
        let mut next: HashMap<&Path, f32> = files
            .iter()
            .map(|p| (*p, (1.0 - DAMPING) * personalization(p)))
            .collect();
        for (src, score) in &scores {
            if let Some(out) = edges.get(src) {
                let total: f32 = out.values().sum();
                if total > 0.0 {
                    for (dst, weight) in out {
                        *next.entry(*dst).or_insert(0.0) += DAMPING * score * weight / total;
                    }
                } else {
                    // dangling node: spread uniformly
                    let share = DAMPING * score / n;
                    for file in &files {
                        *next.entry(*file).or_insert(0.0) += share;
                    }
                }
            } else {
                let share = DAMPING * score / n;
                for file in &files {
                    *next.entry(*file).or_insert(0.0) += share;
                }
            }
        }
        scores = next;
    }

    let mut ranked: Vec<(PathBuf, f32)> = scores
        .into_iter()
        .map(|(p, s)| (p.to_path_buf(), s))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    ranked
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn ranks_definer_above_isolated() {
        let dir = std::env::temp_dir().join(format!("hermes_repomap_score_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // a.rs defines `helper`; b.rs references it; c.rs is isolated noise.
        let a = write_file(&dir, "a.rs", "pub fn helper() {}\n");
        let b = write_file(&dir, "b.rs", "fn use_it() { helper(); }\n");
        let c = write_file(&dir, "c.rs", "fn isolated() {}\n");
        let files = vec![a.clone(), b.clone(), c.clone()];
        let map = build_map(&dir, &files, &[]);
        let score_of = |path: &PathBuf| -> f32 {
            let rel = path.strip_prefix(&dir).unwrap().to_path_buf();
            map.file_scores
                .iter()
                .find(|(p, _)| p == &rel)
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };
        assert!(score_of(&a) > score_of(&c));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn personalization_boosts_chat_files() {
        let dir = std::env::temp_dir().join(format!("hermes_repomap_pers_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = write_file(&dir, "a.rs", "fn alpha() {}\n");
        let b = write_file(&dir, "b.rs", "fn beta() {}\n");
        let files = vec![a.clone(), b.clone()];
        let chat = vec![PathBuf::from("b.rs")];
        let map = build_map(&dir, &files, &chat);
        let score_of = |name: &str| -> f32 {
            map.file_scores
                .iter()
                .find(|(p, _)| p == &PathBuf::from(name))
                .map(|(_, s)| *s)
                .unwrap_or(0.0)
        };
        assert!(score_of("b.rs") > score_of("a.rs"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
