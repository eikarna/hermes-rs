//! `kerux session` — manage and export Kerux conversation sessions.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Subcommand, ValueEnum};
use kerux_core::client::{Message, Role};
use kerux_core::config::AppConfig;
use kerux_core::session_store::{SessionData, SessionStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    Markdown,
    Json,
}

#[derive(Debug, Subcommand)]
pub enum SessionCommands {
    /// List recorded conversation sessions.
    List,
    /// Resume a session in TUI or CLI chat mode.
    Resume {
        /// Session ID / channel key to resume.
        session_id: String,
        /// Force non-TUI CLI chat REPL even if rich TUI is configured.
        #[arg(long)]
        cli: bool,
    },
    /// Export a session transcript and tool calls to file or stdout.
    Export {
        /// Session ID / channel key to export.
        session_id: String,
        /// Output format (markdown or json).
        #[arg(long, value_enum, default_value = "markdown")]
        format: ExportFormat,
        /// Output file path (defaults to stdout).
        #[arg(long, short)]
        out: Option<PathBuf>,
    },
}

pub async fn handle(
    config: &AppConfig,
    command: &SessionCommands,
    system_prompt: Option<&str>,
) -> Result<()> {
    let store = SessionStore::new(SessionStore::default_dir());
    match command {
        SessionCommands::List => {
            list_sessions(&store);
            Ok(())
        }
        SessionCommands::Resume { session_id, cli } => {
            resume_session(config, &store, session_id, *cli, system_prompt).await
        }
        SessionCommands::Export {
            session_id,
            format,
            out,
        } => export_session(&store, session_id, *format, out.as_deref()),
    }
}

fn list_sessions(store: &SessionStore) {
    let sessions = store.list();
    if sessions.is_empty() {
        println!("No recorded sessions found in '{}'", store.dir().display());
        return;
    }

    println!(
        "{:<30} {:<20} {:<8} {:<10} {:<30}",
        "SESSION ID", "UPDATED", "MSGS", "TOKENS", "TITLE"
    );
    println!("{}", "-".repeat(100));

    for s in sessions {
        let date_str = if s.updated_at > 0 {
            if let Some(dt) = chrono::DateTime::from_timestamp(s.updated_at as i64, 0) {
                let local: chrono::DateTime<chrono::Local> = dt.into();
                local.format("%Y-%m-%d %H:%M:%S").to_string()
            } else {
                "-".to_string()
            }
        } else {
            "-".to_string()
        };

        let title = if s.title.chars().count() > 30 {
            let truncated: String = s.title.chars().take(27).collect();
            format!("{}...", truncated)
        } else {
            s.title.clone()
        };

        println!(
            "{:<30} {:<20} {:<8} {:<10} {:<30}",
            s.id, date_str, s.message_count, s.estimated_tokens, title
        );
    }
}

async fn resume_session(
    config: &AppConfig,
    store: &SessionStore,
    session_id: &str,
    force_cli: bool,
    system_prompt: Option<&str>,
) -> Result<()> {
    let session_data = store.load(session_id);
    if session_data.messages.is_empty() && session_data.summary.is_none() {
        println!(
            "Notice: Starting new or empty session for '{}'.",
            session_id
        );
    } else {
        println!(
            "Resuming session '{}' (loaded {} messages).",
            session_id,
            session_data.messages.len()
        );
    }

    if config.tui.rich_output && !force_cli {
        crate::tui::TuiApp::enter(
            config.clone(),
            system_prompt.map(str::to_string),
            crate::tui::LaunchMode::Landing,
        )
        .await?
        .run()
        .await?;
    } else {
        let mut mcp_manager = crate::McpManager::new();
        let agent =
            crate::create_agent_without_events(config, system_prompt, &mut mcp_manager).await?;

        // Seed history
        if let Some(summary) = &session_data.summary {
            agent
                .add_message(Message::system(format!(
                    "{}\n{}",
                    kerux_core::agent::CONTEXT_SUMMARY_MARKER,
                    summary
                )))
                .await;
        }
        for msg in &session_data.messages {
            agent.add_message(msg.clone()).await;
        }

        loop {
            use std::io::{self, Write};
            print!("You: ");
            io::stdout().flush()?;
            let mut input = String::new();
            io::stdin().read_line(&mut input)?;
            let input = input.trim();

            if input.is_empty() {
                continue;
            }
            if input.eq_ignore_ascii_case("exit") || input.eq_ignore_ascii_case("quit") {
                // Save session on exit
                let history = agent.conversation().await;
                let _ = store.save(session_id, session_data.summary.as_deref(), &history);
                break;
            }
            if input.eq_ignore_ascii_case("clear") {
                agent.clear_history().await;
                store.clear(session_id);
                println!("Conversation cleared.");
                continue;
            }

            match agent.run(input.to_string()).await {
                Ok(response) => {
                    println!("Assistant: {}\n", response.content);
                    // Persist turn
                    let history = agent.conversation().await;
                    let _ = store.save(session_id, session_data.summary.as_deref(), &history);
                }
                Err(error) => eprintln!("Error: {}\n", error),
            }
        }
    }

    Ok(())
}

fn export_session(
    store: &SessionStore,
    session_id: &str,
    format: ExportFormat,
    out: Option<&std::path::Path>,
) -> Result<()> {
    let session_data = store.load(session_id);
    if session_data.messages.is_empty() && session_data.summary.is_none() {
        anyhow::bail!("Session '{}' not found or is empty.", session_id);
    }

    let output_str = match format {
        ExportFormat::Json => serde_json::to_string_pretty(&serde_json::json!({
            "session_id": session_id,
            "summary": session_data.summary,
            "messages": session_data.messages,
        }))
        .context("Failed to serialize session to JSON")?,
        ExportFormat::Markdown => render_markdown(session_id, &session_data),
    };

    if let Some(path) = out {
        std::fs::write(path, &output_str)
            .with_context(|| format!("Failed to write export to '{}'", path.display()))?;
        println!("Exported session '{}' to '{}'.", session_id, path.display());
    } else {
        println!("{}", output_str);
    }

    Ok(())
}

fn render_markdown(session_id: &str, data: &SessionData) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Session Export: {}\n\n", session_id));

    if let Some(summary) = &data.summary {
        out.push_str("## Context Summary\n\n");
        out.push_str(summary);
        out.push_str("\n\n---\n\n");
    }

    out.push_str("## Transcript\n\n");

    for (idx, msg) in data.messages.iter().enumerate() {
        let role_name = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
            Role::Tool => "Tool Result",
        };

        out.push_str(&format!("### {}. {}\n\n", idx + 1, role_name));

        if let Some(reasoning) = &msg.reasoning {
            out.push_str("> **Reasoning:**\n");
            for line in reasoning.lines() {
                out.push_str(&format!("> {}\n", line));
            }
            out.push('\n');
        }

        if let Some(tool_calls) = &msg.tool_calls {
            out.push_str("**Tool Calls:**\n");
            for tc in tool_calls {
                out.push_str(&format!("- Function: `{}`\n", tc.function.name));
                out.push_str("  Arguments:\n  ```json\n  ");
                out.push_str(&tc.function.arguments);
                out.push_str("\n  ```\n");
            }
            out.push('\n');
        }

        if let Some(tool_id) = &msg.tool_call_id {
            out.push_str(&format!("**Tool Call ID:** `{}`\n\n", tool_id));
        }

        if !msg.content.is_empty() {
            out.push_str(&msg.content);
            out.push_str("\n\n");
        }

        out.push_str("---\n\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use kerux_core::client::ToolCall;

    fn temp_store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (SessionStore::new(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn render_markdown_formats_correctly() {
        let msg1 = Message::user("Hello agent");
        let mut msg2 = Message::assistant("I will check files.");
        msg2.tool_calls = Some(vec![ToolCall {
            id: "call_123".to_string(),
            function: kerux_core::client::ToolCallFunction {
                name: "search_files".to_string(),
                arguments: r#"{"pattern":"*.rs"}"#.to_string(),
            },
        }]);
        let msg3 = Message::tool("call_123", "Found 5 files");

        let data = SessionData {
            summary: Some("Previous topic summary".to_string()),
            messages: vec![msg1, msg2, msg3],
        };

        let md = render_markdown("test_session", &data);
        assert!(md.contains("# Session Export: test_session"));
        assert!(md.contains("## Context Summary"));
        assert!(md.contains("Previous topic summary"));
        assert!(md.contains("### 1. User"));
        assert!(md.contains("Hello agent"));
        assert!(md.contains("### 2. Assistant"));
        assert!(md.contains("search_files"));
        assert!(md.contains("### 3. Tool Result"));
        assert!(md.contains("Found 5 files"));
    }

    #[test]
    fn export_json_and_markdown_roundtrip() {
        let (store, _dir) = temp_store();
        let msgs = vec![Message::user("test message")];
        store.save("my_session", None, &msgs).unwrap();

        let out_dir = tempfile::tempdir().unwrap();
        let md_file = out_dir.path().join("export.md");
        let json_file = out_dir.path().join("export.json");

        export_session(&store, "my_session", ExportFormat::Markdown, Some(&md_file)).unwrap();
        assert!(md_file.exists());
        let md_content = std::fs::read_to_string(&md_file).unwrap();
        assert!(md_content.contains("test message"));

        export_session(&store, "my_session", ExportFormat::Json, Some(&json_file)).unwrap();
        assert!(json_file.exists());
        let json_content = std::fs::read_to_string(&json_file).unwrap();
        assert!(json_content.contains("test message"));
    }
}
