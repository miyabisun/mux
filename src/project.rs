use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub(crate) struct ProjectDocument {
    pub(crate) project: Project,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Project {
    pub(crate) name: String,
    pub(crate) root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) startup_window: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) startup_pane: Option<usize>,
    pub(crate) windows: Vec<Window>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Window {
    pub(crate) name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) layout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) focused_pane: Option<usize>,
    pub(crate) panes: Vec<String>,
}

impl ProjectDocument {
    pub(crate) fn from_project(project: Project) -> Result<Self> {
        validate(&project)?;
        Ok(Self { project })
    }

    pub(crate) fn parse(source: &str) -> Result<Self> {
        let project: Project = toml::from_str(source).context("project TOML is invalid")?;
        validate(&project)?;
        Ok(Self { project })
    }

    pub(crate) fn to_toml(&self) -> Result<String> {
        let mut source = toml::to_string_pretty(&self.project)
            .context("cannot serialize project snapshot as TOML")?;
        if !source.ends_with('\n') {
            source.push('\n');
        }
        Ok(source)
    }
}

pub(crate) fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        bail!("project name must contain only ASCII letters, digits, '_' or '-'");
    }
    Ok(())
}

fn validate(project: &Project) -> Result<()> {
    validate_project_name(&project.name).context("invalid TOML name")?;
    if project.root.is_empty() {
        bail!("root must not be empty");
    }
    if project.windows.is_empty() {
        bail!("windows must contain at least one window");
    }

    let mut names = HashSet::new();
    for (position, window) in project.windows.iter().enumerate() {
        let name = &window.name;
        if name.is_empty() {
            bail!("windows[{}] has an empty name", position + 1);
        }
        if !names.insert(name.as_str()) {
            bail!("duplicate window name '{name}'");
        }
        if window.panes.is_empty() {
            bail!("window '{name}' must contain at least one pane");
        }
        if window.root.as_deref() == Some("") {
            bail!("window '{name}' root must not be empty");
        }
        if let Some(pane) = window.focused_pane {
            validate_pane_reference(name, "focused_pane", pane, window.panes.len())?;
        }
        if let Some(layout) = &window.layout {
            validate_layout(layout)
                .with_context(|| format!("window '{name}' layout is invalid"))?;
        }
    }

    let startup_name = project
        .startup_window
        .as_deref()
        .unwrap_or(&project.windows[0].name);
    let startup = project
        .windows
        .iter()
        .find(|window| window.name == startup_name)
        .with_context(|| format!("startup_window '{startup_name}' does not exist"))?;
    if let Some(pane) = project.startup_pane {
        validate_pane_reference(startup_name, "startup_pane", pane, startup.panes.len())?;
    }
    Ok(())
}

fn validate_pane_reference(window: &str, field: &str, pane: usize, count: usize) -> Result<()> {
    if pane == 0 || pane > count {
        bail!(
            "{field} {pane} in window '{window}' is out of range; pane positions are 1-based and this window has {count} pane(s)"
        );
    }
    Ok(())
}

fn validate_layout(layout: &str) -> Result<()> {
    let (checksum, body) = layout
        .split_once(',')
        .context("layout must start with a four-digit checksum")?;
    if checksum.len() != 4 || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("layout checksum must be four hexadecimal digits");
    }
    let expected = u16::from_str_radix(checksum, 16)?;
    if layout_checksum(body.as_bytes()) != expected {
        bail!("layout checksum does not match its body");
    }
    let mut parser = LayoutParser::new(body);
    parser.cell()?;
    if !parser.is_done() {
        bail!("unexpected data at byte {}", parser.position);
    }
    Ok(())
}

pub(crate) fn layout_checksum(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0_u16, |checksum, byte| {
        checksum.rotate_right(1).wrapping_add(u16::from(*byte))
    })
}

struct LayoutParser<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> LayoutParser<'a> {
    const fn new(value: &'a str) -> Self {
        Self {
            bytes: value.as_bytes(),
            position: 0,
        }
    }

    fn cell(&mut self) -> Result<()> {
        self.number("width")?;
        self.expect(b'x')?;
        self.number("height")?;
        self.expect(b',')?;
        self.number("x offset")?;
        self.expect(b',')?;
        self.number("y offset")?;

        if self.peek() == Some(b',') {
            self.position += 1;
            self.number("pane id")?;
        }
        if matches!(self.peek(), Some(b'{' | b'[')) {
            let open = self.peek().expect("matched");
            let close = if open == b'{' { b'}' } else { b']' };
            self.position += 1;
            self.cell()?;
            while self.peek() == Some(b',') {
                self.position += 1;
                self.cell()?;
            }
            self.expect(close)?;
        }
        Ok(())
    }

    fn number(&mut self, label: &str) -> Result<()> {
        let start = self.position;
        while self.peek().is_some_and(|byte| byte.is_ascii_digit()) {
            self.position += 1;
        }
        if self.position == start {
            bail!("expected {label} at byte {start}");
        }
        Ok(())
    }

    fn expect(&mut self, expected: u8) -> Result<()> {
        if self.peek() != Some(expected) {
            bail!(
                "expected '{}' at byte {}",
                char::from(expected),
                self.position
            );
        }
        self.position += 1;
        Ok(())
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    const fn is_done(&self) -> bool {
        self.position == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
name = "settings"
root = "/tmp"
startup_window = "agent"
startup_pane = 2

[[windows]]
name = "agent"
layout = "020a,80x24,0,0{40x24,0,0,1,39x24,41,0,2}"
focused_pane = 1
panes = ["claude", ""]
"#;

    #[test]
    fn strict_project_is_validated() {
        ProjectDocument::parse(VALID).unwrap();
        let unknown = VALID.replace("root = \"/tmp\"", "root = \"/tmp\"\nunknown = true");
        assert!(ProjectDocument::parse(&unknown).is_err());
        let bad_reference = VALID.replace("startup_pane = 2", "startup_pane = 3");
        assert!(ProjectDocument::parse(&bad_reference).is_err());
        let missing_window =
            VALID.replace("startup_window = \"agent\"", "startup_window = \"missing\"");
        assert!(ProjectDocument::parse(&missing_window).is_err());
        let bad_focus = VALID.replace("focused_pane = 1", "focused_pane = 3");
        assert!(ProjectDocument::parse(&bad_focus).is_err());
        let wrong_type = VALID.replace("startup_pane = 2", "startup_pane = \"two\"");
        assert!(ProjectDocument::parse(&wrong_type).is_err());
        let unknown_window = VALID.replace(
            "focused_pane = 1",
            "focused_pane = 1\nunknown_window_key = true",
        );
        assert!(ProjectDocument::parse(&unknown_window).is_err());
        let empty_panes = VALID.replace("panes = [\"claude\", \"\"]", "panes = []");
        assert!(ProjectDocument::parse(&empty_panes).is_err());
        let empty_window_name =
            VALID.replace("[[windows]]\nname = \"agent\"", "[[windows]]\nname = \"\"");
        assert!(ProjectDocument::parse(&empty_window_name).is_err());
        let duplicate_window = format!("{VALID}\n[[windows]]\nname = \"agent\"\npanes = [\"\"]\n");
        assert!(ProjectDocument::parse(&duplicate_window).is_err());
    }

    #[test]
    fn layout_checksum_and_shape_are_checked() {
        validate_layout("020a,80x24,0,0{40x24,0,0,1,39x24,41,0,2}").unwrap();
        assert!(validate_layout("0000,80x24,0,0").is_err());
        assert!(validate_layout("7d58,80x24,0").is_err());
    }

    #[test]
    fn names_cannot_escape_the_config_directory() {
        for invalid in ["", ".", "../x", "a/b", "a.b", "名前"] {
            assert!(validate_project_name(invalid).is_err(), "{invalid}");
        }
        for valid in ["a", "my-project", "my_project", "A1"] {
            validate_project_name(valid).unwrap();
        }
    }
}
