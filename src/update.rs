use std::{
    env,
    fs::{self, File},
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail};
use flate2::read::GzDecoder;
use reqwest::blocking::Client;
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::Archive;
use tempfile::{NamedTempFile, TempDir};

const REPOSITORY: &str = "miyabisun/mux";
const RELEASE_API: &str = "https://api.github.com/repos/miyabisun/mux/releases/latest";
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 4096;
const MAX_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct LatestRelease {
    tag_name: String,
}

trait Downloader {
    fn download(&self, url: &str, destination: &Path, maximum_bytes: u64) -> Result<()>;
}

struct HttpsDownloader {
    client: Client,
}

impl HttpsDownloader {
    fn new() -> Result<Self> {
        let client = Client::builder()
            .user_agent(concat!("mux/", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .context("cannot initialize HTTPS client")?;
        Ok(Self { client })
    }
}

impl Downloader for HttpsDownloader {
    fn download(&self, url: &str, destination: &Path, maximum_bytes: u64) -> Result<()> {
        let mut response = self
            .client
            .get(url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .with_context(|| format!("HTTPS download failed for {url}"))?;
        if response
            .content_length()
            .is_some_and(|length| length > maximum_bytes)
        {
            bail!("download from {url} exceeds the size limit");
        }
        let mut output = File::create(destination)?;
        let copied = std::io::copy(&mut response.by_ref().take(maximum_bytes + 1), &mut output)?;
        if copied > maximum_bytes {
            bail!("download from {url} exceeds the size limit");
        }
        output.sync_all()?;
        Ok(())
    }
}

pub(crate) fn run() -> Result<std::process::ExitCode> {
    let target = update_target()?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))
        .context("local package version is not valid semver")?;
    let workspace = tempfile::tempdir()?;
    let downloader = HttpsDownloader::new()?;
    let remote = fetch_latest(&downloader, &workspace)?;
    if !update_required(&current, &remote) {
        println!("mux: up to date (local {current}, latest {remote}); binary unchanged");
        return Ok(std::process::ExitCode::SUCCESS);
    }

    let asset = platform_asset()?;
    let extracted = download_and_verify(&downloader, &workspace, &remote, asset)?;
    verify_binary_version(&extracted, &remote)?;
    atomic_replace(&target, &extracted)?;
    println!("mux: updated {current} -> {remote}");
    Ok(std::process::ExitCode::SUCCESS)
}

fn update_required(current: &Version, remote: &Version) -> bool {
    remote > current
}

fn update_target() -> Result<PathBuf> {
    let target = env::current_exe()?.canonicalize()?;
    let metadata = fs::symlink_metadata(&target)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!(
            "refusing to update a non-regular executable: {}",
            target.display()
        );
    }
    Ok(target)
}

fn fetch_latest(downloader: &impl Downloader, workspace: &TempDir) -> Result<Version> {
    let metadata_path = workspace.path().join("latest.json");
    downloader
        .download(RELEASE_API, &metadata_path, MAX_METADATA_BYTES)
        .context("GitHub latest release lookup failed")?;
    let release: LatestRelease = serde_json::from_reader(File::open(metadata_path)?)
        .context("GitHub latest release response is invalid")?;
    parse_release_tag(&release.tag_name)
}

fn parse_release_tag(tag: &str) -> Result<Version> {
    let value = tag
        .strip_prefix('v')
        .context("latest release tag must start with 'v'")?;
    let version = Version::parse(value).context("latest release tag is not valid semver")?;
    if !version.pre.is_empty()
        || !version.build.is_empty()
        || tag != format!("v{}.{}.{}", version.major, version.minor, version.patch)
    {
        bail!("latest release tag is not a stable vMAJOR.MINOR.PATCH tag");
    }
    Ok(version)
}

fn platform_asset() -> Result<&'static str> {
    platform_asset_for(env::consts::OS, env::consts::ARCH)
}

fn platform_asset_for(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("mux-linux-x86_64.tar.gz"),
        ("macos", "aarch64") => Ok("mux-macos-aarch64.tar.gz"),
        (os, arch) => bail!("self-update is not supported on {os}/{arch}"),
    }
}

fn download_and_verify(
    downloader: &impl Downloader,
    workspace: &TempDir,
    version: &Version,
    asset: &str,
) -> Result<PathBuf> {
    let base = format!("https://github.com/{REPOSITORY}/releases/download/v{version}");
    let archive_path = workspace.path().join(asset);
    let checksum_path = workspace.path().join(format!("{asset}.sha256"));
    downloader.download(&format!("{base}/{asset}"), &archive_path, MAX_ARCHIVE_BYTES)?;
    downloader.download(
        &format!("{base}/{asset}.sha256"),
        &checksum_path,
        MAX_CHECKSUM_BYTES,
    )?;
    verify_checksum(&archive_path, &checksum_path, asset)?;
    extract_binary(&archive_path, workspace.path())
}

fn verify_checksum(archive: &Path, checksum: &Path, asset: &str) -> Result<()> {
    let text = fs::read_to_string(checksum)?;
    let mut fields = text.split_whitespace();
    let expected = fields.next().context("checksum file is empty")?;
    let filename = fields.next().context("checksum file has no filename")?;
    if fields.next().is_some()
        || expected.len() != 64
        || !expected.bytes().all(|byte| byte.is_ascii_hexdigit())
        || filename.trim_start_matches('*') != asset
    {
        bail!("checksum file has an invalid format");
    }

    let mut source = File::open(archive)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("release checksum mismatch");
    }
    Ok(())
}

fn extract_binary(archive_path: &Path, destination: &Path) -> Result<PathBuf> {
    let decoder = GzDecoder::new(File::open(archive_path)?);
    let mut archive = Archive::new(decoder);
    let binary = destination.join("verified-mux");
    let mut found = false;
    let mut expanded_bytes = 0_u64;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if !safe_archive_path(&path) || !entry.header().entry_type().is_file() {
            bail!("release archive contains an unsafe entry");
        }
        expanded_bytes = expanded_bytes
            .checked_add(entry.size())
            .context("release archive size overflow")?;
        if expanded_bytes > MAX_ARCHIVE_BYTES {
            bail!("release archive expands beyond the size limit");
        }
        if path != Path::new("mux") {
            continue;
        }
        if found || entry.size() == 0 || entry.size() > MAX_BINARY_BYTES {
            bail!("release archive contains an invalid mux binary");
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&binary)?;
        std::io::copy(&mut entry, &mut output)?;
        output.sync_all()?;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
        found = true;
    }
    if !found {
        bail!("release archive does not contain mux");
    }
    Ok(binary)
}

fn safe_archive_path(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn verify_binary_version(binary: &Path, expected: &Version) -> Result<()> {
    let output = Command::new(binary)
        .arg("--version")
        .output()
        .context("cannot execute the downloaded mux binary")?;
    if !output.status.success() {
        bail!("downloaded mux --version failed");
    }
    let actual = String::from_utf8(output.stdout).context("mux --version is not UTF-8")?;
    if actual.trim() != format!("mux {expected}") {
        bail!(
            "downloaded binary version mismatch: expected mux {expected}, got {}",
            actual.trim()
        );
    }
    Ok(())
}

fn atomic_replace(target: &Path, source: &Path) -> Result<()> {
    let parent = target.parent().context("executable parent is missing")?;
    let metadata = fs::symlink_metadata(target)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_file() {
        bail!("refusing to replace a non-regular executable");
    }
    let mut staged = NamedTempFile::new_in(parent)?;
    let mut input = File::open(source)?;
    std::io::copy(&mut input, staged.as_file_mut())?;
    staged
        .as_file()
        .set_permissions(fs::Permissions::from_mode(metadata.permissions().mode()))?;
    staged.as_file().sync_all()?;
    staged
        .persist(target)
        .map_err(|error| error.error)
        .context("atomic executable replacement failed")?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, io::Cursor};

    use super::*;

    struct FakeDownloader {
        files: BTreeMap<String, Vec<u8>>,
    }

    impl Downloader for FakeDownloader {
        fn download(&self, url: &str, destination: &Path, maximum_bytes: u64) -> Result<()> {
            let body = self.files.get(url).context("injected download failure")?;
            if body.len() as u64 > maximum_bytes {
                bail!("injected download exceeds limit");
            }
            fs::write(destination, body)?;
            Ok(())
        }
    }

    #[test]
    fn stable_release_tags_and_comparison_are_strict() {
        let current = Version::new(1, 2, 3);
        assert_eq!(parse_release_tag("v1.2.3").unwrap(), current);
        assert!(!update_required(
            &current,
            &parse_release_tag("v1.2.3").unwrap()
        ));
        assert!(!update_required(
            &current,
            &parse_release_tag("v1.2.2").unwrap()
        ));
        assert!(update_required(
            &current,
            &parse_release_tag("v1.2.4").unwrap()
        ));
        for invalid in ["1.2.3", "v1.2", "v1.2.3-dev", "v01.2.3", "v1.2.3+build"] {
            assert!(parse_release_tag(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn release_assets_match_supported_platforms() {
        assert_eq!(
            platform_asset_for("linux", "x86_64").unwrap(),
            "mux-linux-x86_64.tar.gz"
        );
        assert_eq!(
            platform_asset_for("macos", "aarch64").unwrap(),
            "mux-macos-aarch64.tar.gz"
        );
        assert!(platform_asset_for("linux", "aarch64").is_err());
        assert!(platform_asset_for("windows", "x86_64").is_err());
    }

    #[test]
    fn download_failure_is_contextualized() {
        let workspace = tempfile::tempdir().unwrap();
        let error = fetch_latest(
            &FakeDownloader {
                files: BTreeMap::new(),
            },
            &workspace,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("GitHub latest release lookup failed")
        );
    }

    #[test]
    fn bad_checksum_does_not_touch_an_existing_binary() {
        let workspace = tempfile::tempdir().unwrap();
        let archive = workspace.path().join("asset.tar.gz");
        let checksum = workspace.path().join("asset.tar.gz.sha256");
        fs::write(&archive, b"archive").unwrap();
        fs::write(&checksum, format!("{}  asset.tar.gz\n", "0".repeat(64))).unwrap();
        let target = workspace.path().join("mux");
        fs::write(&target, b"old").unwrap();

        assert!(verify_checksum(&archive, &checksum, "asset.tar.gz").is_err());
        assert_eq!(fs::read(target).unwrap(), b"old");
    }

    #[test]
    fn archive_rejects_traversal_symlinks_and_missing_mux() {
        assert!(!safe_archive_path(Path::new("../mux")));
        assert!(safe_archive_path(Path::new("nested/mux")));
        for (path, kind) in [
            ("mux", tar::EntryType::Symlink),
            ("other", tar::EntryType::Regular),
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let archive_path = workspace.path().join("bad.tar.gz");
            let file = File::create(&archive_path).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
            let mut builder = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(if kind.is_file() { 3 } else { 0 });
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(b"bin"))
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
            assert!(extract_binary(&archive_path, workspace.path()).is_err());
        }
    }

    #[test]
    fn release_archive_accepts_mux_and_license() {
        let workspace = tempfile::tempdir().unwrap();
        let archive_path = workspace.path().join("release.tar.gz");
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (path, body, mode) in [
            ("mux", b"binary".as_slice(), 0o755),
            ("LICENSE", b"MIT", 0o644),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(mode);
            header.set_cksum();
            builder
                .append_data(&mut header, path, Cursor::new(body))
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();

        let extracted = extract_binary(&archive_path, workspace.path()).unwrap();
        assert_eq!(fs::read(extracted).unwrap(), b"binary");
    }

    #[test]
    fn release_archive_rejects_duplicate_mux_binaries() {
        let workspace = tempfile::tempdir().unwrap();
        let archive_path = workspace.path().join("duplicate.tar.gz");
        let file = File::create(&archive_path).unwrap();
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for body in [b"first".as_slice(), b"second"] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "mux", Cursor::new(body))
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap();

        assert!(extract_binary(&archive_path, workspace.path()).is_err());
    }

    #[test]
    fn binary_version_is_checked_before_installation() {
        let workspace = tempfile::tempdir().unwrap();
        let binary = workspace.path().join("mux");
        fs::write(&binary, "#!/bin/sh\nprintf 'mux 2.0.0\\n'\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        verify_binary_version(&binary, &Version::new(2, 0, 0)).unwrap();
        assert!(verify_binary_version(&binary, &Version::new(2, 0, 1)).is_err());
    }

    #[test]
    fn atomic_replace_preserves_mode_and_replaces_contents() {
        let workspace = tempfile::tempdir().unwrap();
        let target = workspace.path().join("mux");
        let source = workspace.path().join("new");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&source, b"new").unwrap();

        atomic_replace(&target, &source).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
