use std::fs;
use std::path::{Path, PathBuf};

pub struct SyntheticWorkspace {
    pub root: PathBuf,
}

impl SyntheticWorkspace {
    /// Generates a dynamic workspace with an intentional bug.
    pub fn generate_trap_workspace(base_dir: &Path) -> std::io::Result<Self> {
        let root = base_dir.join("test_env");
        fs::create_dir_all(&root)?;

        // Generate a tricky bug: shadowed variable in a deep module
        let code = r#"
pub fn calculate_total(items: &[i32], discount: i32) -> i32 {
    let mut total = items.iter().sum();
    if discount > 0 {
        // TRAP: shadows the outer 'total' instead of mutating it
        let total = total - discount; 
    }
    total
}
"#;
        fs::write(root.join("calc.rs"), code)?;

        Ok(Self { root })
    }

    pub fn verify_fix(&self) -> bool {
        let code = fs::read_to_string(self.root.join("calc.rs")).unwrap_or_default();
        // Simple assertion: check if the shadowing 'let' was removed
        !code.contains("let total = total - discount;")
            && (code.contains("total -= discount;") || code.contains("total = total - discount;"))
    }
}
