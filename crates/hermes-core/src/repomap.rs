//! Repository map generation for token-efficient code context.
//!
//! The repo map extracts identifiers with tree-sitter, ranks definition sites
//! with personalized PageRank, and renders the highest-value definitions until
//! the configured token budget is exhausted. It is intentionally read-only and
//! tolerant: malformed files or changed grammars are skipped instead of failing
//! an agent request.

pub mod budgeter;
pub mod extractor;
pub mod scorer;

pub use budgeter::RepoMapRenderer;
pub use extractor::{discover_source_files, extract_file_tags, Language, RepoTag, TagKind};
pub use scorer::{rank_and_render, MinimalRepoMap};
