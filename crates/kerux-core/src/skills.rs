//! Skills system for Kerux
//!
//! Provides skill discovery, loading, and management matching
//! Skills are

//! directories containing a SKILL.md file with YAML front matter.

use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Directory name (inside the skills root) holding archived skills.
pub const ARCHIVE_DIR_NAME: &str = "_archive";

/// Directory name (inside the skills root) holding distilled draft skills
/// awaiting human approval before they become loadable.
pub const PENDING_DIR_NAME: &str = "_pending";

/// Provenance of a skill — who authored it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillOrigin {
    /// Installed by the user or hand-authored.
    #[default]
    User,
    /// Created by the agent (e.g. curator distillation).
    Agent,
}

impl SkillOrigin {
    fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }

    fn parse(s: &str) -> Self {
        if s.trim().eq_ignore_ascii_case("agent") {
            Self::Agent
        } else {
            Self::User
        }
    }
}

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
    /// Who authored the skill; curator auto-archiving only touches agent-made skills.
    pub origin: SkillOrigin,
    /// Pinned skills are exempt from curator review/archival.
    pub pinned: bool,
    /// Times this skill was invoked (surfaced via `record_use`).
    pub use_count: u64,
    /// Unix seconds of last invocation; `None` = never used.
    pub last_activity_at: Option<i64>,
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
            // Archived (`_archive/`) and pending-approval (`_pending/`) skills
            // are never auto-loaded.
            if entry.file_name() == ARCHIVE_DIR_NAME || entry.file_name() == PENDING_DIR_NAME {
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

    /// Archive a skill by moving its directory under `<skills_dir>/_archive/`.
    /// Idempotent: archiving an already-archived or missing skill is a no-op
    /// returning `Ok(false)`/`Ok(true)` respectively.
    pub fn archive(&mut self, name: &str) -> Result<bool> {
        let skill_dir = self.skills_dir.join(name);
        if !skill_dir.exists() {
            return Ok(false);
        }
        let archive_root = self.skills_dir.join(ARCHIVE_DIR_NAME);
        std::fs::create_dir_all(&archive_root).map_err(|e| {
            Error::Config(format!(
                "Failed to create archive dir '{}': {}",
                archive_root.display(),
                e
            ))
        })?;
        let target = archive_root.join(name);
        if target.exists() {
            std::fs::remove_dir_all(&target).map_err(|e| {
                Error::Config(format!(
                    "Failed to replace archived skill '{}': {}",
                    target.display(),
                    e
                ))
            })?;
        }
        std::fs::rename(&skill_dir, &target).map_err(|e| {
            Error::Config(format!(
                "Failed to archive skill '{}' -> '{}': {}",
                skill_dir.display(),
                target.display(),
                e
            ))
        })?;
        self.skills.remove(name);
        Ok(true)
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

    /// Create a skill under `_pending/` (awaiting approval; not loadable).
    pub fn create_pending(&mut self, name: &str, content: &str) -> Result<()> {
        let pending_root = self.skills_dir.join(PENDING_DIR_NAME);
        std::fs::create_dir_all(&pending_root).map_err(|e| {
            Error::Config(format!(
                "Failed to create pending dir '{}': {}",
                pending_root.display(),
                e
            ))
        })?;
        let skill_dir = pending_root.join(name);
        if skill_dir.exists() || self.skills_dir.join(name).exists() {
            return Ok(()); // already pending or live; don't overwrite
        }
        std::fs::create_dir_all(&skill_dir).map_err(|e| {
            Error::Config(format!(
                "Failed to create pending skill dir '{}': {}",
                skill_dir.display(),
                e
            ))
        })?;
        std::fs::write(skill_dir.join("SKILL.md"), content).map_err(|e| {
            Error::Config(format!(
                "Failed to write pending SKILL.md for '{}': {}",
                name, e
            ))
        })
    }

    /// List skills awaiting approval in `_pending/` (parsed, for display).
    pub fn pending_skills(&self) -> Vec<Skill> {
        let pending_root = self.skills_dir.join(PENDING_DIR_NAME);
        std::fs::read_dir(&pending_root)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.join("SKILL.md").exists())
                    .filter_map(|p| load_skill(&p).ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List skills awaiting approval in `_pending/`.
    pub fn list_pending(&self) -> Vec<String> {
        let pending_root = self.skills_dir.join(PENDING_DIR_NAME);
        let mut names: Vec<String> = std::fs::read_dir(&pending_root)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().join("SKILL.md").exists())
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Move a pending skill into the loadable set. Returns `false` when no
    /// such pending skill exists (or it is already live).
    pub fn approve(&mut self, name: &str) -> Result<bool> {
        let pending_dir = self.skills_dir.join(PENDING_DIR_NAME).join(name);
        if !pending_dir.exists() || self.skills_dir.join(name).exists() {
            return Ok(false);
        }
        std::fs::rename(&pending_dir, self.skills_dir.join(name))
            .map_err(|e| Error::Config(format!("Failed to approve skill '{}': {}", name, e)))?;
        if let Ok(skill) = load_skill(&self.skills_dir.join(name)) {
            self.skills.insert(skill.name.clone(), skill);
        }
        Ok(true)
    }

    /// Delete a pending skill (pre-approval discard). Returns `false` when no
    /// such pending skill exists.
    pub fn discard_pending(&mut self, name: &str) -> Result<bool> {
        let pending_dir = self.skills_dir.join(PENDING_DIR_NAME).join(name);
        if !pending_dir.exists() {
            return Ok(false);
        }
        std::fs::remove_dir_all(&pending_dir).map_err(|e| {
            Error::Config(format!("Failed to discard pending skill '{}': {}", name, e))
        })?;
        Ok(true)
    }

    /// Record an invocation of this skill: increment use_count, set last_activity_at.
    pub fn record_use(&mut self, name: &str) -> bool {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.use_count += 1;
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|d| i64::try_from(d.as_secs()).ok());
            skill.last_activity_at = now;
            // Persist metadata (best effort; non-fatal).
            let _ = write_skill_metadata(&self.skills_dir.join(name), skill);
            true
        } else {
            false
        }
    }

    /// Update provenance fields for existing skill.
    pub fn update_metadata(&mut self, name: &str, origin: SkillOrigin, pinned: bool) -> bool {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.origin = origin;
            skill.pinned = pinned;
            let _ = write_skill_metadata(&self.skills_dir.join(name), skill);
            true
        } else {
            false
        }
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
    let origin = front_matter
        .get("created_by")
        .map(|s| SkillOrigin::parse(s))
        .unwrap_or_default();
    let pinned = front_matter
        .get("pinned")
        .map(|v| v.trim().eq_ignore_ascii_case("true") || v.trim() == "1")
        .unwrap_or(false);
    let use_count = front_matter
        .get("use_count")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let last_activity_at = front_matter
        .get("last_activity_at")
        .and_then(|v| v.trim().parse::<i64>().ok());

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
        origin,
        pinned,
        use_count,
        last_activity_at,
    })
}

/// Write lifecycle metadata (`origin`, `pinned`, `use_count`,
/// `last_activity_at`) back into a skill's SKILL.md front matter, preserving
/// the body and any unknown keys. Creating the file when absent.
pub fn write_skill_metadata(skill_dir: &Path, skill: &Skill) -> Result<()> {
    let skill_file = skill_dir.join("SKILL.md");
    let raw = std::fs::read_to_string(&skill_file).unwrap_or_default();
    let (front_matter, body) = parse_front_matter(&raw).unwrap_or((HashMap::new(), raw.clone()));

    let mut fm = front_matter;
    fm.insert("name".to_string(), skill.name.clone());
    fm.insert("description".to_string(), skill.description.clone());
    fm.insert("version".to_string(), skill.version.clone());
    fm.insert("created_by".to_string(), skill.origin.as_str().to_string());
    if skill.pinned {
        fm.insert("pinned".to_string(), "true".to_string());
    } else {
        fm.remove("pinned");
    }
    if skill.use_count > 0 {
        fm.insert("use_count".to_string(), skill.use_count.to_string());
    } else {
        fm.remove("use_count");
    }
    match skill.last_activity_at {
        Some(ts) => {
            fm.insert("last_activity_at".to_string(), ts.to_string());
        }
        None => {
            fm.remove("last_activity_at");
        }
    }

    // Stable key order for readable diffs.
    let order = [
        "name",
        "description",
        "version",
        "created_by",
        "pinned",
        "use_count",
        "last_activity_at",
        "platforms",
        "prerequisites_env",
        "prerequisites_commands",
    ];
    let mut keys: Vec<&String> = fm.keys().collect();
    keys.sort_by_key(|k| {
        order
            .iter()
            .position(|o| *o == k.as_str())
            .unwrap_or(order.len())
    });

    let mut out = String::from("---\n");
    for key in keys {
        out.push_str(&format!("{}: {}\n", key, fm[key]));
    }
    out.push_str("---\n");
    out.push_str(body.trim_start_matches('\n'));
    std::fs::write(&skill_file, out)
        .map_err(|e| Error::Config(format!("Failed to write SKILL.md: {}", e)))
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
///
/// Implemented as a direct filesystem scan instead of shelling out to
/// `which`/`where`: spawning a process per lookup is orders of magnitude
/// slower, and a missing/hosed shell turns every skill refresh into an
/// error path (the TUI calls this on its render thread).
fn command_exists(cmd: &str) -> bool {
    use std::path::Path;

    // Absolute or relative paths are checked as-is, mirroring how exec
    // would resolve them.
    if cmd.contains('/') || cmd.contains('\\') {
        return is_executable_file(Path::new(cmd));
    }

    let path_var = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };

    #[cfg(target_os = "windows")]
    let candidates: Vec<String> = {
        // Windows resolves bare names against PATHEXT (.exe, .bat, ...);
        // a name that already carries an extension is tried verbatim.
        let exts: Vec<String> = std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(|e| e.to_ascii_lowercase())
            .collect();
        let has_ext = Path::new(cmd)
            .extension()
            .map(|e| {
                let e = format!(".{}", e.to_string_lossy().to_ascii_lowercase());
                exts.iter().any(|x| x == &e)
            })
            .unwrap_or(false);
        if has_ext {
            vec![cmd.to_string()]
        } else {
            exts.iter().map(|e| format!("{}{}", cmd, e)).collect()
        }
    };
    #[cfg(not(target_os = "windows"))]
    let candidates: Vec<String> = vec![cmd.to_string()];

    for dir in std::env::split_paths(&path_var) {
        for cand in &candidates {
            if is_executable_file(&dir.join(cand)) {
                return true;
            }
        }
    }
    false
}

/// True when `path` is an existing regular file with an execute bit set
/// (Unix). On Windows mere existence is enough — execute permission is
/// not a filesystem property there.
fn is_executable_file(path: &std::path::Path) -> bool {
    #[cfg(not(target_os = "windows"))]
    use std::os::unix::fs::PermissionsExt;

    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        true
    }
    #[cfg(not(target_os = "windows"))]
    {
        meta.permissions().mode() & 0o111 != 0
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
            "kerux_skills_test_{}_{}",
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

    #[test]
    fn command_exists_finds_real_binary() {
        // `sh` is guaranteed present on every CI/dev box we target
        // (Git Bash on Windows provides it too).
        assert!(command_exists("sh"));
    }

    #[test]
    fn command_exists_rejects_missing_and_non_executable() {
        assert!(!command_exists("definitely-not-a-real-binary-kerux"));

        // Absolute-path branch: existing file WITHOUT exec bit must not
        // count as an available command.
        let dir = make_temp_dir();
        let noexec = dir.join("noexec-binary");
        fs::write(&noexec, b"#!/bin/sh\n").unwrap();
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&noexec, fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!command_exists(noexec.to_str().unwrap()));
        }

        // ...and WITH the exec bit it must count, even though the file
        // lives outside PATH.
        #[cfg(not(target_os = "windows"))]
        {
            use std::os::unix::fs::PermissionsExt;
            let exec = dir.join("exec-binary");
            fs::write(&exec, b"#!/bin/sh\n").unwrap();
            fs::set_permissions(&exec, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(command_exists(exec.to_str().unwrap()));
        }

        cleanup(&dir);
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
    fn test_pending_skills_lifecycle() {
        let dir = make_temp_dir();
        let mut manager = SkillManager::new(dir.clone());

        // Distill to pending: never visible via load_all.
        manager
            .create_pending("distilled-rust", sample_skill_md())
            .unwrap();
        assert_eq!(manager.list_pending(), vec!["distilled-rust"]);
        assert!(manager.load_all().unwrap().is_empty());

        // Approve moves into loadable set.
        assert!(manager.approve("distilled-rust").unwrap());
        assert!(manager.list_pending().is_empty());
        assert_eq!(manager.load_all().unwrap().len(), 1);
        assert!(manager.get("test-skill").is_some());
        // Second approve is a no-op.
        assert!(!manager.approve("distilled-rust").unwrap());

        // Create another and discard it: never loadable, pending emptied.
        manager
            .create_pending("distilled-python", sample_skill_md())
            .unwrap();
        assert!(manager.discard_pending("distilled-python").unwrap());
        assert!(manager.list_pending().is_empty());
        assert_eq!(manager.load_all().unwrap().len(), 1);
        assert!(!manager.discard_pending("missing").unwrap());

        cleanup(&dir);
    }

    #[test]
    fn test_load_all_ignores_pending_dir() {
        let dir = make_temp_dir();
        let mut manager = SkillManager::new(dir.clone());
        manager.create("live-skill", sample_skill_md()).unwrap();
        manager
            .create_pending("draft-skill", sample_skill_md())
            .unwrap();
        let loaded = manager.load_all().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].name, "test-skill"); // front-matter name wins
        assert_eq!(manager.list_pending(), vec!["draft-skill"]);
        cleanup(&dir);
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
            origin: SkillOrigin::User,
            pinned: false,
            use_count: 0,
            last_activity_at: None,
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
            origin: SkillOrigin::User,
            pinned: false,
            use_count: 0,
            last_activity_at: None,
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
            origin: SkillOrigin::User,
            pinned: false,
            use_count: 0,
            last_activity_at: None,
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
            prerequisites_env: vec!["KERUX_NONEXISTENT_VAR_12345".into()],
            prerequisites_commands: vec![],
            references: HashMap::new(),
            origin: SkillOrigin::User,
            pinned: false,
            use_count: 0,
            last_activity_at: None,
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

    #[test]
    fn test_record_use_updates_metadata() {
        let tmp = make_temp_dir();
        let mut mgr = SkillManager::new(tmp.clone());
        let content =
            "---\nname: usage-skill\ndescription: Tracks uses\nversion: 1.0.0\n---\nUse me.";
        mgr.create("usage-skill", content).unwrap();
        assert!(mgr.record_use("usage-skill"));
        let skill = mgr.get("usage-skill").unwrap();
        assert_eq!(skill.use_count, 1);
        assert!(skill.last_activity_at.is_some());
        cleanup(&tmp);
    }

    #[test]
    fn test_update_metadata_provenance() {
        let tmp = make_temp_dir();
        let mut mgr = SkillManager::new(tmp.clone());
        let content = "---\nname: prov-skill\ndescription: Prov\nversion: 1.0.0\n---\nBody.";
        mgr.create("prov-skill", content).unwrap();
        assert!(mgr.update_metadata("prov-skill", SkillOrigin::Agent, true));
        let skill = mgr.get("prov-skill").unwrap();
        assert_eq!(skill.origin, SkillOrigin::Agent);
        assert!(skill.pinned);
        cleanup(&tmp);
    }
}
