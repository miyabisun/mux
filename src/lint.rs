use std::{env, path::PathBuf};

use crate::project::{ProjectDocument, resolve_window_root};

const ALLOWED_COMMANDS: [&str; 4] = ["claude", "codex", "cursor-agent", "nvim"];

pub(crate) fn is_allowed_command(command: &str) -> bool {
    ALLOWED_COMMANDS.contains(&command)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Warning {
    message: String,
}

impl std::fmt::Display for Warning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[must_use]
pub(crate) fn lint_project(document: &ProjectDocument) -> Vec<Warning> {
    let mut warnings = Vec::new();
    let project_root = expand_home(&document.project.root);
    lint_path("project", &project_root, &mut warnings);
    for window in &document.project.windows {
        let name = &window.name;
        if let Some(root) = &window.root {
            let root = PathBuf::from(root);
            let root = if root.is_absolute() {
                root
            } else {
                resolve_window_root(&project_root, Some(root.to_string_lossy().as_ref()))
            };
            lint_path(&format!("window '{name}'"), &root, &mut warnings);
        }
        for (index, command) in window.panes.iter().enumerate() {
            lint_command(name, index + 1, command, &mut warnings);
        }
    }
    warnings
}

fn lint_path(owner: &str, path: &std::path::Path, warnings: &mut Vec<Warning>) {
    if !path.is_dir() {
        warnings.push(warning(format!(
            "{owner} root '{}' is not an existing directory",
            path.display()
        )));
    }
}

fn expand_home(root: &str) -> PathBuf {
    if root == "~" {
        return env::var_os("HOME").map_or_else(|| PathBuf::from(root), PathBuf::from);
    }
    if let Some(suffix) = root.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(suffix);
    }
    PathBuf::from(root)
}

fn lint_command(window: &str, pane: usize, command: &str, warnings: &mut Vec<Warning>) {
    if command.is_empty() {
        return;
    }
    let words: Vec<_> = command.split_whitespace().collect();
    let executable = words.first().copied().unwrap_or_default();
    if !is_allowed_command(executable) {
        warnings.push(warning(format!(
            "window '{window}' pane {pane} command '{executable}' is not allowlisted"
        )));
    }
    if words.len() > 1 {
        warnings.push(warning(format!(
            "window '{window}' pane {pane} command has arguments"
        )));
    }
    if command.contains("://") || words.iter().any(|word| looks_like_host(word)) {
        warnings.push(warning(format!(
            "window '{window}' pane {pane} command looks like it contains a hostname or URL"
        )));
    }
    if words.iter().any(|word| looks_like_assignment(word)) || command.contains("${") {
        warnings.push(warning(format!(
            "window '{window}' pane {pane} command looks like it assigns or expands an environment variable"
        )));
    }
}

fn looks_like_host(word: &str) -> bool {
    let word = word
        .trim_matches(|character: char| matches!(character, '\'' | '"' | ',' | ';' | '(' | ')'));
    let labels: Vec<_> = word.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

fn looks_like_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
        })
}

fn warning(message: String) -> Warning {
    Warning { message }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn command_risks_are_reported() {
        let document = ProjectDocument::parse(
            r#"
name = "x"
root = "/tmp"

[[windows]]
name = "one"
panes = ["claude --resume", "curl https://example.com", "FOO=secret nvim"]
"#,
        )
        .unwrap();
        let text = lint_project(&document)
            .into_iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("has arguments"));
        assert!(text.contains("not allowlisted"));
        assert!(text.contains("hostname or URL"));
        assert!(text.contains("environment variable"));
    }

    #[test]
    fn missing_root_is_reported() {
        let document = ProjectDocument::parse(
            "name = \"x\"\nroot = \"/definitely/missing/mux-test\"\n[[windows]]\nname = \"one\"\npanes = [\"\"]\n",
        )
        .unwrap();
        let warnings = lint_project(&document);
        assert!(
            warnings
                .iter()
                .any(|item| item.to_string().contains("not an existing"))
        );
    }

    #[test]
    fn relative_window_roots_are_checked_under_the_project_root() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("src")).unwrap();
        let source = format!(
            "name = \"x\"\nroot = \"{}\"\n[[windows]]\nname = \"one\"\nroot = \"src\"\npanes = [\"\"]\n",
            temp.path().display()
        );
        let document = ProjectDocument::parse(&source).unwrap();
        assert!(lint_project(&document).is_empty());

        let missing = source.replace("root = \"src\"", "root = \"missing\"");
        let warnings = lint_project(&ProjectDocument::parse(&missing).unwrap());
        assert!(warnings[0].to_string().contains("missing"));
    }
}
