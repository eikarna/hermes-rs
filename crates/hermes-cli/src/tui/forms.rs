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
    fn test_form_field_display_value() {
        // Normal field
        let field = FormField::new("username", "alice");
        assert_eq!(field.display_value(), "alice");

        // Secret field, empty
        let secret_empty = FormField::secret("password", "");
        assert_eq!(secret_empty.display_value(), "");

        // Secret field, short
        let secret_short = FormField::secret("password", "12345");
        assert_eq!(secret_short.display_value(), "*****");

        // Secret field, long (limit to 16)
        let secret_long = FormField::secret(
            "password",
            "this_is_a_very_long_password_that_should_be_capped",
        );
        assert_eq!(secret_long.display_value(), "*".repeat(16));
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
