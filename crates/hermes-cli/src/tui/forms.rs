use hermes_core::config::McpTransportKind;

#[derive(Debug, Clone)]
pub struct FormField {
    pub label: &'static str,
    pub value: String,
    pub secret: bool,
}

impl FormField {
    pub fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            secret: false,
        }
    }

    pub fn secret(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            secret: true,
        }
    }

    pub fn display_value(&self) -> String {
        if self.secret && !self.value.is_empty() {
            "*".repeat(self.value.chars().count().min(16))
        } else {
            self.value.clone()
        }
    }
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub title: &'static str,
    pub help: &'static str,
    pub fields: Vec<FormField>,
    pub selected: usize,
}

impl FormState {
    pub fn new(title: &'static str, help: &'static str, fields: Vec<FormField>) -> Self {
        Self {
            title,
            help,
            fields,
            selected: 0,
        }
    }

    pub fn active_mut(&mut self) -> &mut FormField {
        &mut self.fields[self.selected]
    }

    pub fn next(&mut self) {
        self.selected = (self.selected + 1) % self.fields.len();
    }

    pub fn previous(&mut self) {
        if self.selected == 0 {
            self.selected = self.fields.len() - 1;
        } else {
            self.selected -= 1;
        }
    }
}

#[derive(Debug, Clone)]
pub enum Modal {
    AddMcp(FormState),
    CreateSkill(FormState),

    EditBehavior(FormState),
    Settings(FormState),
}

impl Modal {
    pub fn add_mcp() -> Self {
        Self::AddMcp(FormState::new(
            "Add MCP Server",
            "Tab moves fields. transport is http or stdio. args/env use comma-separated values.",
            vec![
                FormField::new("transport", "http"),
                FormField::new("name", ""),
                FormField::new("url", ""),
                FormField::secret("auth_token", ""),
                FormField::new("command", ""),
                FormField::new("args", ""),
                FormField::new("env", ""),
            ],
        ))
    }

    pub fn create_skill() -> Self {
        Self::CreateSkill(FormState::new(
            "Create Skill",
            "Name becomes the directory. Description fills the template front matter.",
            vec![
                FormField::new("name", ""),
                FormField::new("description", ""),
            ],
        ))
    }

    pub fn edit_behavior(field: &str, value: &str) -> Self {
        Self::EditBehavior(FormState::new(
            "Edit Behavior",
            "Enter saves. Booleans accept true/false.",
            vec![
                FormField::new("field", field),
                FormField::new("value", value),
            ],
        ))
    }

    pub fn settings(theme: &str, simple_mode: bool) -> Self {
        Self::Settings(FormState::new(
            "Settings",
            "Configure TUI. Theme options: opencode, high-contrast. Simple Mode merges panels on small screens (true/false).",
            vec![
                FormField::new("theme", theme),
                FormField::new("simple_mode", if simple_mode { "true" } else { "false" }),
            ],
        ))
    }

    pub fn form(&self) -> &FormState {
        match self {
            Self::AddMcp(form)
            | Self::CreateSkill(form)
            | Self::EditBehavior(form)
            | Self::Settings(form) => form,
        }
    }

    pub fn form_mut(&mut self) -> &mut FormState {
        match self {
            Self::AddMcp(form)
            | Self::CreateSkill(form)
            | Self::EditBehavior(form)
            | Self::Settings(form) => form,
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.form_mut().active_mut().value.push(ch);
    }

    pub fn backspace(&mut self) {
        self.form_mut().active_mut().value.pop();
    }

    pub fn next_field(&mut self) {
        self.form_mut().next();
    }

    pub fn previous_field(&mut self) {
        self.form_mut().previous();
    }

    pub fn title(&self) -> &'static str {
        self.form().title
    }

    pub fn help(&self) -> &'static str {
        self.form().help
    }
}

#[derive(Debug, Clone)]
pub struct SubmittedMcpForm {
    pub transport: McpTransportKind,
    pub name: String,
    pub url: Option<String>,
    pub auth_token: Option<String>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: std::collections::HashMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_value_non_secret() {
        let field_empty = FormField::new("label", "");
        assert_eq!(field_empty.display_value(), "");

        let field_value = FormField::new("label", "hello");
        assert_eq!(field_value.display_value(), "hello");
    }

    #[test]
    fn test_display_value_secret_empty() {
        let field = FormField::secret("label", "");
        assert_eq!(field.display_value(), "");
    }

    #[test]
    fn test_display_value_secret_short() {
        let field = FormField::secret("label", "secret123");
        assert_eq!(field.display_value(), "*********"); // length is 9
    }

    #[test]
    fn test_display_value_secret_exact_max() {
        let field = FormField::secret("label", "1234567890123456");
        assert_eq!(field.display_value(), "****************"); // length is 16
    }

    #[test]
    fn test_display_value_secret_long() {
        let field = FormField::secret("label", "12345678901234567890");
        assert_eq!(field.display_value(), "****************"); // max length is 16
    }

    #[test]
    fn test_display_value_secret_unicode() {
        let field = FormField::secret("label", "🍎🍊🍇");
        assert_eq!(field.display_value(), "***"); // 3 unicode characters

        let field_long = FormField::secret("label", "🍎🍊🍇🍎🍊🍇🍎🍊🍇🍎🍊🍇🍎🍊🍇🍎🍊🍇"); // 18 chars
        assert_eq!(field_long.display_value(), "****************"); // max length is 16
    }

    #[test]
    fn test_form_field_new() {
        let field = FormField::new("username", "alice");
        assert_eq!(field.label, "username");
        assert_eq!(field.value, "alice");
        assert!(!field.secret);
    }

    #[test]
    fn test_form_field_secret() {
        let field = FormField::secret("password", "12345");
        assert_eq!(field.label, "password");
        assert_eq!(field.value, "12345");
        assert!(field.secret);
    }

    #[test]
    fn test_form_state_navigation() {
        let fields = vec![
            FormField::new("field1", "val1"),
            FormField::new("field2", "val2"),
            FormField::new("field3", "val3"),
        ];
        let mut state = FormState::new("Title", "Help", fields);

        assert_eq!(state.selected, 0);
        assert_eq!(state.active_mut().label, "field1");

        state.next();
        assert_eq!(state.selected, 1);
        assert_eq!(state.active_mut().label, "field2");

        state.next();
        assert_eq!(state.selected, 2);
        assert_eq!(state.active_mut().label, "field3");

        state.next();
        assert_eq!(state.selected, 0); // Wrap around

        state.previous();
        assert_eq!(state.selected, 2); // Wrap around back

        state.previous();
        assert_eq!(state.selected, 1);
    }

    #[test]
    fn test_form_state_active_mut() {
        let fields = vec![FormField::new("field1", "val1")];
        let mut state = FormState::new("Title", "Help", fields);

        state.active_mut().value = "new_val".to_string();
        assert_eq!(state.fields[0].value, "new_val");
    }
}
