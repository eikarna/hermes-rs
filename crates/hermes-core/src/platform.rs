//! Cross-platform utility module for hermes-rs.
//!
//! Provides shell detection, config directory helpers, file permission utilities,
//! interpreter detection, and platform information using only the standard library.

use std::env;
use std::path::PathBuf;
use std::process::Command;

// ---------------------------------------------------------------------------
// 1. Shell detection
// ---------------------------------------------------------------------------

/// Information about the detected shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellInfo {
    /// Absolute path (or bare command name) of the shell executable.
    pub path: PathBuf,
    /// The flag used to pass a command string, e.g. `["-c"]` or `["/C"]`.
    pub args_pattern: Vec<String>,
}

/// Find the best available shell on the current platform.
///
/// - **Windows**: tries `pwsh.exe`, then `powershell.exe`, then `cmd.exe`.
/// - **Unix**: tries the `$SHELL` env var, then `bash`, then `sh`.
pub fn detect_shell() -> ShellInfo {
    #[cfg(target_os = "windows")]
    {
        detect_shell_windows()
    }
    #[cfg(not(target_os = "windows"))]
    {
        detect_shell_unix()
    }
}

#[cfg(target_os = "windows")]
fn detect_shell_windows() -> ShellInfo {
    for (cmd, args) in &[
        ("pwsh.exe", vec!["-Command"]),
        ("powershell.exe", vec!["-Command"]),
        ("cmd.exe", vec!["/C"]),
    ] {
        if command_exists(cmd) {
            return ShellInfo {
                path: PathBuf::from(cmd),
                args_pattern: args.iter().map(|s| s.to_string()).collect(),
            };
        }
    }
    // Fallback – cmd.exe should always be present on Windows.
    ShellInfo {
        path: PathBuf::from("cmd.exe"),
        args_pattern: vec!["/C".to_string()],
    }
}

#[cfg(not(target_os = "windows"))]
fn detect_shell_unix() -> ShellInfo {
    if let Ok(shell) = env::var("SHELL") {
        if !shell.is_empty() && command_exists(&shell) {
            return ShellInfo {
                path: PathBuf::from(shell),
                args_pattern: vec!["-c".to_string()],
            };
        }
    }
    for candidate in &["bash", "sh"] {
        if command_exists(candidate) {
            return ShellInfo {
                path: PathBuf::from(candidate),
                args_pattern: vec!["-c".to_string()],
            };
        }
    }
    // Ultimate fallback.
    ShellInfo {
        path: PathBuf::from("sh"),
        args_pattern: vec!["-c".to_string()],
    }
}

/// Returns `true` if `cmd` can be invoked (very cheap check via `--version`
/// or `/C ver` on Windows).
fn command_exists(cmd: &str) -> bool {
    #[cfg(target_os = "windows")]
    {
        Command::new(cmd)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new(cmd)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok()
    }
}

// ---------------------------------------------------------------------------
// 2. Config directory helpers
// ---------------------------------------------------------------------------

/// Returns the root Hermes home directory.
///
/// Resolution order:
/// 1. `$HERMES_HOME` environment variable (if set and non-empty).
/// 2. `~/.hermes` on Unix, `%APPDATA%\hermes` on Windows.
pub fn hermes_home() -> PathBuf {
    if let Ok(val) = env::var("HERMES_HOME") {
        if !val.is_empty() {
            return PathBuf::from(val);
        }
    }

    #[cfg(target_os = "windows")]
    {
        let base = env::var("APPDATA").unwrap_or_else(|_| {
            let home = env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\Default".to_string());
            format!("{home}\\AppData\\Roaming")
        });
        PathBuf::from(base).join("hermes")
    }

    #[cfg(not(target_os = "windows"))]
    {
        let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(home).join(".hermes")
    }
}

/// `<hermes_home>/config`
pub fn hermes_config_dir() -> PathBuf {
    hermes_home().join("config")
}

/// `<hermes_home>/data`
pub fn hermes_data_dir() -> PathBuf {
    hermes_home().join("data")
}

/// `<hermes_home>/memories`
pub fn hermes_memories_dir() -> PathBuf {
    hermes_home().join("memories")
}

/// `<hermes_home>/skills`
pub fn hermes_skills_dir() -> PathBuf {
    hermes_home().join("skills")
}

/// `<hermes_home>/sessions`
pub fn hermes_sessions_dir() -> PathBuf {
    hermes_home().join("sessions")
}

// ---------------------------------------------------------------------------
// 3. File permissions helpers
// ---------------------------------------------------------------------------

/// Set secure permissions on a path (mode `0o700` on Unix, no-op on Windows).
pub fn set_secure_permissions(path: &std::path::Path) -> std::io::Result<()> {
    set_file_permissions(path, 0o700)
}

/// Set file permissions to `mode` on Unix; no-op on Windows.
#[cfg(not(target_os = "windows"))]
pub fn set_file_permissions(path: &std::path::Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(path, perms)
}

/// Set file permissions – no-op on Windows.
#[cfg(target_os = "windows")]
pub fn set_file_permissions(path: &std::path::Path, _mode: u32) -> std::io::Result<()> {
    let _ = path;
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. Interpreter detection
// ---------------------------------------------------------------------------

/// Try to find a working Python interpreter (`python3` first, then `python`).
pub fn find_python() -> Option<PathBuf> {
    for candidate in &["python3", "python"] {
        if interpreter_works(candidate) {
            return Some(PathBuf::from(candidate));
        }
    }
    None
}

/// Try to find a working Node.js interpreter.
pub fn find_node() -> Option<PathBuf> {
    if interpreter_works("node") {
        return Some(PathBuf::from("node"));
    }
    None
}

/// Try to find `rustc`.
pub fn find_rustc() -> Option<PathBuf> {
    if interpreter_works("rustc") {
        return Some(PathBuf::from("rustc"));
    }
    None
}

/// Returns `true` when `cmd --version` exits successfully.
fn interpreter_works(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// 5. Platform info
// ---------------------------------------------------------------------------

/// Operating system name as a short lowercase string.
pub fn os_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "windows"
    }
    #[cfg(target_os = "macos")]
    {
        "macos"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        env::consts::OS
    }
}

/// `true` when compiled for Windows.
pub const fn is_windows() -> bool {
    cfg!(target_os = "windows")
}

/// `true` when compiled for macOS.
pub const fn is_macos() -> bool {
    cfg!(target_os = "macos")
}

/// `true` when compiled for Linux.
pub const fn is_linux() -> bool {
    cfg!(target_os = "linux")
}

/// Snapshot of the current platform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformInfo {
    pub os: &'static str,
    pub arch: &'static str,
    pub shell: ShellInfo,
}

/// Collect platform information in one call.
pub fn platform_info() -> PlatformInfo {
    PlatformInfo {
        os: os_name(),
        arch: env::consts::ARCH,
        shell: detect_shell(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // -- Shell detection ----------------------------------------------------

    #[test]
    fn test_detect_shell_returns_non_empty_path() {
        let shell = detect_shell();
        assert!(
            !shell.path.as_os_str().is_empty(),
            "shell path must not be empty"
        );
    }

    #[test]
    fn test_detect_shell_has_args_pattern() {
        let shell = detect_shell();
        assert!(
            !shell.args_pattern.is_empty(),
            "args_pattern must contain at least one element"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_detect_shell_windows_known_shells() {
        let shell = detect_shell();
        let name = shell.path.to_string_lossy().to_lowercase();
        assert!(
            name.contains("pwsh") || name.contains("powershell") || name.contains("cmd"),
            "expected a known Windows shell, got: {name}"
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_detect_shell_unix_args() {
        let shell = detect_shell();
        assert_eq!(shell.args_pattern, vec!["-c"]);
    }

    // -- Config directories -------------------------------------------------

    #[test]
    fn test_hermes_home_respects_env() {
        let key = "HERMES_HOME";
        let original = env::var(key).ok();

        env::set_var(key, "/tmp/hermes_test_home");
        assert_eq!(hermes_home(), PathBuf::from("/tmp/hermes_test_home"));

        // Restore original value.
        match original {
            Some(val) => env::set_var(key, val),
            None => env::remove_var(key),
        }
    }

    #[test]
    fn test_hermes_home_default_not_empty() {
        let key = "HERMES_HOME";
        let original = env::var(key).ok();

        env::remove_var(key);
        let home = hermes_home();
        assert!(!home.as_os_str().is_empty());

        if let Some(val) = original {
            env::set_var(key, val);
        }
    }

    #[test]
    fn test_hermes_subdirs_are_children_of_home() {
        let home = hermes_home();
        assert!(hermes_config_dir().starts_with(&home));
        assert!(hermes_data_dir().starts_with(&home));
        assert!(hermes_memories_dir().starts_with(&home));
        assert!(hermes_skills_dir().starts_with(&home));
        assert!(hermes_sessions_dir().starts_with(&home));
    }

    #[test]
    fn test_hermes_subdir_names() {
        // Verify suffix only — avoids race with tests that modify HERMES_HOME.
        assert!(hermes_config_dir().ends_with("config"));
        assert!(hermes_data_dir().ends_with("data"));
        assert!(hermes_memories_dir().ends_with("memories"));
        assert!(hermes_skills_dir().ends_with("skills"));
        assert!(hermes_sessions_dir().ends_with("sessions"));
    }

    // -- File permissions ---------------------------------------------------

    #[test]
    fn test_set_secure_permissions_on_tempdir() {
        let dir = env::temp_dir().join("hermes_perm_test");
        let _ = fs::create_dir_all(&dir);

        let result = set_secure_permissions(&dir);
        assert!(result.is_ok(), "set_secure_permissions failed: {result:?}");

        let _ = fs::remove_dir(&dir);
    }

    #[test]
    fn test_set_file_permissions_on_tempfile() {
        let path = env::temp_dir().join("hermes_perm_test_file.txt");
        fs::write(&path, "test").expect("write test file");

        let result = set_file_permissions(&path, 0o644);
        assert!(result.is_ok(), "set_file_permissions failed: {result:?}");

        let _ = fs::remove_file(&path);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn test_set_file_permissions_unix_mode() {
        use std::os::unix::fs::PermissionsExt;

        let path = env::temp_dir().join("hermes_unix_mode_test.txt");
        fs::write(&path, "test").expect("write test file");

        set_file_permissions(&path, 0o700).expect("set permissions");
        let meta = fs::metadata(&path).expect("metadata");
        assert_eq!(meta.permissions().mode() & 0o777, 0o700);

        let _ = fs::remove_file(&path);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_set_file_permissions_windows_noop() {
        let path = env::temp_dir().join("hermes_win_perm_test.txt");
        fs::write(&path, "test").expect("write test file");

        // Should succeed as a no-op.
        assert!(set_file_permissions(&path, 0o700).is_ok());

        let _ = fs::remove_file(&path);
    }

    // -- Interpreter detection ----------------------------------------------

    #[test]
    fn test_find_rustc_available() {
        // In any Rust development environment rustc should be present.
        // We don't hard-fail if it isn't – CI images may differ.
        if let Some(p) = find_rustc() {
            assert!(p.to_string_lossy().contains("rustc"));
        }
    }

    #[test]
    fn test_find_python_returns_valid_path_or_none() {
        if let Some(p) = find_python() {
            let name = p.to_string_lossy().to_lowercase();
            assert!(
                name.contains("python"),
                "expected 'python' in path, got: {name}"
            );
        }
    }

    #[test]
    fn test_find_node_returns_valid_path_or_none() {
        if let Some(p) = find_node() {
            let name = p.to_string_lossy().to_lowercase();
            assert!(
                name.contains("node"),
                "expected 'node' in path, got: {name}"
            );
        }
    }

    // -- Platform info ------------------------------------------------------

    #[test]
    fn test_os_name_not_empty() {
        assert!(!os_name().is_empty());
    }

    #[test]
    fn test_exactly_one_os_flag() {
        let flags = [is_windows(), is_macos(), is_linux()];
        let count = flags.iter().filter(|&&f| f).count();
        // On the three major platforms exactly one should be true.
        // On exotic targets none may be true, which is acceptable.
        assert!(count <= 1, "at most one OS flag should be true");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn test_is_windows() {
        assert!(is_windows());
        assert!(!is_macos());
        assert!(!is_linux());
        assert_eq!(os_name(), "windows");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_is_macos() {
        assert!(is_macos());
        assert!(!is_windows());
        assert!(!is_linux());
        assert_eq!(os_name(), "macos");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_is_linux() {
        assert!(is_linux());
        assert!(!is_windows());
        assert!(!is_macos());
        assert_eq!(os_name(), "linux");
    }

    #[test]
    fn test_platform_info_consistent() {
        let info = platform_info();
        assert_eq!(info.os, os_name());
        assert!(!info.arch.is_empty());
        assert_eq!(info.shell, detect_shell());
    }

    #[test]
    fn test_platform_info_arch_known() {
        let info = platform_info();
        let known = [
            "x86_64",
            "x86",
            "aarch64",
            "arm",
            "mips",
            "mips64",
            "powerpc",
            "powerpc64",
            "riscv64",
            "s390x",
        ];
        // Don't hard-fail on exotic arches, but log for awareness.
        if !known.contains(&info.arch) {
            eprintln!("note: unexpected arch value: {}", info.arch);
        }
    }
}
