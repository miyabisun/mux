mod cli;
mod config;
mod lint;
mod project;
mod tmux;
mod update;

use std::{
    fs,
    io::{self, Read},
    process::ExitCode,
};

use anyhow::{Context, Result, bail};
use clap::Parser;

use crate::{
    cli::{Cli, Command},
    config::ConfigDir,
    lint::lint_project,
    project::{ProjectDocument, validate_project_name},
};

/// Run the command-line application.
///
/// # Errors
///
/// Returns an error when configuration, validation, an external command, or an
/// update operation fails.
pub fn run() -> Result<ExitCode> {
    run_cli(Cli::parse())
}

fn run_cli(cli: Cli) -> Result<ExitCode> {
    match cli.command {
        Some(Command::Update) => update::run(),
        command => {
            let config = ConfigDir::discover()?;
            match command {
                None => select_and_open(&config),
                Some(Command::Save { name, force }) => save(&config, &name, force),
                Some(Command::Rm { name }) => remove(&config, &name),
                Some(Command::Ls) => list(&config),
                Some(Command::Check { name }) => check(&config, &name),
                Some(Command::Lint { name }) => lint(&config, name.as_deref()),
                Some(Command::Update) => unreachable!("handled above"),
            }
        }
    }
}

fn select_and_open(config: &ConfigDir) -> Result<ExitCode> {
    let names = config.list()?;
    if names.is_empty() {
        bail!("no projects found in {}", config.path().display());
    }
    let Some(name) = cli::select_with_fzf(&names)? else {
        return Ok(ExitCode::from(130));
    };
    let document = config.load(&name)?;
    tmux::open(&document.project)?;
    Ok(ExitCode::SUCCESS)
}

fn save(config: &ConfigDir, name: &str, force: bool) -> Result<ExitCode> {
    validate_project_name(name)?;
    let mut source = String::new();
    io::stdin()
        .read_to_string(&mut source)
        .context("cannot read project TOML from stdin")?;
    let document = ProjectDocument::parse(&source)?;
    for warning in lint_project(&document) {
        eprintln!("warning: {warning}");
    }
    config.save(name, source.as_bytes(), force)?;
    Ok(ExitCode::SUCCESS)
}

fn remove(config: &ConfigDir, name: &str) -> Result<ExitCode> {
    config.remove(name)?;
    Ok(ExitCode::SUCCESS)
}

fn list(config: &ConfigDir) -> Result<ExitCode> {
    for name in config.list()? {
        println!("{name}");
    }
    Ok(ExitCode::SUCCESS)
}

fn check(config: &ConfigDir, name: &str) -> Result<ExitCode> {
    config.load(name)?;
    println!("{name}: ok");
    Ok(ExitCode::SUCCESS)
}

fn lint(config: &ConfigDir, name: Option<&str>) -> Result<ExitCode> {
    let names = match name {
        Some(name) => {
            validate_project_name(name)?;
            vec![name.to_owned()]
        }
        None => config.list()?,
    };

    let mut warning_count = 0_u32;
    for name in names {
        let document = config.load(&name)?;
        for warning in lint_project(&document) {
            eprintln!("{name}: warning: {warning}");
            warning_count += 1;
        }
    }
    if warning_count == 0 {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

fn load_toml(path: &std::path::Path) -> Result<ProjectDocument> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("cannot read project {}", path.display()))?;
    ProjectDocument::parse(&source)
}
