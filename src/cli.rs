use std::{
    io::Write,
    path::PathBuf,
    process::{Command as ProcessCommand, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    version,
    about = "Select, validate, and launch tmux projects",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    /// Override the selected project's tmux session name.
    #[arg(short = 't', long = "target", value_name = "NAME")]
    pub target: Option<String>,
    /// Override the selected project's top-level working directory.
    #[arg(short = 'c', long = "cwd", value_name = "DIR")]
    pub cwd: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print a TOML project for the current tmux session.
    Snapshot,
    /// Save a TOML project from standard input.
    Save {
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// Remove a saved project.
    Rm { name: String },
    /// List saved project names.
    Ls,
    /// Check whether one project can be launched.
    Check { name: String },
    /// Report safety and quality warnings.
    Lint { name: Option<String> },
    /// Update mux from the latest GitHub Release.
    Update,
}

pub fn select_with_fzf(names: &[String]) -> Result<Option<String>> {
    let mut child = ProcessCommand::new("fzf")
        .args(["--prompt", "mux> ", "--height", "40%", "--reverse"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("fzf is required for interactive project selection")?;
    {
        let mut stdin = child.stdin.take().context("cannot open fzf stdin")?;
        for name in names {
            writeln!(stdin, "{name}")?;
        }
    }
    let output = child.wait_with_output().context("cannot wait for fzf")?;
    if !output.status.success() {
        return Ok(None);
    }
    let selected = String::from_utf8(output.stdout).context("fzf returned non-UTF-8 output")?;
    let selected = selected.trim_end_matches(['\r', '\n']);
    if selected.is_empty() {
        return Ok(None);
    }
    if !names.iter().any(|name| name == selected) {
        bail!("fzf returned an unknown project");
    }
    Ok(Some(selected.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn launcher_overrides_accept_short_and_long_forms() {
        let short = Cli::try_parse_from(["mux", "-t", "work", "-c", "/tmp"]).unwrap();
        assert_eq!(short.target.as_deref(), Some("work"));
        assert_eq!(short.cwd.as_deref(), Some(Path::new("/tmp")));

        let long = Cli::try_parse_from(["mux", "--target", "work", "--cwd", "/tmp"]).unwrap();
        assert_eq!(long.target.as_deref(), Some("work"));
        assert_eq!(long.cwd.as_deref(), Some(Path::new("/tmp")));
    }

    #[test]
    fn launcher_overrides_conflict_with_subcommands_in_both_orders() {
        assert!(Cli::try_parse_from(["mux", "-t", "work", "check", "demo"]).is_err());
        assert!(Cli::try_parse_from(["mux", "check", "demo", "-t", "work"]).is_err());
        assert!(Cli::try_parse_from(["mux", "-c", "/tmp", "ls"]).is_err());
        assert!(Cli::try_parse_from(["mux", "ls", "-c", "/tmp"]).is_err());
    }
}
