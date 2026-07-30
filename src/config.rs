use std::{
    collections::BTreeSet,
    env,
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use tempfile::NamedTempFile;

use crate::{load_toml, project::ProjectDocument, project::validate_project_name};

#[derive(Debug, Clone)]
pub(crate) struct ConfigDir {
    path: PathBuf,
}

impl ConfigDir {
    pub(crate) fn discover() -> Result<Self> {
        let path = discover_path(
            env::var_os("MUX_CONFIG"),
            env::var_os("XDG_CONFIG_HOME"),
            env::var_os("HOME"),
        )?;
        Ok(Self { path })
    }

    #[must_use]
    #[cfg(test)]
    pub(crate) const fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn list(&self) -> Result<Vec<String>> {
        let entries = match fs::read_dir(&self.path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).context("cannot read mux config directory"),
        };
        let mut projects = BTreeSet::new();
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            let path = entry.path();
            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            if extension != "toml" {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            validate_project_name(stem)
                .with_context(|| format!("invalid project filename in {}", self.path.display()))?;
            projects.insert(stem.to_owned());
        }
        Ok(projects.into_iter().collect())
    }

    pub(crate) fn load(&self, name: &str) -> Result<ProjectDocument> {
        let path = self.resolve(name)?;
        load_toml(&path)
    }

    pub(crate) fn save(&self, name: &str, content: &[u8], force: bool) -> Result<()> {
        validate_project_name(name)?;
        fs::create_dir_all(&self.path)
            .with_context(|| format!("cannot create config directory {}", self.path.display()))?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot secure config directory {}", self.path.display()))?;
        let target = self.path.join(format!("{name}.toml"));
        if target.exists() && !force {
            bail!("project '{name}' already exists; use --force to overwrite");
        }

        if force {
            let mut staged = NamedTempFile::new_in(&self.path)?;
            staged
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            staged.write_all(content)?;
            staged.as_file().sync_all()?;
            staged
                .persist(&target)
                .map_err(|error| error.error)
                .with_context(|| format!("cannot replace {}", target.display()))?;
        } else {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&target)
                .with_context(|| format!("cannot create {}", target.display()))?;
            file.write_all(content)?;
            file.sync_all()?;
        }
        Ok(())
    }

    pub(crate) fn remove(&self, name: &str) -> Result<()> {
        let path = self.resolve(name)?;
        fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))
    }

    fn resolve(&self, name: &str) -> Result<PathBuf> {
        validate_project_name(name)?;
        let path = self.path.join(format!("{name}.toml"));
        if path.is_file() {
            Ok(path)
        } else {
            bail!("project '{name}' does not exist in {}", self.path.display())
        }
    }
}

fn discover_path(
    mux_config: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(path) = mux_config {
        return Ok(path.into());
    }
    if let Some(path) = xdg_config_home {
        return Ok(PathBuf::from(path).join("mux"));
    }
    let home = home.context(
        "cannot resolve config directory: HOME, XDG_CONFIG_HOME, and MUX_CONFIG are unset",
    )?;
    Ok(PathBuf::from(home).join(".config/mux"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_and_ignores_other_extensions() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("z.toml"), "").unwrap();
        fs::write(temp.path().join("a.toml"), "").unwrap();
        let config = ConfigDir::from_path(temp.path().to_owned());
        assert_eq!(config.list().unwrap(), ["a", "z"]);
        fs::write(temp.path().join("ignored.yml"), "").unwrap();
        assert_eq!(config.list().unwrap(), ["a", "z"]);
    }

    #[test]
    fn missing_directory_lists_as_empty() {
        let temp = tempfile::tempdir().unwrap();
        let config = ConfigDir::from_path(temp.path().join("missing"));
        assert!(config.list().unwrap().is_empty());
    }

    #[test]
    fn config_path_precedence_is_deterministic() {
        assert_eq!(
            discover_path(
                Some("/mux".into()),
                Some("/xdg".into()),
                Some("/home".into())
            )
            .unwrap(),
            PathBuf::from("/mux")
        );
        assert_eq!(
            discover_path(None, Some("/xdg".into()), Some("/home".into())).unwrap(),
            PathBuf::from("/xdg/mux")
        );
        assert_eq!(
            discover_path(None, None, Some("/home".into())).unwrap(),
            PathBuf::from("/home/.config/mux")
        );
        assert!(discover_path(None, None, None).is_err());
    }

    #[test]
    fn save_secures_the_config_directory_and_files() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("config");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
        let config = ConfigDir::from_path(directory.clone());

        config
            .save("project", b"name = \"project\"\n", false)
            .unwrap();
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(directory.join("project.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        config
            .save("project", b"name = \"updated\"\n", true)
            .unwrap();
        assert_eq!(
            fs::metadata(directory.join("project.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
