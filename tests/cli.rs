use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
};

fn mux(config: &Path, arguments: &[&str], stdin: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mux"));
    command
        .args(arguments)
        .env("MUX_CONFIG", config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn().unwrap();
    if let Some(stdin) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
    }
    child.wait_with_output().unwrap()
}

fn project_toml(root: &Path, name: &str) -> String {
    format!(
        "name = \"{name}\"\nroot = \"{}\"\nstartup_window = \"shell\"\nstartup_pane = 1\n\n[[windows]]\nname = \"shell\"\nfocused_pane = 1\npanes = [\"\"]\n",
        root.display()
    )
}

#[test]
fn save_check_list_lint_and_remove_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let source = project_toml(temp.path(), "demo");

    assert!(
        mux(&config, &["save", "demo"], Some(&source))
            .status
            .success()
    );
    assert!(config.join("demo.toml").is_file());

    let existing = mux(&config, &["save", "demo"], Some(&source));
    assert!(!existing.status.success());
    assert!(String::from_utf8_lossy(&existing.stderr).contains("--force"));
    assert!(
        mux(&config, &["save", "demo", "--force"], Some(&source))
            .status
            .success()
    );

    let check = mux(&config, &["check", "demo"], None);
    assert!(check.status.success());
    assert_eq!(String::from_utf8(check.stdout).unwrap(), "demo: ok\n");
    let list = mux(&config, &["ls"], None);
    assert!(list.status.success());
    assert_eq!(String::from_utf8(list.stdout).unwrap(), "demo\n");
    assert!(mux(&config, &["lint", "demo"], None).status.success());

    assert!(mux(&config, &["rm", "demo"], None).status.success());
    assert!(!mux(&config, &["rm", "demo"], None).status.success());
}

#[test]
fn save_rejects_invalid_input_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let invalid_cases = [
        "not = [\"valid\"",
        "name = \"x\"\nroot = \"/tmp\"\nunknown = true\nwindows = []\n",
        "name = \"x\"\nroot = \"/tmp\"\nstartup_pane = 2\n[[windows]]\nname = \"one\"\npanes = [\"\"]\n",
    ];
    for source in invalid_cases {
        let output = mux(&config, &["save", "bad"], Some(source));
        assert!(!output.status.success(), "{source}");
        assert!(!config.join("bad.toml").exists());
    }
    fs::create_dir_all(&config).unwrap();
    fs::write(config.join("kept.toml"), b"original").unwrap();
    assert!(
        !mux(&config, &["save", "kept", "--force"], Some("invalid = ["))
            .status
            .success()
    );
    assert_eq!(fs::read(config.join("kept.toml")).unwrap(), b"original");
    assert!(
        !mux(
            &config,
            &["save", "../bad"],
            Some(&project_toml(temp.path(), "bad"))
        )
        .status
        .success()
    );
}

#[test]
fn lint_warnings_do_not_block_save_but_make_lint_fail() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let source = format!(
        "name = \"warn\"\nroot = \"{}\"\n[[windows]]\nname = \"shell\"\npanes = [\"claude --resume\"]\n",
        temp.path().display()
    );
    let save = mux(&config, &["save", "warn"], Some(&source));
    assert!(save.status.success());
    assert!(String::from_utf8_lossy(&save.stderr).contains("warning:"));
    let lint = mux(&config, &["lint", "warn"], None);
    assert_eq!(lint.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&lint.stderr).contains("has arguments"));
    assert_eq!(mux(&config, &["lint"], None).status.code(), Some(1));
}

#[test]
fn missing_projects_and_empty_config_have_distinct_contracts() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("missing");
    let list = mux(&config, &["ls"], None);
    assert!(list.status.success());
    assert!(list.stdout.is_empty());
    assert!(!mux(&config, &["check", "none"], None).status.success());
    assert!(!mux(&config, &["lint", "none"], None).status.success());
    let select = mux(&config, &[], None);
    assert!(!select.status.success());
    assert!(String::from_utf8_lossy(&select.stderr).contains("no projects found"));
}

#[test]
fn non_toml_files_are_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let source = project_toml(temp.path(), "same");
    fs::write(temp.path().join("same.yml"), &source).unwrap();
    fs::write(temp.path().join("same.yaml"), &source).unwrap();
    let output = mux(temp.path(), &["ls"], None);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
}

#[test]
fn toml_is_loaded_and_force_overwritten_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("demo.toml");
    fs::write(&path, project_toml(temp.path(), "demo")).unwrap();
    assert!(mux(temp.path(), &["check", "demo"], None).status.success());

    let replacement = project_toml(temp.path(), "replacement");
    assert!(
        mux(
            temp.path(),
            &["save", "demo", "--force"],
            Some(&replacement)
        )
        .status
        .success()
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), replacement);
}

#[test]
fn selector_alone_requires_fzf_and_cancellation_is_silent() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("demo.toml"),
        project_toml(temp.path(), "demo"),
    )
    .unwrap();

    let missing = Command::new(env!("CARGO_BIN_EXE_mux"))
        .env("MUX_CONFIG", temp.path())
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("fzf is required"));

    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let fzf = bin.join("fzf");
    fs::write(&fzf, "#!/bin/sh\nexit 130\n").unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o755)).unwrap();
    let cancelled = Command::new(env!("CARGO_BIN_EXE_mux"))
        .env("MUX_CONFIG", temp.path())
        .env("PATH", &bin)
        .output()
        .unwrap();
    assert_eq!(cancelled.status.code(), Some(130));
    assert!(cancelled.stderr.is_empty());
}

#[test]
fn selector_switches_inside_tmux_and_attaches_outside() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("demo.toml"),
        project_toml(temp.path(), "demo"),
    )
    .unwrap();
    let bin = temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let fzf = bin.join("fzf");
    fs::write(
        &fzf,
        "#!/bin/sh\nwhile IFS= read -r _line; do :; done\nprintf 'demo\\n'\n",
    )
    .unwrap();
    fs::set_permissions(&fzf, fs::Permissions::from_mode(0o755)).unwrap();
    let tmux = bin.join("tmux");
    fs::write(
        &tmux,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$MUX_TEST_LOG\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let log = temp.path().join("tmux.log");

    let inside = Command::new(env!("CARGO_BIN_EXE_mux"))
        .env("MUX_CONFIG", temp.path())
        .env("MUX_TEST_LOG", &log)
        .env("PATH", &bin)
        .env("TMUX", "test")
        .output()
        .unwrap();
    assert!(
        inside.status.success(),
        "{}",
        String::from_utf8_lossy(&inside.stderr)
    );
    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains("has-session -t =demo"), "{calls}");
    assert!(calls.contains("switch-client -t =demo"), "{calls}");

    fs::write(&log, "").unwrap();
    let outside = Command::new(env!("CARGO_BIN_EXE_mux"))
        .env("MUX_CONFIG", temp.path())
        .env("MUX_TEST_LOG", &log)
        .env("PATH", &bin)
        .env_remove("TMUX")
        .output()
        .unwrap();
    assert!(
        outside.status.success(),
        "{}",
        String::from_utf8_lossy(&outside.stderr)
    );
    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains("attach-session -t =demo"), "{calls}");
}
