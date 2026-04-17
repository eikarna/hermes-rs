//! Skills system for Hermes-RS
//!
//! Provides skill discovery, loading, and management matching
//! the Python hermes-agent's skills architecture. Skills are
//! directories containing a SKILL.md file with YAML front matter.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A loaded skill with parsed metadata and content.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Unique skill name (derived from directory name)
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Semantic version string
    pub version: String,
    /// The full SKILL.md content (body after front matter)
    pub content: String,
    /// Supported platforms (e.g. ["linux", "macos", "windows"])
    pub platforms: Vec<String>,
    /// Required environment variables
    pub prerequisites_env: Vec<String>,
    /// Required commands on PATH
    pub prerequisites_commands: Vec<String>,
    /// Supporting reference files: filename -> content
    pub references: HashMap<String, String>,
}

/// Manages skill discovery, loading, and lifecycle.
pub struct SkillManager {
    /// Root directory containing skill subdirectories
    pub skills_dir: PathBuf,
    /// Cache of loaded skills keyed by name
    skills: HashMap<String, Skill>,
}

impl SkillManager {
    /// Create a new SkillManager pointing at the given skills directory.
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills_dir,
            skills: HashMap::new(),
        }
    }

    /// Scan the skills directory and load all valid skills.
    ///
    /// Each subdirectory containing a `SKILL.md` file is treated as a skill.
    /// Skills with parse errors are logged and skipped.
    pub fn load_all(&mut self) -> Result<Vec<Skill>> {
        self.skills.clear();

        let entries = std::fs::read_dir(&self.skills_dir).map_err(|e| {
            Error::Config(format!(
                "Failed to read skills directory '{}': {}",
                self.skills_dir.display(),
                e
            ))
        })?;

        let mut loaded = Vec::new();

        for entry in entries {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let skill_file = path.join("SKILL.md");
            if !skill_file.exists() {
                continue;
            }

            match load_skill(&path) {
                Ok(skill) => {
                    self.skills.insert(skill.name.clone(), skill.clone());
                    loaded.push(skill);
                }
                Err(_) => {
                    // Skip skills that fail to parse
                    continue;
                }
            }
        }

        Ok(loaded)
    }

    /// Get a specific skill by name.
    ///
    /// Returns `None` if the skill hasn't been loaded or doesn't exist.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// List all loaded skills as `(name, description)` pairs.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut pairs: Vec<_> = self
            .skills
            .values()
            .map(|s| (s.name.clone(), s.description.clone()))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    }

    /// Check whether a skill is available on the current platform
    /// and all prerequisites are met.
    pub fn is_available(&self, skill: &Skill) -> bool {
        // Check platform
        if !skill.platforms.is_empty() {
            let current = current_platform();
            if !skill.platforms.iter().any(|p| p == current) {
                return false;
            }
        }

        // Check required environment variables
        for var in &skill.prerequisites_env {
            if std::env::var(var).is_err() {
                return false;
            }
        }

        // Check required commands
        for cmd in &skill.prerequisites_commands {
            if !command_exists(cmd) {
                return false;
            }
        }

        true
    }

    /// Create a new skill with the given name and SKILL.md content.
    ///
    /// Creates a subdirectory under `skills_dir` with a `SKILL.md` file.
    pub fn create(&mut self, name: &str, content: &str) -> Result<()> {
        let skill_dir = self.skills_dir.join(name);

        if skill_dir.exists() {
            return Err(Error::Config(format!("Skill '{}' already exists", name)));
        }

        std::fs::create_dir_all(&skill_dir).map_err(|e| {
            Error::Config(format!(
                "Failed to create skill directory '{}': {}",
                skill_dir.display(),
                e
            ))
        })?;

        let skill_file = skill_dir.join("SKILL.md");
        std::fs::write(&skill_file, content).map_err(|e| {
            Error::Config(format!("Failed to write SKILL.md for '{}': {}", name, e))
        })?;

        // Reload the newly created skill into cache
        if let Ok(skill) = load_skill(&skill_dir) {
            self.skills.insert(skill.name.clone(), skill);
        }

        Ok(())
    }

    /// Delete a skill by removing its directory.
    pub fn delete(&mut self, name: &str) -> Result<()> {
        let skill_dir = self.skills_dir.join(name);

        if !skill_dir.exists() {
            return Err(Error::Config(format!("Skill '{}' not found", name)));
        }

        std::fs::remove_dir_all(&skill_dir)
            .map_err(|e| Error::Config(format!("Failed to delete skill '{}': {}", name, e)))?;

        self.skills.remove(name);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Load a single skill from its directory.
fn load_skill(skill_dir: &Path) -> Result<Skill> {
    let dir_name = skill_dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Config("Invalid skill directory name".into()))?
        .to_string();

    let skill_file = skill_dir.join("SKILL.md");
    let raw = std::fs::read_to_string(&skill_file)
        .map_err(|e| Error::Config(format!("Failed to read SKILL.md in '{}': {}", dir_name, e)))?;

    let (front_matter, body) = parse_front_matter(&raw)?;

    let name = front_matter
        .get("name")
        .cloned()
        .unwrap_or_else(|| dir_name.clone());
    let description = front_matter.get("description").cloned().unwrap_or_default();
    let version = front_matter
        .get("version")
        .cloned()
        .unwrap_or_else(|| "0.1.0".into());
    let platforms = parse_list(front_matter.get("platforms"));
    let prerequisites_env = parse_list(front_matter.get("prerequisites_env"));
    let prerequisites_commands = parse_list(front_matter.get("prerequisites_commands"));

    // Load reference files (everything in the skill dir that isn't SKILL.md)
    let mut references = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(skill_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(fname) = path.file_name().and_then(|n| n.to_str()) {
                    if fname != "SKILL.md" {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            references.insert(fname.to_string(), content);
                        }
                    }
                }
            }
        }
    }

    Ok(Skill {
        name,
        description,
        version,
        content: body,
        platforms,
        prerequisites_env,
        prerequisites_commands,
        references,
    })
}

/// Parse YAML-like front matter delimited by `---` lines.
///
/// Returns a map of key-value pairs and the remaining body content.
/// Values that look like YAML lists (`[a, b, c]`) are stored as-is;
/// use `parse_list` to expand them.
fn parse_front_matter(raw: &str) -> Result<(HashMap<String, String>, String)> {
    let trimmed = raw.trim_start();

    if !trimmed.starts_with("---") {
        // No front matter — treat entire content as body, use defaults
        return Ok((HashMap::new(), raw.to_string()));
    }

    // Find the closing `---`
    let after_open = &trimmed[3..];
    let close_pos = after_open
        .find("\n---")
        .ok_or_else(|| Error::Config("SKILL.md front matter missing closing '---'".into()))?;

    let fm_block = &after_open[..close_pos];
    // Body starts after the closing `---` line
    let body_start = 3 + close_pos + 4; // "---" + "\n---"
    let body = if body_start < trimmed.len() {
        trimmed[body_start..].trim_start_matches('\n').to_string()
    } else {
        String::new()
    };

    let mut map = HashMap::new();

    for line in fm_block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_string();
            let value = line[colon_pos + 1..].trim().to_string();
            if !key.is_empty() {
                map.insert(key, value);
            }
        }
    }

    Ok((map, body))
}

/// Parse a YAML-like list value.
///
/// Supports both inline `[a, b, c]` and bare `a, b, c` formats.
/// Returns an empty vec for `None` or empty strings.
fn parse_list(value: Option<&String>) -> Vec<String> {
    let s = match value {
        Some(s) if !s.is_empty() => s,
        _ => return Vec::new(),
    };

    let s = s.trim();

    // Strip surrounding brackets if present
    let inner = if s.starts_with('[') && s.ends_with(']') {
        &s[1..s.len() - 1]
    } else {
        s
    };

    inner
        .split(',')
        .map(|item| item.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Return the current platform as a lowercase string matching common conventions.
fn current_platform() -> &'static str {
    if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else {
        "unknown"
    }
}

/// Check whether a command is available on the system PATH.
fn command_exists(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("where")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("which")
            .arg(cmd)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use std::sync::atomic::{AtomicU64, Ordering};
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn make_temp_dir() -> PathBuf {
        let count = TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "hermes_skills_test_{}_{}",
            std::process::id(),
            count
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn cleanup(dir: &Path) {
        let _ = fs::remove_dir_all(dir);
    }

    fn sample_skill_md() -> &'static str {
        "---\n\
         name: test-skill\n\
         description: A test skill for unit tests\n\
         version: 1.0.0\n\
         platforms: [linux, macos, windows]\n\
         prerequisites_env: [HOME]\n\
         prerequisites_commands: []\n\
         ---\n\
         # Test Skill\n\
         \n\
         This is the skill content.\n"
    }

    #[test]
    fn test_parse_front_matter() {
        let (fm, body) = parse_front_matter(sample_skill_md()).unwrap();
        assert_eq!(fm.get("name").unwrap(), "test-skill");
        assert_eq!(
            fm.get("description").unwrap(),
            "A test skill for unit tests"
        );
        assert_eq!(fm.get("version").unwrap(), "1.0.0");
        assert!(body.contains("# Test Skill"));
        assert!(body.contains("This is the skill content."));
    }

    #[test]
    fn test_parse_front_matter_no_front_matter() {
        let (fm, body) = parse_front_matter("# Just content\nNo front matter here").unwrap();
        assert!(fm.is_empty());
        assert!(body.contains("# Just content"));
    }

    #[test]
    fn test_parse_list_inline() {
        let val = "[linux, macos, windows]".to_string();
        let result = parse_list(Some(&val));
        assert_eq!(result, vec!["linux", "macos", "windows"]);
    }

    #[test]
    fn test_parse_list_bare() {
        let val = "linux, macos".to_string();
        let result = parse_list(Some(&val));
        assert_eq!(result, vec!["linux", "macos"]);
    }

    #[test]
    fn test_parse_list_empty() {
        assert!(parse_list(None).is_empty());
        let val = "[]".to_string();
        assert!(parse_list(Some(&val)).is_empty());
    }

    #[test]
    fn test_load_skill() {
        let tmp = make_temp_dir();
        let skill_dir = tmp.join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), sample_skill_md()).unwrap();
        fs::write(skill_dir.join("helper.py"), "print('hello')").unwrap();

        let skill = load_skill(&skill_dir).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.version, "1.0.0");
        assert_eq!(skill.platforms, vec!["linux", "macos", "windows"]);
        assert!(skill.content.contains("# Test Skill"));
        assert_eq!(skill.references.get("helper.py").unwrap(), "print('hello')");

        cleanup(&tmp);
    }

    #[test]
    fn test_skill_manager_load_all() {
        let tmp = make_temp_dir();

        // Create two skills
        let s1 = tmp.join("skill-a");
        fs::create_dir_all(&s1).unwrap();
        fs::write(
            s1.join("SKILL.md"),
            "---\nname: skill-a\ndescription: First\nversion: 0.1.0\n---\nContent A\n",
        )
        .unwrap();

        let s2 = tmp.join("skill-b");
        fs::create_dir_all(&s2).unwrap();
        fs::write(
            s2.join("SKILL.md"),
            "---\nname: skill-b\ndescription: Second\nversion: 0.2.0\n---\nContent B\n",
        )
        .unwrap();

        // Create a non-skill directory (no SKILL.md)
        let s3 = tmp.join("not-a-skill");
        fs::create_dir_all(&s3).unwrap();
        fs::write(s3.join("README.md"), "nothing").unwrap();

        let mut mgr = SkillManager::new(tmp.clone());
        let loaded = mgr.load_all().unwrap();
        assert_eq!(loaded.len(), 2);

        let list = mgr.list();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|(n, _)| n == "skill-a"));
        assert!(list.iter().any(|(n, _)| n == "skill-b"));

        assert!(mgr.get("skill-a").is_some());
        assert!(mgr.get("nonexistent").is_none());

        cleanup(&tmp);
    }

    #[test]
    fn test_skill_manager_create_and_delete() {
        let tmp = make_temp_dir();
        let mut mgr = SkillManager::new(tmp.clone());

        let content =
            "---\nname: new-skill\ndescription: Created skill\nversion: 1.0.0\n---\n# New\n";
        mgr.create("new-skill", content).unwrap();

        assert!(tmp.join("new-skill").join("SKILL.md").exists());
        assert!(mgr.get("new-skill").is_some());
        assert_eq!(mgr.get("new-skill").unwrap().description, "Created skill");

        // Creating duplicate should fail
        assert!(mgr.create("new-skill", content).is_err());

        // Delete
        mgr.delete("new-skill").unwrap();
        assert!(!tmp.join("new-skill").exists());
        assert!(mgr.get("new-skill").is_none());

        // Deleting non-existent should fail
        assert!(mgr.delete("new-skill").is_err());

        cleanup(&tmp);
    }

    #[test]
    fn test_is_available_platform_match() {
        let skill = Skill {
            name: "test".into(),
            description: "test".into(),
            version: "1.0.0".into(),
            content: String::new(),
            platforms: vec![current_platform().to_string()],
            prerequisites_env: vec![],
            prerequisites_commands: vec![],
            references: HashMap::new(),
        };

        let mgr = SkillManager::new(PathBuf::from("."));
        assert!(mgr.is_available(&skill));
    }

    #[test]
    fn test_is_available_platform_mismatch() {
        let skill = Skill {
            name: "test".into(),
            description: "test".into(),
            version: "1.0.0".into(),
            content: String::new(),
            platforms: vec!["plan9".to_string()],
            prerequisites_env: vec![],
            prerequisites_commands: vec![],
            references: HashMap::new(),
        };

        let mgr = SkillManager::new(PathBuf::from("."));
        assert!(!mgr.is_available(&skill));
    }

    #[test]
    fn test_is_available_empty_platforms_means_all() {
        let skill = Skill {
            name: "test".into(),
            description: "test".into(),
            version: "1.0.0".into(),
            content: String::new(),
            platforms: vec![],
            prerequisites_env: vec![],
            prerequisites_commands: vec![],
            references: HashMap::new(),
        };

        let mgr = SkillManager::new(PathBuf::from("."));
        assert!(mgr.is_available(&skill));
    }

    #[test]
    fn test_is_available_missing_env() {
        let skill = Skill {
            name: "test".into(),
            description: "test".into(),
            version: "1.0.0".into(),
            content: String::new(),
            platforms: vec![],
            prerequisites_env: vec!["HERMES_NONEXISTENT_VAR_12345".into()],
            prerequisites_commands: vec![],
            references: HashMap::new(),
        };

        let mgr = SkillManager::new(PathBuf::from("."));
        assert!(!mgr.is_available(&skill));
    }

    #[test]
    fn test_load_skill_defaults() {
        let tmp = make_temp_dir();
        let skill_dir = tmp.join("minimal");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "# Just content, no front matter\n",
        )
        .unwrap();

        let skill = load_skill(&skill_dir).unwrap();
        // Name falls back to directory name
        assert_eq!(skill.name, "minimal");
        assert_eq!(skill.version, "0.1.0");
        assert!(skill.platforms.is_empty());
        assert!(skill.content.contains("# Just content"));

        cleanup(&tmp);
    }
}
