//! Headless TUI screenshot capture.
//!
//! Renders the TUI into a ratatui [`TestBackend`] buffer, converts the buffer
//! to an HTML grid, then rasterises it with a headless Chromium binary. No new
//! Rust dependencies are introduced — the browser is invoked via `std::process`.
//!
//! Used by the `screenshot` hidden subcommand and the CI preview workflow.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use ratatui::Terminal;

use kerux_core::config::AppConfig;

use crate::tui::render::render;
use crate::tui::state::{ActivityItem, AppState, TelemetryState, Tone, TranscriptEntry};

/// Terminal grid size for the captured shots.
const COLS: u16 = 120;
const ROWS: u16 = 36;

/// HTML cell metrics (pixels). DejaVu Sans Mono advances 0.6em per glyph, so
/// at 15px font each cell is exactly 9px wide — matching the CSS `ch` unit.
const CELL_W: u32 = 9;
const CELL_H: u32 = 18;
const FONT_PX: u32 = 15;

/// Candidate headless browser binaries, in preference order.
const BROWSERS: &[&str] = &[
    "chromium",
    "chromium-browser",
    "google-chrome",
    "google-chrome-stable",
];

/// Capture the landing and workspace shots into `out_dir`.
pub fn capture(config: &AppConfig, out_dir: &Path) -> Result<()> {
    fs::create_dir_all(out_dir).with_context(|| format!("creating {}", out_dir.display()))?;

    let landing = build_landing(config);
    let workspace = build_workspace(config);

    let shots = [("main.png", &landing), ("chat.png", &workspace)];

    for (name, state) in shots {
        let buffer = render_to_buffer(state);
        let html = buffer_to_html(&buffer);
        let html_path = out_dir.join(format!(
            "{}.html",
            Path::new(name).file_stem().unwrap().to_string_lossy()
        ));
        fs::write(&html_path, &html).with_context(|| format!("writing {}", html_path.display()))?;

        let png_path = out_dir.join(name);
        rasterise(&html_path, &png_path)?;
        // The HTML is an intermediate artifact; keep the directory tidy.
        let _ = fs::remove_file(&html_path);
        println!("captured {}", png_path.display());
    }

    Ok(())
}

fn render_to_buffer(state: &AppState) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(COLS, ROWS);
    let mut terminal = Terminal::new(backend).expect("test backend");
    terminal.draw(|frame| render(frame, state)).expect("draw");
    terminal.backend().buffer().clone()
}

/// Build the landing-screen demo state.
fn build_landing(config: &AppConfig) -> AppState {
    let mut state = AppState::new(config.clone(), String::new(), false);
    state.persistent.behavior.model = "glm-5.2".to_string();
    state
}

/// Build a populated workspace demo state so the shot shows a real session.
fn build_workspace(config: &AppConfig) -> AppState {
    let mut state = AppState::new(
        config.clone(),
        "Summarize the gateway module".to_string(),
        true,
    );
    state.persistent.behavior.model = "glm-5.2".to_string();
    state.session.title = "Gateway deep-dive".to_string();
    state.session.active_query = "Summarize the gateway module".to_string();
    state.session.status = "Idle".to_string();
    state.session.running = false;
    state.session.transcript = vec![
        TranscriptEntry {
            role: "User",
            content: "Summarize the gateway module.".to_string(),
        },
        TranscriptEntry {
            role: "Assistant",
            content: "The gateway connects Kerux to messaging platforms. It \
                      long-polls Telegram, drains the WhatsApp bridge queue, \
                      converts Markdown to MarkdownV2, and streams replies \
                      back with live status edits."
                .to_string(),
        },
    ];
    state.session.activity = vec![
        ActivityItem {
            label: "Ready".to_string(),
            body: "Session loaded from disk.".to_string(),
            tone: Tone::Success,
        },
        ActivityItem {
            label: "Tool".to_string(),
            body: "read_file crates/kerux-core/src/gateway.rs".to_string(),
            tone: Tone::Info,
        },
    ];
    state.session.telemetry = TelemetryState {
        prompt_tokens: 1840,
        completion_tokens: 213,
        total_tokens: 2053,
        context_window: 65536,
        compacted: false,
        estimated: false,
        total_cost: 0.0,
        tokens_per_second: Some(42.5),
        turns_completed: 1,
        context_window_usage_pct: Some(3.1),
        cached_prompt_tokens: 512,
    };
    state
}

/// Convert a ratatui buffer into a standalone HTML document.
fn buffer_to_html(buffer: &ratatui::buffer::Buffer) -> String {
    let width = buffer.area.width as usize;
    let height = buffer.area.height as usize;
    let px_w = width as u32 * CELL_W;
    let px_h = height as u32 * CELL_H;

    let mut body = String::new();
    for y in 0..height {
        body.push_str("<div class=\"row\">");
        let mut x = 0;
        while x < width {
            let cell = &buffer.content[y * width + x];
            let style = cell.style();
            // Coalesce a run of identically-styled cells into one span.
            let mut run = String::new();
            push_symbol(&mut run, cell.symbol());
            let mut nx = x + 1;
            while nx < width {
                let next = &buffer.content[y * width + nx];
                if next.style() != style {
                    break;
                }
                push_symbol(&mut run, next.symbol());
                nx += 1;
            }

            let fg = css_color(style.fg.unwrap_or(Color::Reset), "#e6e4de");
            let bg = css_color(style.bg.unwrap_or(Color::Reset), "#000000");
            let mut deco = String::new();
            if style.add_modifier.contains(Modifier::BOLD) {
                deco.push_str("font-weight:bold;");
            }
            if style.add_modifier.contains(Modifier::ITALIC) {
                deco.push_str("font-style:italic;");
            }
            if style.add_modifier.contains(Modifier::UNDERLINED) {
                deco.push_str("text-decoration:underline;");
            }
            let opacity = if style.add_modifier.contains(Modifier::DIM) {
                "opacity:0.6;"
            } else {
                ""
            };

            let nchars = run.chars().count().max(1);
            body.push_str(&format!(
                "<span style=\"width:{nchars}ch;color:{fg};background:{bg};{deco}{opacity}\">{run}</span>"
            ));
            x = nx;
        }
        body.push_str("</div>\n");
    }

    format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><style>
html,body{{margin:0;padding:0;background:#000000;}}
.row{{white-space:pre;height:{CELL_H}px;line-height:{CELL_H}px;}}
.row span{{display:inline-block;height:{CELL_H}px;
line-height:{CELL_H}px;font:{FONT_PX}px/{CELL_H}px 'DejaVu Sans Mono',monospace;
text-align:left;overflow:hidden;}}
</style></head>
<body style="width:{px_w}px;height:{px_h}px;">
{body}
</body></html>"#
    )
}

fn push_symbol(out: &mut String, symbol: &str) {
    for ch in symbol.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            ' ' => out.push('\u{00a0}'),
            c => out.push(c),
        }
    }
}

fn css_color(color: Color, default: &str) -> String {
    match color {
        Color::Reset => default.to_string(),
        Color::Black => "#000000".to_string(),
        Color::Red => "#dc6257".to_string(),
        Color::Green => "#73b973".to_string(),
        Color::Yellow => "#d0aa52".to_string(),
        Color::Blue => "#6f9fd8".to_string(),
        Color::Magenta => "#c586c0".to_string(),
        Color::Cyan => "#6fc3c3".to_string(),
        Color::Gray => "#86847e".to_string(),
        Color::DarkGray => "#5a5852".to_string(),
        Color::LightRed => "#e88a7a".to_string(),
        Color::LightGreen => "#9fd89f".to_string(),
        Color::LightYellow => "#e8ce8a".to_string(),
        Color::LightBlue => "#9fbfe8".to_string(),
        Color::LightMagenta => "#d8a8d8".to_string(),
        Color::LightCyan => "#9fd8d8".to_string(),
        Color::White => "#e6e4de".to_string(),
        Color::Rgb(r, g, b) => format!("#{:02x}{:02x}{:02x}", r, g, b),
        Color::Indexed(i) => format!("#{:02x}{:02x}{:02x}", i, i, i),
    }
}

/// Rasterise the HTML file to PNG using a headless browser.
fn rasterise(html: &Path, png: &Path) -> Result<()> {
    let browser = BROWSERS
        .iter()
        .find(|b| which(b))
        .copied()
        .context("no headless Chromium binary found (chromium / google-chrome)")?;

    let px_w = COLS as u32 * CELL_W;
    let px_h = ROWS as u32 * CELL_H;

    let url = format!(
        "file://{}",
        html.canonicalize()
            .context("canonicalize html path")?
            .display()
    );

    let status = Command::new(browser)
        .arg("--headless=new")
        .arg("--disable-gpu")
        .arg("--no-sandbox")
        .arg("--hide-scrollbars")
        .arg("--force-device-scale-factor=1")
        .arg("--default-background-color=FF000000")
        .arg(format!("--window-size={px_w},{px_h}"))
        .arg(format!("--screenshot={}", png.display()))
        .arg(url)
        .status()
        .with_context(|| format!("spawning {browser}"))?;

    if !status.success() {
        bail!("headless browser exited with {status}");
    }
    if !png.exists() {
        bail!("screenshot was not written to {}", png.display());
    }
    Ok(())
}

fn which(bin: &str) -> bool {
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if PathBuf::from(dir).join(bin).is_file() {
                return true;
            }
        }
    }
    false
}
