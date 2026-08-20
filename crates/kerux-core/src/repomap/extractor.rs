//! Tree-sitter symbol extraction: definition/reference tags per source file.

use std::path::{Path, PathBuf};

use tree_sitter::Parser;

const SOURCE_EXTENSIONS: &[&str] = &["rs", "py", "c", "h", "ts", "tsx", "js", "jsx", "mjs", "cjs"];
const IGNORED_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "dist",
    "build",
    ".next",
    "__pycache__",
    ".venv",
    "venv",
];

/// Supported language for tag extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    C,
    Python,
    Rust,
    TypeScript,
    JavaScript,
}

impl Language {
    /// Infer language from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "rs" => Some(Self::Rust),
            "py" => Some(Self::Python),
            "c" | "h" => Some(Self::C),
            "ts" | "tsx" => Some(Self::TypeScript),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            _ => None,
        }
    }

    fn grammar(&self) -> Option<tree_sitter_language::LanguageFn> {
        // Grammar availability depends on grammar crate versions compatible with tree-sitter core.
        match self {
            Self::Rust => Some(tree_sitter_rust::LANGUAGE),
            Self::Python => Some(tree_sitter_python::LANGUAGE),
            Self::C => Some(tree_sitter_c::LANGUAGE),
            Self::TypeScript => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT),
            Self::JavaScript => None, // JavaScript grammar not yet a dependency
        }
    }
}

/// Kind of a tag discovered in a source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagKind {
    Definition,
    Reference,
}

/// A named symbol occurrence in a file.
#[derive(Debug, Clone)]
pub struct RepoTag {
    pub rel_path: PathBuf,
    pub name: String,
    pub kind: TagKind,
    /// Symbol kind (e.g. "function", "class", "struct") for rendering.
    pub symbol_kind: String,
    /// 1-based line number.
    pub line: usize,
}

/// Recursively collect source files under `root`, skipping build/dependency directories.
pub fn discover_source_files(root: &Path) -> Vec<PathBuf> {
    discover_source_files_with_limit(root, 500)
}

/// Bounded variant of [`discover_source_files`]; stops after `max_files` to keep
/// scan time predictable in very large trees.
pub fn discover_source_files_with_limit(root: &Path, max_files: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if out.len() >= max_files {
                return out;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') && name != ".github" {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if !IGNORED_DIRS.contains(&name.as_ref()) {
                    stack.push(path);
                }
            } else if file_type.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if SOURCE_EXTENSIONS.contains(&ext) {
                        out.push(path);
                    }
                }
            }
        }
    }
    out.sort();
    out
}

/// Which identifier node kinds represent definitions vs references, per language.
fn definition_spec(lang: Language) -> (&'static [&'static str], &'static [&'static str]) {
    match lang {
        Language::Rust => (
            &[
                "function_item",
                "struct_item",
                "enum_item",
                "trait_item",
                "impl_item",
                "mod_item",
                "type_item",
                "const_item",
                "static_item",
                "macro_definition",
            ],
            &["identifier"],
        ),
        Language::Python => (
            &["function_definition", "class_definition"],
            &["identifier"],
        ),
        Language::C => (
            &[
                "function_definition",
                "struct_specifier",
                "enum_specifier",
                "type_definition",
                "preproc_def",
            ],
            &["identifier"],
        ),
        Language::TypeScript => (
            &[
                "function_declaration",
                "function_definition",
                "class_declaration",
                "method_definition",
                "interface_declaration",
                "type_alias_declaration",
                "enum_declaration",
                "variable_declarator",
            ],
            &["identifier", "type_identifier"],
        ),
        Language::JavaScript => (&[], &[]),
    }
}

/// Extract definition and reference tags from a single source file.
///
/// Tolerant by design: unsupported extensions, unreadable files, parse
/// failures, or missing grammars return an empty result.
pub fn extract_file_tags(root: &Path, path: &Path) -> Vec<RepoTag> {
    let ext = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => ext,
        None => return Vec::new(),
    };
    let lang = match Language::from_extension(ext) {
        Some(lang) => lang,
        None => return Vec::new(),
    };
    let grammar = match lang.grammar() {
        Some(g) => g,
        None => return Vec::new(),
    };
    let source = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return Vec::new(),
    };
    let mut parser = Parser::new();
    if parser.set_language(&grammar.into()).is_err() {
        return Vec::new();
    }
    let tree = match parser.parse(&source, None) {
        Some(tree) => tree,
        None => return Vec::new(),
    };
    let rel_path = path
        .strip_prefix(root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.to_path_buf());

    let (def_kinds, _ref_kinds) = definition_spec(lang);
    let mut tags = Vec::new();
    let text: &[u8] = &source;

    // Root walk: record definition nodes and identifier references.
    let root_node = tree.root_node();
    let mut stack = vec![root_node];
    while let Some(node) = stack.pop() {
        let kind = node.kind();
        if def_kinds.contains(&kind) {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Ok(name) = name_node.utf8_text(text) {
                    tags.push(RepoTag {
                        rel_path: rel_path.clone(),
                        name: name.to_string(),
                        kind: TagKind::Definition,
                        symbol_kind: kind.to_string(),
                        line: name_node.start_position().row + 1,
                    });
                }
            } else if kind == "impl_item" {
                // impl blocks: use the type name as the tag
                if let Some(type_node) = node.child_by_field_name("type") {
                    if let Ok(name) = type_node.utf8_text(text) {
                        tags.push(RepoTag {
                            rel_path: rel_path.clone(),
                            name: name.to_string(),
                            kind: TagKind::Definition,
                            symbol_kind: kind.to_string(),
                            line: type_node.start_position().row + 1,
                        });
                    }
                }
            }
        } else if kind == "identifier" {
            if let Ok(name) = node.utf8_text(text) {
                if !name.is_empty() && !is_common_keyword(name) {
                    tags.push(RepoTag {
                        rel_path: rel_path.clone(),
                        name: name.to_string(),
                        kind: TagKind::Reference,
                        symbol_kind: String::new(),
                        line: node.start_position().row + 1,
                    });
                }
            }
        }
        // push children in reverse source order so traversal stays top-down
        let mut cursor = node.walk();
        let children: Vec<_> = node.children(&mut cursor).collect();
        stack.extend(children.into_iter().rev());
    }
    tags
}

fn is_common_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "else"
            | "for"
            | "while"
            | "loop"
            | "match"
            | "return"
            | "fn"
            | "let"
            | "var"
            | "const"
            | "static"
            | "use"
            | "mod"
            | "pub"
            | "struct"
            | "enum"
            | "trait"
            | "impl"
            | "def"
            | "class"
            | "import"
            | "from"
            | "function"
            | "interface"
            | "type"
            | "new"
            | "self"
            | "this"
            | "true"
            | "false"
            | "None"
            | "null"
            | "True"
            | "False"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn extracts_rust_function_definition() {
        let dir = std::env::temp_dir().join(format!("kerux_repomap_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = write_temp(&dir, "lib.rs", "pub fn hello() {}\n");
        let tags = extract_file_tags(&dir, &file);
        let defs: Vec<_> = tags
            .iter()
            .filter(|t| t.kind == TagKind::Definition)
            .collect();
        assert!(defs.iter().any(|t| t.name == "hello"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discovers_and_skips_target_dir() {
        let dir = std::env::temp_dir().join(format!("kerux_repomap_disc_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        write_temp(&dir.join("src"), "a.rs", "fn a() {}\n");
        write_temp(&dir.join("target"), "b.rs", "fn b() {}\n");
        let files = discover_source_files(&dir);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.rs"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn extracts_python_def() {
        let dir = std::env::temp_dir().join(format!("kerux_repomap_py_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = write_temp(&dir, "mod.py", "def greet():\n    pass\n");
        let tags = extract_file_tags(&dir, &file);
        assert!(tags
            .iter()
            .any(|t| t.name == "greet" && t.kind == TagKind::Definition));
        std::fs::remove_dir_all(&dir).ok();
    }
}
