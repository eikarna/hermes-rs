//! Token-budgeted renderer for the repo map.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::context::estimate_tokens;

use super::scorer::MinimalRepoMap;

/// Renders the repo map as `<repo_map>` XML-ish text constrained by a token
/// budget. Definitions are grouped by file; files are visited in rank order.
#[derive(Debug, Clone)]
pub struct RepoMapRenderer {
    /// Maximum number of tokens the rendered map may consume.
    pub max_tokens: usize,
}

impl Default for RepoMapRenderer {
    fn default() -> Self {
        Self { max_tokens: 1024 }
    }
}

impl RepoMapRenderer {
    pub fn new(max_tokens: usize) -> Self {
        Self { max_tokens }
    }

    /// Render `map` and truncate at the token budget.
    pub fn render(&self, map: &MinimalRepoMap) -> String {
        // Group ranked defs by file, preserving rank order of first appearance.
        let mut by_file: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
        let mut file_order: Vec<PathBuf> = Vec::new();
        for tag in map.ranked_definitions() {
            let entry = by_file.entry(tag.rel_path.clone());
            let path = tag.rel_path.clone();
            let kind = tag.symbol_kind.clone();
            let name = tag.name.clone();
            let line = tag.line;
            let v = entry.or_insert_with(|| {
                file_order.push(path);
                Vec::new()
            });
            v.push(format!("  {} {} (L{})", kind, name, line));
        }

        let mut out = String::from("<repo_map>\n");
        for path in file_order {
            let block_header = format!("{}:\n", path.display());
            let block_body = by_file[&path].join("\n");
            // rough token accounting per block plus a trailing newline
            let block = format!("{}{}\n", block_header, block_body);
            let block_tokens = estimate_tokens(&block) + 4;
            if estimate_tokens(&out) + block_tokens + "</repo_map>\n".len() / 4 > self.max_tokens {
                break;
            }
            out.push_str(&block);
        }
        out.push_str("</repo_map>\n");
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repomap::extractor::TagKind;
    use crate::repomap::RepoTag;

    fn tag(path: &str, name: &str, line: usize) -> RepoTag {
        RepoTag {
            rel_path: PathBuf::from(path),
            name: name.to_string(),
            kind: TagKind::Definition,
            symbol_kind: "function_item".to_string(),
            line,
        }
    }

    #[test]
    fn renders_grouped_by_rank() {
        let map = MinimalRepoMap {
            tags: vec![tag("b.rs", "beta", 5), tag("a.rs", "alpha", 1)],
            file_scores: vec![(PathBuf::from("a.rs"), 0.7), (PathBuf::from("b.rs"), 0.3)],
        };
        let renderer = RepoMapRenderer::new(2048);
        let rendered = renderer.render(&map);
        // a.rs (rank 0.7) must appear before b.rs (0.3)
        let pos_a = rendered.find("a.rs").unwrap();
        let pos_b = rendered.find("b.rs").unwrap();
        assert!(pos_a < pos_b);
        assert!(rendered.contains("<repo_map>"));
        assert!(rendered.contains("alpha"));
    }

    #[test]
    fn budget_truncates_rendering() {
        let mut tags = Vec::new();
        for i in 0..50 {
            tags.push(tag(&format!("file{i}.rs"), &format!("symbol_{i}"), 1));
        }
        let map = MinimalRepoMap {
            tags,
            file_scores: (0..50)
                .map(|i| (PathBuf::from(format!("file{i}.rs")), 0.0))
                .collect(),
        };
        let renderer = RepoMapRenderer::new(64);
        let rendered = renderer.render(&map);
        assert!(rendered.len() < 64 * 8); // roughly bounded
    }
}
