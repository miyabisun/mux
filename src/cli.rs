use std::{
    io::Write,
    process::{Command as ProcessCommand, Stdio},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(version, about = "Select, validate, and launch tmux projects")]
pub struct Cli {
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
