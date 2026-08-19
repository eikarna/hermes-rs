use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

static RUNTIME_CONFIG: OnceLock<RwLock<AppConfig>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub client: ClientSettings,
    pub agent: BehaviorSettings,
    pub autonomous: AutonomousSettings,
    pub logging: LoggingSettings,
    pub tui: TuiSettings,
    pub telemetry: TelemetrySettings,
    pub mcp: McpSettings,
    pub skills: SkillsSettings,
    pub gateway: GatewaySettings,
    pub tools: ToolSettings,
    pub curator: crate::curator::CurationPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetrySettings {
    pub enabled: bool,
    pub currency: String,
    pub input_cost_per_million: f64,
    pub output_cost_per_million: f64,
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            currency: "USD".to_string(),
            input_cost_per_million: 0.0,
            output_cost_per_million: 0.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClientSettings {
    pub base_url: String,
    pub api_key: Option<String>,
    pub auth_ref: Option<String>,
    pub timeout_secs: u64,
    pub max_context_length: usize,
    /// LLM provider backend: "openai" (default), "anthropic", "ollama", "openrouter", "gemini"
    pub provider: String,
    /// Optional per-provider connection overrides.
    pub openai: ProviderEndpointSettings,
    pub anthropic: ProviderEndpointSettings,
    pub ollama: ProviderEndpointSettings,
    pub openrouter: ProviderEndpointSettings,
    pub gemini: ProviderEndpointSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderEndpointSettings {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub timeout_secs: Option<u64>,
}

impl Default for ClientSettings {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_string(),
            api_key: None,
            auth_ref: None,
            timeout_secs: 60,
            max_context_length: 128_000,
            provider: "openai".to_string(),
            openai: ProviderEndpointSettings::default(),
            anthropic: ProviderEndpointSettings::default(),
            ollama: ProviderEndpointSettings::default(),
            openrouter: ProviderEndpointSettings::default(),
            gemini: ProviderEndpointSettings::default(),
        }
    }
}

impl ClientSettings {
    fn endpoint_settings(
        &self,
        kind: crate::client::ProviderKind,
    ) -> Option<&ProviderEndpointSettings> {
        match kind {
            crate::client::ProviderKind::Openai => Some(&self.openai),
            crate::client::ProviderKind::Ollama => Some(&self.ollama),
            crate::client::ProviderKind::Openrouter => Some(&self.openrouter),
            crate::client::ProviderKind::Anthropic => Some(&self.anthropic),
            crate::client::ProviderKind::Gemini => Some(&self.gemini),
            crate::client::ProviderKind::Nous => None,
        }
    }

    pub(crate) fn resolved_base_url_for(
        &self,
        kind: crate::client::ProviderKind,
    ) -> Option<String> {
        let configured = self
            .endpoint_settings(kind)
            .and_then(|settings| settings.base_url.clone());
        let value = configured.or(match kind {
            crate::client::ProviderKind::Openai => Some(self.base_url.clone()),
            crate::client::ProviderKind::Ollama => non_empty_env("OLLAMA_BASE_URL"),
            crate::client::ProviderKind::Openrouter => non_empty_env("OPENROUTER_BASE_URL"),
            crate::client::ProviderKind::Anthropic => non_empty_env("ANTHROPIC_BASE_URL"),
            crate::client::ProviderKind::Gemini => non_empty_env("GEMINI_BASE_URL"),
            crate::client::ProviderKind::Nous => None,
        });
        value.filter(|v| !v.trim().is_empty())
    }

    pub(crate) fn resolved_api_key_for(&self, kind: crate::client::ProviderKind) -> Option<String> {
        let configured = self
            .endpoint_settings(kind)
            .and_then(|settings| settings.api_key.clone());
        configured
            .or(match kind {
                crate::client::ProviderKind::Openai => self
                    .api_key
                    .clone()
                    .or_else(|| non_empty_env("OPENAI_API_KEY")),
                crate::client::ProviderKind::Ollama => non_empty_env("OLLAMA_API_KEY"),
                crate::client::ProviderKind::Openrouter => non_empty_env("OPENROUTER_API_KEY"),
                crate::client::ProviderKind::Anthropic => non_empty_env("ANTHROPIC_API_KEY"),
                crate::client::ProviderKind::Gemini => non_empty_env("GEMINI_API_KEY"),
                crate::client::ProviderKind::Nous => None,
            })
            .filter(|v| !v.trim().is_empty())
    }

    pub(crate) fn resolved_timeout_secs_for(
        &self,
        kind: crate::client::ProviderKind,
    ) -> Option<u64> {
        let configured = self
            .endpoint_settings(kind)
            .and_then(|settings| settings.timeout_secs);
        configured.or(match kind {
            crate::client::ProviderKind::Openai => Some(self.timeout_secs),
            crate::client::ProviderKind::Ollama => {
                non_empty_env("OLLAMA_TIMEOUT_SECS").and_then(|value| value.parse().ok())
            }
            crate::client::ProviderKind::Openrouter => {
                non_empty_env("OPENROUTER_TIMEOUT_SECS").and_then(|value| value.parse().ok())
            }
            crate::client::ProviderKind::Anthropic => {
                non_empty_env("ANTHROPIC_TIMEOUT_SECS").and_then(|value| value.parse().ok())
            }
            crate::client::ProviderKind::Gemini => {
                non_empty_env("GEMINI_TIMEOUT_SECS").and_then(|value| value.parse().ok())
            }
            crate::client::ProviderKind::Nous => None,
        })
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BehaviorSettings {
    pub model: String,
    pub max_iterations: usize,
    pub tool_timeout_secs: u64,
    pub request_timeout_secs: u64,
    pub system_prompt: Option<String>,
    pub stream: bool,
    pub context_window: usize,
    pub max_healing_attempts: usize,
    pub show_reasoning: bool,
    /// Token budget for the `<repo_map>` context block. `0` disables
    /// repository mapping entirely.
    pub repo_map_tokens: usize,
    /// Maximum files discovered for repo map scoring (cap huge repos).
    /// Defaults to 500; increase only if needed for very small repos.
    pub repo_map_max_files: usize,
    /// Force the edit-format prompt hint (`search_replace`, `patch`, or
    /// `full_file`) regardless of what the capability table guesses for the
    /// configured model. `None` keeps capability-table behavior.
    pub edit_format_override: Option<crate::client::EditFormat>,
    /// After each successful interactive run, auto-commit working-tree
    /// changes with a Conventional Commit derived from the staged diff.
    /// Default `false`: keep `/undo`-only behavior. Agent-authored edits
    /// inside a transaction are only committed when the run succeeds.
    pub auto_commit: bool,
}

impl Default for BehaviorSettings {
    fn default() -> Self {
        Self {
            model: "gpt-4".to_string(),
            max_iterations: 20,
            tool_timeout_secs: 30,
            request_timeout_secs: 120,
            system_prompt: None,
            stream: true,
            context_window: 128_000,
            max_healing_attempts: 3,
            show_reasoning: true,
            repo_map_tokens: 0,
            repo_map_max_files: 500,
            edit_format_override: None,
            auto_commit: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AutonomousSettings {
    pub interval_secs: u64,
    pub todo_path: PathBuf,
    pub status_path: PathBuf,
    pub test_command: String,
    pub git_remote: String,
    pub git_branch: String,
    pub commit_message: String,
    pub command_timeout_secs: u64,
    pub max_failures_per_state: usize,
}

impl Default for AutonomousSettings {
    fn default() -> Self {
        Self {
            interval_secs: 300,
            todo_path: PathBuf::from("TODO.md"),
            status_path: PathBuf::from("autonomous-status.toml"),
            test_command: "cargo test --workspace".to_string(),
            git_remote: "origin".to_string(),
            git_branch: "agent-dev".to_string(),
            commit_message: "Auto-commit by hermes-rs".to_string(),
            command_timeout_secs: 900,
            max_failures_per_state: 3,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingSettings {
    pub level: String,
    pub format: String,
    pub log_file: Option<String>,
    pub with_target: bool,
    pub with_thread_ids: bool,
    pub with_file: bool,
    pub with_line_number: bool,
}

impl Default for LoggingSettings {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: "pretty".to_string(),
            log_file: None,
            with_target: false,
            with_thread_ids: false,
            with_file: false,
            with_line_number: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TuiSettings {
    pub theme: String,
    pub rich_output: bool,
    pub show_tool_calls: bool,
    pub show_iterations: bool,
    pub landing_title: String,
    pub prompt_placeholder: String,
    pub refresh_rate_ms: u64,
    pub compact_width: u16,
    pub medium_width: u16,
}

impl Default for TuiSettings {
    fn default() -> Self {
        Self {
            theme: "opencode".to_string(),
            rich_output: true,
            show_tool_calls: true,
            show_iterations: true,
            landing_title: "HERMES".to_string(),
            prompt_placeholder: "Ask anything... \"Fix a TODO in the codebase\"".to_string(),
            refresh_rate_ms: 80,
            compact_width: 96,
            medium_width: 120,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum McpTransportKind {
    #[default]
    Http,
    Stdio,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpServerConfig {
    pub name: String,
    pub transport: McpTransportKind,
    pub url: Option<String>,
    pub auth_token: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
    pub enabled: bool,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            transport: McpTransportKind::Http,
            url: None,
            auth_token: None,
            command: None,
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpSettings {
    pub autoload: bool,
    pub servers: Vec<McpServerConfig>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            autoload: true,
            servers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SkillsSettings {
    pub root_dir: PathBuf,
    pub autoload: bool,
    pub template_name: String,
    pub template_description: String,
}

impl Default for SkillsSettings {
    fn default() -> Self {
        let root_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hermes")
            .join("skills");

        Self {
            root_dir,
            autoload: true,
            template_name: "new-skill".to_string(),
            template_description: "Describe what this skill does.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewaySettings {
    pub telegram_enabled: bool,
    pub telegram_token: Option<String>,
    pub telegram_api_base: String,
    pub discord_enabled: bool,
    pub discord_token: Option<String>,
    pub discord_api_base: String,
    pub slack_enabled: bool,
    pub slack_token: Option<String>,
    pub slack_api_base: String,
    pub webhooks_enabled: bool,
    pub webhooks_addr: Option<String>,
    pub admins: Vec<String>,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            telegram_enabled: false,
            telegram_token: None,
            telegram_api_base: "https://api.telegram.org".to_string(),
            discord_enabled: false,
            discord_token: None,
            discord_api_base: "https://discord.com/api/v10".to_string(),
            slack_enabled: false,
            slack_token: None,
            slack_api_base: "https://slack.com/api".to_string(),
            webhooks_enabled: false,
            webhooks_addr: None,
            admins: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolSettings {
    pub registry_timeout_secs: u64,
    pub event_channel_size: usize,
    pub web: WebToolSettings,
    pub http: HttpToolSettings,
    pub terminal: TerminalSettings,
    pub code_execution: CodeExecutionSettings,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            registry_timeout_secs: 30,
            event_channel_size: 100,
            web: WebToolSettings::default(),
            http: HttpToolSettings::default(),
            terminal: TerminalSettings::default(),
            code_execution: CodeExecutionSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebToolSettings {
    pub search_url: String,
    pub search_timeout_secs: u64,
    pub fetch_timeout_secs: u64,
    pub user_agent: String,
    pub default_results: usize,
    pub max_results: usize,
}

impl Default for WebToolSettings {
    fn default() -> Self {
        Self {
            search_url: "https://lite.duckduckgo.com/lite/?q={query}".to_string(),
            search_timeout_secs: 15,
            fetch_timeout_secs: 30,
            user_agent: "Mozilla/5.0 (compatible; HermesAgent/0.1)".to_string(),
            default_results: 10,
            max_results: 20,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpToolSettings {
    pub timeout_secs: u64,
}

impl Default for HttpToolSettings {
    fn default() -> Self {
        Self { timeout_secs: 30 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalSettings {
    pub max_timeout_secs: u64,
    pub max_output_bytes: usize,
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            max_timeout_secs: 300,
            max_output_bytes: 1_000_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CodeExecutionSettings {
    pub default_timeout_secs: u64,
    pub max_timeout_secs: u64,
}

impl Default for CodeExecutionSettings {
    fn default() -> Self {
        Self {
            default_timeout_secs: 60,
            max_timeout_secs: 300,
        }
    }
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub source: Option<PathBuf>,
}

pub fn install_runtime_config(config: AppConfig) {
    let store = RUNTIME_CONFIG.get_or_init(|| RwLock::new(AppConfig::default()));
    if let Ok(mut current) = store.write() {
        *current = config;
    }
}

pub fn runtime_config() -> AppConfig {
    let store = RUNTIME_CONFIG.get_or_init(|| RwLock::new(AppConfig::default()));
    store
        .read()
        .map(|config| config.clone())
        .unwrap_or_else(|_| AppConfig::default())
}

pub fn load_app_config(explicit: Option<&Path>) -> Result<LoadedConfig> {
    if let Some(path) = explicit {
        if !path.exists() {
            return Err(Error::Config(format!(
                "Config file '{}' was not found. Pass a valid --config path or create hermes.toml.",
                path.display()
            )));
        }

        return Ok(LoadedConfig {
            config: parse_config_file(path)?,
            source: Some(path.to_path_buf()),
        });
    }

    for path in default_config_paths() {
        if path.exists() {
            return Ok(LoadedConfig {
                config: parse_config_file(&path)?,
                source: Some(path),
            });
        }
    }

    Ok(LoadedConfig {
        config: AppConfig::default(),
        source: None,
    })
}

pub fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from("hermes.toml"), PathBuf::from(".hermes.toml")];

    if let Some(config_dir) = dirs::config_dir() {
        paths.push(config_dir.join("hermes").join("config.toml"));
    }

    paths
}

pub fn parse_config_file(path: &Path) -> Result<AppConfig> {
    let raw = std::fs::read_to_string(path).map_err(|error| {
        Error::Config(format!(
            "Failed to read config file '{}': {}",
            path.display(),
            error
        ))
    })?;

    parse_config_str(&raw, path)
}

pub fn parse_config_str(raw: &str, source: &Path) -> Result<AppConfig> {
    toml::from_str(raw).map_err(|error| {
        let message = match error.span() {
            Some(span) => format!(
                "Invalid TOML in '{}': {} (bytes {}..{})",
                source.display(),
                error,
                span.start,
                span.end
            ),
            None => format!("Invalid TOML in '{}': {}", source.display(), error),
        };

        Error::Config(message)
    })
}

impl AppConfig {
    pub fn apply_env_overrides(&mut self) -> Result<()> {
        apply_string_value_override("HERMES_PROVIDER", &mut self.client.provider);
        apply_string_option_override("OPENAI_API_KEY", &mut self.client.api_key)?;
        apply_string_value_override("OPENAI_BASE_URL", &mut self.client.base_url);
        apply_string_option_override("HERMES_AUTH_REF", &mut self.client.auth_ref)?;
        apply_string_value_override("HERMES_MODEL", &mut self.agent.model);
        apply_usize_override("HERMES_MAX_ITERATIONS", &mut self.agent.max_iterations)?;
        apply_u64_override("HERMES_TOOL_TIMEOUT", &mut self.agent.tool_timeout_secs)?;
        apply_u64_override(
            "HERMES_REQUEST_TIMEOUT",
            &mut self.agent.request_timeout_secs,
        )?;
        apply_usize_override("HERMES_CONTEXT_WINDOW", &mut self.agent.context_window)?;
        apply_usize_override(
            "HERMES_MAX_HEALING_ATTEMPTS",
            &mut self.agent.max_healing_attempts,
        )?;
        apply_bool_override("HERMES_STREAM", &mut self.agent.stream)?;
        apply_string_option_override("HERMES_SYSTEM_PROMPT", &mut self.agent.system_prompt)?;
        apply_u64_override(
            "HERMES_AUTONOMOUS_INTERVAL",
            &mut self.autonomous.interval_secs,
        )?;
        apply_path_override("HERMES_AUTONOMOUS_TODO", &mut self.autonomous.todo_path)?;
        apply_path_override("HERMES_AUTONOMOUS_STATUS", &mut self.autonomous.status_path)?;
        apply_string_value_override(
            "HERMES_AUTONOMOUS_TEST_COMMAND",
            &mut self.autonomous.test_command,
        );
        apply_string_value_override(
            "HERMES_AUTONOMOUS_GIT_REMOTE",
            &mut self.autonomous.git_remote,
        );
        apply_string_value_override(
            "HERMES_AUTONOMOUS_GIT_BRANCH",
            &mut self.autonomous.git_branch,
        );
        apply_string_value_override(
            "HERMES_AUTONOMOUS_COMMIT_MESSAGE",
            &mut self.autonomous.commit_message,
        );
        apply_u64_override(
            "HERMES_AUTONOMOUS_COMMAND_TIMEOUT",
            &mut self.autonomous.command_timeout_secs,
        )?;
        apply_usize_override(
            "HERMES_AUTONOMOUS_MAX_FAILURES",
            &mut self.autonomous.max_failures_per_state,
        )?;
        apply_string_value_override("HERMES_LOG_LEVEL", &mut self.logging.level);
        apply_path_override("HERMES_SKILLS_DIR", &mut self.skills.root_dir)?;
        Ok(())
    }
}

fn read_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn apply_string_option_override(name: &str, target: &mut Option<String>) -> Result<()> {
    if let Some(value) = read_env(name) {
        *target = Some(value);
    }
    Ok(())
}

fn apply_string_value_override(name: &str, target: &mut String) {
    if let Some(value) = read_env(name) {
        *target = value;
    }
}

fn apply_u64_override(name: &str, target: &mut u64) -> Result<()> {
    if let Some(value) = read_env(name) {
        *target = value.parse().map_err(|_| {
            Error::Config(format!(
                "Environment variable '{}' must be an unsigned integer.",
                name
            ))
        })?;
    }
    Ok(())
}

fn apply_usize_override(name: &str, target: &mut usize) -> Result<()> {
    if let Some(value) = read_env(name) {
        *target = value.parse().map_err(|_| {
            Error::Config(format!(
                "Environment variable '{}' must be an unsigned integer.",
                name
            ))
        })?;
    }
    Ok(())
}

fn apply_bool_override(name: &str, target: &mut bool) -> Result<()> {
    if let Some(value) = read_env(name) {
        *target = match value.to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => {
                return Err(Error::Config(format!(
                    "Environment variable '{}' must be a boolean.",
                    name
                )))
            }
        };
    }
    Ok(())
}

fn apply_path_override(name: &str, target: &mut PathBuf) -> Result<()> {
    if let Some(value) = read_env(name) {
        *target = PathBuf::from(value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::{Mutex, OnceLock};

    static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn env_lock() -> &'static Mutex<()> {
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "hermes_config_test_{}_{}_{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn with_current_dir<T>(path: &Path, f: impl FnOnce() -> T) -> T {
        let current = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        let result = f();
        std::env::set_current_dir(current).unwrap();
        result
    }

    fn set_env(name: &str, value: &str) -> Option<OsString> {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        previous
    }

    fn restore_env(name: &str, previous: Option<OsString>) {
        if let Some(value) = previous {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    #[test]
    fn example_toml_parses() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("hermes.example.toml");
        let raw = std::fs::read_to_string(&root).unwrap();
        let config = parse_config_str(&raw, &root).unwrap();
        assert_eq!(config.client.provider, "openai");
        assert_eq!(config.agent.model, "gpt-4");
        assert!(config.tui.rich_output);
        assert_eq!(config.autonomous.git_branch, "agent-dev");
        assert_eq!(
            config.autonomous.status_path,
            PathBuf::from("autonomous-status.toml")
        );
        assert_eq!(config.curator.memory_decay_days, 14);
        assert_eq!(config.curator.skill_distill_min_facts, 3);
    }

    #[test]
    fn agent_edit_format_override_parses() {
        let settings: BehaviorSettings =
            toml::from_str("model = \"gpt-4\"\nedit_format_override = \"search_replace\"\n")
                .unwrap();
        assert_eq!(
            settings.edit_format_override,
            Some(crate::client::EditFormat::SearchReplace)
        );

        let settings: BehaviorSettings = toml::from_str("model = \"gpt-4\"\n").unwrap();
        assert_eq!(settings.edit_format_override, None);
    }

    #[test]
    fn client_gemini_provider_resolves() {
        let settings: ClientSettings =
            toml::from_str("provider = \"gemini\"\n[gemini]\napi_key = \"k\"\n").unwrap();
        let resolved = crate::client::resolve_provider_settings(&settings).unwrap();
        assert_eq!(resolved.kind, crate::client::ProviderKind::Gemini);
        assert_eq!(resolved.config.api_key.as_deref(), Some("k"));
        assert_eq!(
            resolved.config.base_url,
            "https://generativelanguage.googleapis.com/v1beta"
        );
    }

    #[test]
    fn agent_auto_commit_defaults_off_and_parses() {
        let settings: BehaviorSettings = toml::from_str("model = \"gpt-4\"\n").unwrap();
        assert!(!settings.auto_commit);

        let settings: BehaviorSettings =
            toml::from_str("model = \"gpt-4\"\nauto_commit = true\n").unwrap();
        assert!(settings.auto_commit);
    }

    #[test]
    fn default_path_discovery_prefers_local_hermes_toml() {
        let _guard = env_lock().lock().unwrap();
        let dir = temp_dir("default_path");
        std::fs::write(
            dir.join("hermes.toml"),
            "[agent]\nmodel = \"gpt-4.1-mini\"\n",
        )
        .unwrap();

        let loaded = with_current_dir(&dir, || load_app_config(None)).unwrap();
        assert_eq!(loaded.source.unwrap().file_name().unwrap(), "hermes.toml");
        assert_eq!(loaded.config.agent.model, "gpt-4.1-mini");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn explicit_path_overrides_defaults() {
        let _guard = env_lock().lock().unwrap();
        let dir = temp_dir("explicit_path");
        let explicit = dir.join("custom.toml");
        std::fs::write(dir.join("hermes.toml"), "[agent]\nmodel = \"wrong\"\n").unwrap();
        std::fs::write(&explicit, "[agent]\nmodel = \"right\"\n").unwrap();

        let loaded = with_current_dir(&dir, || load_app_config(Some(&explicit))).unwrap();
        assert_eq!(loaded.config.agent.model, "right");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_provider_endpoint_overrides() {
        let config = parse_config_str(
            r##"
            [client]
            provider = "anthropic"

            [client.anthropic]
            base_url = "https://anthropic.local/v1"
            api_key = "anthropic-key"
            timeout_secs = 33
            "##,
            Path::new("providers.toml"),
        )
        .unwrap();

        assert_eq!(config.client.provider, "anthropic");
        assert_eq!(
            config.client.anthropic.base_url.as_deref(),
            Some("https://anthropic.local/v1")
        );
        assert_eq!(
            config
                .client
                .resolved_api_key_for(crate::client::ProviderKind::Anthropic)
                .as_deref(),
            Some("anthropic-key")
        );
    }

    #[test]
    fn invalid_toml_returns_field_aware_error() {
        let path = PathBuf::from("broken.toml");
        let error = parse_config_str("[agent]\nmax_iterations = \"many\"\n", &path).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("Invalid TOML"));
        assert!(text.contains("expected"));
    }

    #[test]
    fn env_overrides_apply_after_file_values() {
        let _guard = env_lock().lock().unwrap();
        let previous_model = set_env("HERMES_MODEL", "gpt-4.1");
        let previous_provider = set_env("HERMES_PROVIDER", "ollama");
        let previous_stream = set_env("HERMES_STREAM", "false");
        let previous_interval = set_env("HERMES_AUTONOMOUS_INTERVAL", "120");
        let previous_status = set_env("HERMES_AUTONOMOUS_STATUS", "runtime/autonomous-status.toml");

        let mut config = parse_config_str(
            "[agent]\nmodel = \"gpt-4o-mini\"\nstream = true\n",
            Path::new("env.toml"),
        )
        .unwrap();
        config.apply_env_overrides().unwrap();

        assert_eq!(config.agent.model, "gpt-4.1");
        assert_eq!(config.client.provider, "ollama");
        assert!(!config.agent.stream);
        assert_eq!(config.autonomous.interval_secs, 120);
        assert_eq!(
            config.autonomous.status_path,
            PathBuf::from("runtime/autonomous-status.toml")
        );

        restore_env("HERMES_MODEL", previous_model);
        restore_env("HERMES_PROVIDER", previous_provider);
        restore_env("HERMES_STREAM", previous_stream);
        restore_env("HERMES_AUTONOMOUS_INTERVAL", previous_interval);
        restore_env("HERMES_AUTONOMOUS_STATUS", previous_status);
    }
}
