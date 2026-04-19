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

    pub fn form(&self) -> &FormState {
        match self {
            Self::AddMcp(form) | Self::CreateSkill(form) | Self::EditBehavior(form) => form,
        }
    }

    pub fn form_mut(&mut self) -> &mut FormState {
        match self {
            Self::AddMcp(form) | Self::CreateSkill(form) | Self::EditBehavior(form) => form,
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
