use std::{
    ffi::OsStr,
    fs,
    io::Write,
    path::Path,
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, SystemTime},
};

struct TmuxServer {
    socket: String,
}

impl TmuxServer {
    fn new() -> Self {
        Self {
            socket: format!(
                "mux-snapshot-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
        }
    }

    fn output<I, S>(&self, arguments: I) -> Output
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Command::new("tmux")
            .args(["-L", &self.socket, "-f", "/dev/null"])
            .args(arguments)
            .env_remove("TMUX")
            .output()
            .unwrap()
    }

    fn success<I, S>(&self, arguments: I) -> String
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.output(arguments);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .env_remove("TMUX")
            .status();
    }
}

struct SessionEvidence {
    first_layout: String,
    tmux_environment: String,
    tmux_pane: String,
}

fn create_windows(server: &TmuxServer, roots: [&Path; 3]) -> (String, String, String) {
    let first = server.success([
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{window_id}:#{pane_id}",
        "-s",
        "snapshot session",
        "-n",
        "same",
        "-c",
        roots[0].to_str().unwrap(),
    ]);
    let (first_window, first_pane) = first.split_once(':').unwrap();
    server.success([
        "set-window-option",
        "-t",
        first_window,
        "automatic-rename",
        "off",
    ]);
    let second_pane = server.success([
        "split-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        first_window,
        "-c",
        roots[2].to_str().unwrap(),
    ]);
    server.success(["select-pane", "-t", &second_pane]);

    let second = server.success([
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}:#{pane_id}",
        "-t",
        "=snapshot session",
        "-n",
        "same",
        "-c",
        roots[1].to_str().unwrap(),
    ]);
    let (second_window, _) = second.split_once(':').unwrap();
    server.success([
        "set-window-option",
        "-t",
        second_window,
        "automatic-rename",
        "off",
    ]);
    server.success(["rename-window", "-t", first_window, "same"]);
    server.success(["rename-window", "-t", second_window, "same"]);
    server.success(["select-window", "-t", second_window]);
    (first_window.to_owned(), first_pane.to_owned(), second_pane)
}

fn capture_session_evidence(
    server: &TmuxServer,
    temp: &Path,
    first_window: &str,
    first_pane: &str,
    second_pane: &str,
) -> SessionEvidence {
    let first_layout = server.success([
        "display-message",
        "-p",
        "-t",
        first_window,
        "#{window_layout}",
    ]);
    server.success(["resize-pane", "-Z", "-t", second_pane]);
    let zoomed_layout = server.success([
        "display-message",
        "-p",
        "-t",
        first_window,
        "#{window_layout}",
    ]);
    assert_eq!(zoomed_layout, first_layout);
    server.success(["resize-pane", "-Z", "-t", second_pane]);

    let env_path = temp.join("pane-environment");
    let command = format!(
        "printf '%s\\n%s\\n' \"$TMUX\" \"$TMUX_PANE\" > '{}'",
        env_path.display()
    );
    server.success(["send-keys", "-l", "-t", first_pane, "--", &command]);
    server.success(["send-keys", "-t", first_pane, "Enter"]);
    for _ in 0..100 {
        if env_path.is_file() {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    let environment = fs::read_to_string(&env_path).unwrap();
    let mut lines = environment.lines();
    let tmux_environment = lines.next().unwrap().to_owned();
    let tmux_pane = lines.next().unwrap().to_owned();
    assert!(lines.next().is_none());
    SessionEvidence {
        first_layout,
        tmux_environment,
        tmux_pane,
    }
}

fn assert_snapshot(source: &str, stderr: &[u8], roots: [&Path; 3], first_layout: &str) {
    let project: toml::Value = toml::from_str(source).unwrap();
    assert_eq!(project["name"].as_str(), Some("snapshot-session"));
    assert_eq!(project["root"].as_str(), roots[0].to_str());
    assert_eq!(project["startup_window"].as_str(), Some("same-2"));
    assert_eq!(project["startup_pane"].as_integer(), Some(1));
    let windows = project["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0]["name"].as_str(), Some("same"));
    assert_eq!(windows[0]["focused_pane"].as_integer(), Some(2));
    assert_eq!(windows[0]["layout"].as_str(), Some(first_layout));
    assert_eq!(windows[0]["panes"].as_array().unwrap().len(), 2);
    assert!(
        windows[0]["panes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|pane| pane.as_str() == Some(""))
    );
    assert!(windows[0].get("root").is_none());
    assert_eq!(windows[1]["name"].as_str(), Some("same-2"));
    assert_eq!(windows[1]["root"].as_str(), roots[1].to_str());
    let warnings = String::from_utf8_lossy(stderr);
    assert!(warnings.contains("keep names unique"), "{warnings}");
    assert!(warnings.contains("cwd differs"), "{warnings}");
}

fn save_and_check(source: &str, config: &Path) {
    let mut save = Command::new(env!("CARGO_BIN_EXE_mux"))
        .args(["save", "captured"])
        .env("MUX_CONFIG", config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    save.stdin
        .take()
        .unwrap()
        .write_all(source.as_bytes())
        .unwrap();
    let saved = save.wait_with_output().unwrap();
    assert!(
        saved.status.success(),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert!(
        Command::new(env!("CARGO_BIN_EXE_mux"))
            .args(["check", "captured"])
            .env("MUX_CONFIG", config)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
#[ignore = "requires a real tmux binary"]
fn snapshot_round_trips_through_the_real_tmux_socket() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("root");
    let other = temp.path().join("other");
    let different = temp.path().join("different");
    for path in [&root, &other, &different] {
        fs::create_dir(path).unwrap();
    }
    let roots = [root.as_path(), other.as_path(), different.as_path()];
    let server = TmuxServer::new();
    let (first_window, first_pane, second_pane) = create_windows(&server, roots);
    let evidence = capture_session_evidence(
        &server,
        temp.path(),
        &first_window,
        &first_pane,
        &second_pane,
    );

    let snapshot = Command::new(env!("CARGO_BIN_EXE_mux"))
        .arg("snapshot")
        .env("TMUX", &evidence.tmux_environment)
        .env("TMUX_PANE", &evidence.tmux_pane)
        .output()
        .unwrap();
    assert!(
        snapshot.status.success(),
        "{}",
        String::from_utf8_lossy(&snapshot.stderr)
    );
    let source = String::from_utf8(snapshot.stdout).unwrap();
    assert_snapshot(&source, &snapshot.stderr, roots, &evidence.first_layout);
    save_and_check(&source, &temp.path().join("config"));
}
