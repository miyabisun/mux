use std::{collections::HashSet, env, ffi::OsString, path::Path, process::Command};

use anyhow::{Context, Result, bail};

use crate::{
    lint::is_allowed_command,
    project::{Project, ProjectDocument, Window, validate_project_name},
};

pub(crate) struct Snapshot {
    pub(crate) document: ProjectDocument,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn snapshot() -> Result<Snapshot> {
    let pane = env::var("TMUX_PANE").context("mux snapshot must be run inside tmux")?;
    if pane.is_empty() {
        bail!("mux snapshot must be run inside tmux");
    }
    Tmux::default().snapshot(&pane)
}

pub(crate) fn open(project: &Project) -> Result<()> {
    let client = Tmux::default();
    if !client.has_session(&project.name)? {
        client.create_session(project)?;
    }
    match client_action(env::var_os("TMUX").is_some()) {
        ClientAction::Switch => client.run_interactive([
            OsString::from("switch-client"),
            OsString::from("-t"),
            OsString::from(format!("={}", project.name)),
        ]),
        ClientAction::Attach => client.run_interactive([
            OsString::from("attach-session"),
            OsString::from("-t"),
            OsString::from(format!("={}", project.name)),
        ]),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientAction {
    Attach,
    Switch,
}

const fn client_action(in_tmux: bool) -> ClientAction {
    if in_tmux {
        ClientAction::Switch
    } else {
        ClientAction::Attach
    }
}

#[derive(Debug, Default)]
struct Tmux {
    leading_args: Vec<OsString>,
    remove_tmux_environment: bool,
}

impl Tmux {
    fn snapshot(&self, pane_target: &str) -> Result<Snapshot> {
        let session_id = self.display_value(pane_target, "#{session_id}")?;
        let raw_session_name = self.display_value(&session_id, "#{session_name}")?;
        let project_name = normalize_project_name(&raw_session_name)?;
        let mut warnings = Vec::new();
        if project_name != raw_session_name {
            warnings.push(format!(
                "session name {raw_session_name:?} was normalized to {project_name:?}"
            ));
        }

        let window_ids = self.identifier_list([
            OsString::from("list-windows"),
            OsString::from("-t"),
            OsString::from(&session_id),
            OsString::from("-F"),
            OsString::from("#{window_id}"),
        ])?;
        let active_window_id = self.display_value(&session_id, "#{window_id}")?;
        let default_shell = self.display_value(&session_id, "#{default-shell}")?;
        let default_shell = Path::new(&default_shell)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();

        let mut used_window_names = HashSet::new();
        let mut project_root = None;
        let mut windows = Vec::with_capacity(window_ids.len());
        let mut startup_window = None;
        let mut startup_pane = None;

        for window_id in window_ids {
            let window = self.snapshot_window(
                &window_id,
                default_shell,
                &mut used_window_names,
                &mut project_root,
                &mut warnings,
            )?;

            if window_id == active_window_id {
                startup_window = Some(window.name.clone());
                startup_pane = window.focused_pane;
            }
            windows.push(window);
        }

        let root = project_root.context("tmux session has no windows")?;
        let startup_window = startup_window.context("tmux session has no active window")?;
        let document = ProjectDocument::from_project(Project {
            name: project_name,
            root,
            startup_window: Some(startup_window),
            startup_pane,
            windows,
        })?;
        Ok(Snapshot { document, warnings })
    }

    fn snapshot_window(
        &self,
        window_id: &str,
        default_shell: &str,
        used_window_names: &mut HashSet<String>,
        project_root: &mut Option<String>,
        warnings: &mut Vec<String>,
    ) -> Result<Window> {
        let raw_name = self.display_value(window_id, "#{window_name}")?;
        let name = unique_window_name(&raw_name, used_window_names);
        if name != raw_name {
            warnings.push(format!(
                "window name {raw_name:?} was changed to {name:?} to keep names unique"
            ));
        }
        let layout = self.display_value(window_id, "#{window_layout}")?;
        let pane_ids = self.identifier_list([
            OsString::from("list-panes"),
            OsString::from("-t"),
            OsString::from(window_id),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
        ])?;
        let active_pane_id = self.display_value(window_id, "#{pane_id}")?;
        let focused_pane = pane_ids
            .iter()
            .position(|pane_id| pane_id == &active_pane_id)
            .map(|position| position + 1)
            .with_context(|| format!("active pane is missing from window '{raw_name}'"))?;

        let mut paths = Vec::with_capacity(pane_ids.len());
        let mut commands = Vec::with_capacity(pane_ids.len());
        for pane_id in &pane_ids {
            paths.push(self.display_value(pane_id, "#{pane_current_path}")?);
            commands.push(self.display_value(pane_id, "#{pane_current_command}")?);
        }
        let window_root = paths
            .first()
            .filter(|path| !path.is_empty())
            .cloned()
            .with_context(|| format!("window '{raw_name}' has no usable pane path"))?;
        let root = project_root.get_or_insert_with(|| window_root.clone());
        let panes = paths
            .iter()
            .zip(commands.iter())
            .enumerate()
            .map(|(position, (path, command))| {
                snapshot_pane_command(
                    &name,
                    position + 1,
                    path,
                    &window_root,
                    command,
                    default_shell,
                    warnings,
                )
            })
            .collect();

        Ok(Window {
            name,
            layout: Some(layout),
            root: snapshot_window_root(root, &window_root),
            focused_pane: Some(focused_pane),
            panes,
        })
    }

    fn has_session(&self, name: &str) -> Result<bool> {
        let output = self
            .command()
            .args(["has-session", "-t", &format!("={name}")])
            .output()
            .context("cannot run tmux; install tmux before launching projects")?;
        Ok(output.status.success())
    }

    fn create_session(&self, project: &Project) -> Result<()> {
        let mut created_session = None;
        let result = self.create_session_inner(project, &mut created_session);
        if let (Err(_), Some(session_id)) = (&result, created_session) {
            let _ = self
                .command()
                .args(["kill-session", "-t", &session_id])
                .status();
        }
        result
    }

    fn create_session_inner(
        &self,
        project: &Project,
        created_session: &mut Option<String>,
    ) -> Result<()> {
        let first_window = &project.windows[0];
        let first_name = &first_window.name;
        let first_root = project.resolved_window_root(first_window)?;
        let arguments = [
            OsString::from("new-session"),
            OsString::from("-d"),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{session_id}"),
            OsString::from("-s"),
            OsString::from(&project.name),
            OsString::from("-n"),
            OsString::from(first_name),
            OsString::from("-c"),
            OsString::from(first_root.as_os_str()),
        ];
        let session_id = self.single_output(arguments)?;
        *created_session = Some(session_id.clone());

        let first_id = self.single_output([
            OsString::from("list-windows"),
            OsString::from("-t"),
            OsString::from(&session_id),
            OsString::from("-F"),
            OsString::from("#{window_id}"),
        ])?;
        let mut windows = vec![self.populate_window(&first_id, first_window, &first_root)?];

        for window in project.windows.iter().skip(1) {
            let name = &window.name;
            let root = project.resolved_window_root(window)?;
            let arguments = [
                OsString::from("new-window"),
                OsString::from("-d"),
                OsString::from("-P"),
                OsString::from("-F"),
                OsString::from("#{window_id}"),
                OsString::from("-t"),
                OsString::from(&session_id),
                OsString::from("-n"),
                OsString::from(name),
                OsString::from("-c"),
                OsString::from(root.as_os_str()),
            ];
            let id = self.single_output(arguments)?;
            windows.push(self.populate_window(&id, window, &root)?);
        }

        let startup_window_name = project
            .startup_window
            .as_deref()
            .unwrap_or(first_name.as_str());
        let startup_index = project
            .windows
            .iter()
            .position(|window| window.name == startup_window_name)
            .expect("validated startup window");
        let startup = &windows[startup_index];
        self.run_checked([
            OsString::from("select-window"),
            OsString::from("-t"),
            OsString::from(&startup.window_id),
        ])?;
        if let Some(position) = project.startup_pane {
            self.select_pane(&startup.pane_ids[position - 1])?;
        }
        Ok(())
    }

    fn populate_window(
        &self,
        window_id: &str,
        window: &Window,
        root: &Path,
    ) -> Result<CreatedWindow> {
        let mut pane_ids = vec![self.single_output([
            OsString::from("list-panes"),
            OsString::from("-t"),
            OsString::from(window_id),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
        ])?];
        self.send_command(&pane_ids[0], &window.panes[0])?;
        for command in window.panes.iter().skip(1) {
            let arguments = [
                OsString::from("split-window"),
                OsString::from("-d"),
                OsString::from("-P"),
                OsString::from("-F"),
                OsString::from("#{pane_id}"),
                OsString::from("-t"),
                OsString::from(window_id),
                OsString::from("-c"),
                OsString::from(root.as_os_str()),
            ];
            let pane_id = self.single_output(arguments)?;
            self.send_command(&pane_id, command)?;
            pane_ids.push(pane_id);
        }
        if let Some(layout) = &window.layout {
            self.run_checked([
                OsString::from("select-layout"),
                OsString::from("-t"),
                OsString::from(window_id),
                OsString::from(layout),
            ])?;
        }
        if let Some(position) = window.focused_pane {
            self.select_pane(&pane_ids[position - 1])?;
        }
        Ok(CreatedWindow {
            window_id: window_id.to_owned(),
            pane_ids,
        })
    }

    fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.run_checked([
            OsString::from("select-pane"),
            OsString::from("-t"),
            OsString::from(pane_id),
        ])
    }

    fn send_command(&self, pane_id: &str, command: &str) -> Result<()> {
        if command.is_empty() {
            return Ok(());
        }
        self.run_checked([
            OsString::from("send-keys"),
            OsString::from("-l"),
            OsString::from("-t"),
            OsString::from(pane_id),
            OsString::from("--"),
            OsString::from(command),
        ])?;
        self.run_checked([
            OsString::from("send-keys"),
            OsString::from("-t"),
            OsString::from(pane_id),
            OsString::from("Enter"),
        ])
    }

    fn run_interactive<I>(&self, arguments: I) -> Result<()>
    where
        I: IntoIterator<Item = OsString>,
    {
        let status = self
            .command()
            .args(arguments)
            .status()
            .context("cannot run tmux; install tmux before launching projects")?;
        if !status.success() {
            bail!("tmux client exited with {status}");
        }
        Ok(())
    }

    fn run_checked<I>(&self, arguments: I) -> Result<()>
    where
        I: IntoIterator<Item = OsString>,
    {
        let output = self
            .command()
            .args(arguments)
            .output()
            .context("cannot run tmux; install tmux before launching projects")?;
        if !output.status.success() {
            bail!(
                "tmux failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(())
    }

    fn single_output<I>(&self, arguments: I) -> Result<String>
    where
        I: IntoIterator<Item = OsString>,
    {
        let output = self
            .command()
            .args(arguments)
            .output()
            .context("cannot run tmux; install tmux before launching projects")?;
        if !output.status.success() {
            bail!(
                "tmux failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = String::from_utf8(output.stdout).context("tmux returned non-UTF-8 output")?;
        let mut lines = text.lines();
        let value = lines.next().context("tmux returned no identifier")?;
        if lines.next().is_some() || value.is_empty() {
            bail!("tmux returned an unexpected identifier list");
        }
        Ok(value.to_owned())
    }

    fn identifier_list<I>(&self, arguments: I) -> Result<Vec<String>>
    where
        I: IntoIterator<Item = OsString>,
    {
        let output = self
            .command()
            .args(arguments)
            .output()
            .context("cannot run tmux; install tmux before taking a snapshot")?;
        if !output.status.success() {
            bail!(
                "tmux failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = String::from_utf8(output.stdout).context("tmux returned non-UTF-8 output")?;
        let values: Vec<_> = text.lines().map(str::to_owned).collect();
        if values.is_empty() || values.iter().any(String::is_empty) {
            bail!("tmux returned an unexpected identifier list");
        }
        Ok(values)
    }

    fn display_value(&self, target: &str, format: &str) -> Result<String> {
        let output = self
            .command()
            .args(["display-message", "-p", "-t", target, format])
            .output()
            .context("cannot run tmux; install tmux before taking a snapshot")?;
        if !output.status.success() {
            bail!(
                "tmux failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let mut bytes = output.stdout;
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
        }
        String::from_utf8(bytes).context("tmux returned non-UTF-8 output")
    }

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(&self.leading_args);
        if self.remove_tmux_environment {
            command.env_remove("TMUX");
        }
        command
    }
}

fn snapshot_window_root(project_root: &str, window_root: &str) -> Option<String> {
    if window_root == project_root {
        return None;
    }
    Path::new(window_root)
        .strip_prefix(project_root)
        .ok()
        .filter(|root| !root.as_os_str().is_empty())
        .map_or_else(
            || Some(window_root.to_owned()),
            |root| Some(root.to_string_lossy().into_owned()),
        )
}

fn normalize_project_name(name: &str) -> Result<String> {
    let mut normalized = String::new();
    let mut replacing = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            normalized.push(character);
            replacing = false;
        } else if !replacing {
            normalized.push('-');
            replacing = true;
        }
    }
    if !normalized.bytes().any(|byte| byte.is_ascii_alphanumeric()) {
        bail!(
            "tmux session name {name:?} cannot be converted to a meaningful mux project name; rename the session"
        );
    }
    validate_project_name(&normalized)?;
    Ok(normalized)
}

fn unique_window_name(name: &str, used: &mut HashSet<String>) -> String {
    let base = if name.is_empty() { "window" } else { name };
    if used.insert(base.to_owned()) {
        return base.to_owned();
    }
    for suffix in 2_usize.. {
        let candidate = format!("{base}-{suffix}");
        if used.insert(candidate.clone()) {
            return candidate;
        }
    }
    unreachable!("an unused window suffix always exists")
}

fn is_shell_command(command: &str, default_shell: &str) -> bool {
    command == default_shell
        || matches!(
            command,
            "sh" | "bash" | "dash" | "zsh" | "fish" | "ksh" | "csh" | "tcsh" | "nu"
        )
}

fn snapshot_pane_command(
    window_name: &str,
    position: usize,
    path: &str,
    window_root: &str,
    command: &str,
    default_shell: &str,
    warnings: &mut Vec<String>,
) -> String {
    if path != window_root {
        warnings.push(format!(
            "window {window_name:?} pane {position} cwd differs from its window root; its command was omitted"
        ));
        return String::new();
    }
    if is_allowed_command(command) {
        return command.to_owned();
    }
    if !command.is_empty() && !is_shell_command(command, default_shell) {
        warnings.push(format!(
            "window {window_name:?} pane {position} command {command:?} was omitted"
        ));
    }
    String::new()
}

#[derive(Debug)]
struct CreatedWindow {
    window_id: String,
    pane_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration, time::SystemTime};

    use super::*;
    use crate::project::ProjectDocument;

    #[test]
    fn client_mode_depends_only_on_tmux_environment() {
        assert_eq!(client_action(false), ClientAction::Attach);
        assert_eq!(client_action(true), ClientAction::Switch);
    }

    #[test]
    fn snapshot_names_are_safe_and_deterministic() {
        assert_eq!(
            normalize_project_name("my session.1").unwrap(),
            "my-session-1"
        );
        assert!(normalize_project_name("日本語___").is_err());

        let mut used = HashSet::new();
        assert_eq!(unique_window_name("shell", &mut used), "shell");
        assert_eq!(unique_window_name("shell", &mut used), "shell-2");
        assert_eq!(unique_window_name("shell-2", &mut used), "shell-2-2");
        assert_eq!(unique_window_name("", &mut used), "window");
    }

    #[test]
    fn snapshot_only_keeps_allowlisted_non_shell_commands() {
        assert!(is_allowed_command("claude"));
        assert!(is_allowed_command("codex"));
        assert!(!is_allowed_command("node"));
        assert!(is_shell_command("zsh", "zsh"));
        assert!(is_shell_command("bash", "zsh"));
        assert!(!is_shell_command("node", "zsh"));
    }

    #[test]
    fn snapshot_window_roots_are_portable_inside_the_project() {
        assert_eq!(snapshot_window_root("/work/app", "/work/app"), None);
        assert_eq!(
            snapshot_window_root("/work/app", "/work/app/src/api"),
            Some("src/api".to_owned())
        );
        assert_eq!(
            snapshot_window_root("/work/app", "/work/other"),
            Some("/work/other".to_owned())
        );
    }

    #[test]
    #[ignore = "requires a real tmux binary"]
    fn real_tmux_session_is_built_without_user_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let editor_root = root.join("editor");
        fs::create_dir(&editor_root).unwrap();
        let marker = root.join("command-ran");
        let socket = format!(
            "mux-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let layout_body = "80x24,0,0{40x24,0,0,1,39x24,41,0,2}";
        let checksum = super::super::project::layout_checksum(layout_body.as_bytes());
        let source = format!(
            "name = \"mux-e2e\"\nroot = \"{}\"\nstartup_window = \"agent\"\nstartup_pane = 2\n\n[[windows]]\nname = \"agent\"\nlayout = \"{checksum:04x},{layout_body}\"\nfocused_pane = 1\npanes = [\"printf ok > {}\", \"\"]\n\n[[windows]]\nname = \"editor\"\nroot = \"{}\"\nfocused_pane = 2\npanes = [\"\", \"\"]\n",
            root.display(),
            marker.display(),
            "editor"
        );
        let project = ProjectDocument::parse(&source).unwrap().project;
        let client = Tmux {
            leading_args: vec![
                "-L".into(),
                socket.clone().into(),
                "-f".into(),
                "/dev/null".into(),
            ],
            remove_tmux_environment: true,
        };

        client.create_session(&project).unwrap();
        for _ in 0..100 {
            if marker.is_file() {
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        assert_eq!(fs::read_to_string(&marker).unwrap(), "ok");
        let output = client
            .command()
            .args([
                "list-windows",
                "-t",
                "=mux-e2e",
                "-F",
                "#{window_name}:#{window_panes}:#{window_active}:#{pane_active}",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("agent:2:1:1"), "{text}");
        assert!(text.contains("editor:2:0:1"), "{text}");

        let panes = client
            .command()
            .args([
                "list-panes",
                "-t",
                "=mux-e2e:agent",
                "-F",
                "#{pane_current_path}",
            ])
            .output()
            .unwrap();
        assert!(panes.status.success());
        let expected_root = root.to_string_lossy();
        assert!(
            String::from_utf8(panes.stdout)
                .unwrap()
                .lines()
                .all(|path| path == expected_root)
        );

        for (window, expected_root) in [("agent", &root), ("editor", &editor_root)] {
            let panes = client
                .command()
                .args([
                    "list-panes",
                    "-t",
                    &format!("=mux-e2e:{window}"),
                    "-F",
                    "#{pane_index}:#{pane_active}:#{pane_current_path}",
                ])
                .output()
                .unwrap();
            assert!(panes.status.success());
            let text = String::from_utf8(panes.stdout).unwrap();
            let expected_root = expected_root.to_string_lossy();
            assert!(text.contains(&format!("0:0:{expected_root}")), "{text}");
            assert!(text.contains(&format!("1:1:{expected_root}")), "{text}");
        }

        let _ = client.command().args(["kill-server"]).status();
        let _ = fs::remove_dir_all(temp.path());
    }
}
