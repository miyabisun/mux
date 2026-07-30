use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

#[test]
#[ignore = "requires real tmux and script binaries"]
fn attach_inherits_a_real_pseudo_terminal() {
    let tmux = find_executable("tmux");
    let script = find_executable("script");
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    let bin = temp.path().join("bin");
    fs::create_dir(&config).unwrap();
    fs::create_dir(&bin).unwrap();
    fs::write(
        config.join("demo.toml"),
        format!(
            "name = \"mux-attach-e2e\"\nroot = \"{}\"\n[[windows]]\nname = \"shell\"\npanes = [\"sleep 1; tmux detach-client\"]\n",
            temp.path().display()
        ),
    )
    .unwrap();
    write_executable(
        &bin.join("fzf"),
        "#!/bin/sh\nwhile IFS= read -r _line; do :; done\nprintf 'demo\\n'\n",
    );
    let socket = format!(
        "mux-attach-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    write_executable(
        &bin.join("tmux"),
        &format!(
            "#!/bin/sh\nexec '{}' -L '{}' -f /dev/null \"$@\"\n",
            tmux.display(),
            socket
        ),
    );

    let path = env::join_paths(
        std::iter::once(bin.clone()).chain(env::split_paths(&env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let command = format!(
        "env -u TMUX TERM='xterm-256color' PATH='{}' MUX_CONFIG='{}' '{}'",
        Path::new(&path).display(),
        config.display(),
        env!("CARGO_BIN_EXE_mux")
    );
    let status = Command::new(&script)
        .args(["-qec", &command, "/dev/null"])
        .status()
        .unwrap();

    let _ = Command::new(&tmux)
        .args(["-L", &socket, "kill-server"])
        .status();
    assert!(
        status.success(),
        "mux attach did not retain its pseudo-terminal"
    );
}

fn find_executable(name: &str) -> PathBuf {
    env::split_paths(&env::var_os("PATH").unwrap())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{name} is not installed"))
}

fn write_executable(path: &Path, content: &str) {
    fs::write(path, content).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
