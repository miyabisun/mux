use std::{env, ffi::OsString, process::Command};

use anyhow::{Context, Result, bail};

use crate::project::{Project, Window};

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
        let first_root = first_window.root.as_deref().unwrap_or(&project.root);
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
            OsString::from(first_root),
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
        let mut windows = vec![self.populate_window(&first_id, first_window, first_root)?];

        for window in project.windows.iter().skip(1) {
            let name = &window.name;
            let root = window.root.as_deref().unwrap_or(&project.root);
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
                OsString::from(root),
            ];
            let id = self.single_output(arguments)?;
            windows.push(self.populate_window(&id, window, root)?);
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
        root: &str,
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
                OsString::from(root),
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

    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(&self.leading_args);
        if self.remove_tmux_environment {
            command.env_remove("TMUX");
        }
        command
    }
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
            editor_root.display()
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
