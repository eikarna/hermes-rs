use ratatui::style::Color;

pub(crate) struct UiTheme {
    pub bg: Color,
    pub panel: Color,
    pub panel_alt: Color,
    pub accent: Color,
    pub text: Color,
    pub muted: Color,
    pub help: Color,
    pub success: Color,
    pub error: Color,
    pub warn: Color,
}

pub(crate) const THEME: UiTheme = UiTheme {
    bg: Color::Black,
    panel: Color::Rgb(26, 24, 22),
    panel_alt: Color::Rgb(18, 17, 15),
    accent: Color::Rgb(232, 165, 54),
    text: Color::Rgb(230, 228, 222),
    muted: Color::Rgb(134, 132, 126),
    help: Color::Rgb(188, 184, 176),
    success: Color::Rgb(115, 185, 115),
    error: Color::Rgb(220, 98, 87),
    warn: Color::Rgb(208, 170, 82),
};

pub(crate) const BG: Color = THEME.bg;
pub(crate) const PANEL: Color = THEME.panel;
pub(crate) const PANEL_ALT: Color = THEME.panel_alt;
pub(crate) const ACCENT: Color = THEME.accent;
pub(crate) const TEXT: Color = THEME.text;
pub(crate) const MUTED: Color = THEME.muted;
pub(crate) const HELP: Color = THEME.help;
pub(crate) const SUCCESS: Color = THEME.success;
pub(crate) const ERROR: Color = THEME.error;
pub(crate) const WARN: Color = THEME.warn;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_theme_matches_expected_palette() {
        assert_eq!(THEME.bg, Color::Black);
        assert_eq!(THEME.panel, Color::Rgb(26, 24, 22));
        assert_eq!(THEME.panel_alt, Color::Rgb(18, 17, 15));
        assert_eq!(THEME.accent, Color::Rgb(232, 165, 54));
        assert_eq!(THEME.text, Color::Rgb(230, 228, 222));
        assert_eq!(THEME.muted, Color::Rgb(134, 132, 126));
        assert_eq!(THEME.help, Color::Rgb(188, 184, 176));
        assert_eq!(THEME.success, Color::Rgb(115, 185, 115));
        assert_eq!(THEME.error, Color::Rgb(220, 98, 87));
        assert_eq!(THEME.warn, Color::Rgb(208, 170, 82));
    }
}
