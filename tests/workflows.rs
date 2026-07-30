use std::{fs, path::Path};

#[test]
fn github_workflows_are_valid_yaml() {
    for path in [
        Path::new(".github/workflows/ci.yml"),
        Path::new(".github/workflows/release.yml"),
    ] {
        let source = fs::read_to_string(path).unwrap();
        serde_yaml::from_str::<serde_yaml::Value>(&source).unwrap();
    }
}

#[test]
fn release_workflow_publishes_the_contract_assets() {
    let source = fs::read_to_string(".github/workflows/release.yml").unwrap();
    for required in [
        "x86_64-unknown-linux-musl",
        "aarch64-apple-darwin",
        "mux-linux-x86_64.tar.gz",
        "mux-macos-aarch64.tar.gz",
        "Verify tag matches Cargo version",
        "softprops/action-gh-release@3bb12739c298aeb8a4eeaf626c5b8d85266b0e65",
        "mux LICENSE",
    ] {
        assert!(source.contains(required), "missing {required}");
    }
}

#[test]
fn third_party_actions_are_pinned_to_full_commit_shas() {
    for path in [
        Path::new(".github/workflows/ci.yml"),
        Path::new(".github/workflows/release.yml"),
    ] {
        let source = fs::read_to_string(path).unwrap();
        for line in source.lines() {
            let Some(action) = line.trim().strip_prefix("- uses: ") else {
                continue;
            };
            let action = action.split_whitespace().next().unwrap();
            let (_, reference) = action.split_once('@').unwrap();
            assert_eq!(reference.len(), 40, "{action} in {}", path.display());
            assert!(
                reference.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{action} in {}",
                path.display()
            );
        }
    }
}

#[test]
fn release_write_permission_is_scoped_to_the_release_job() {
    let source = fs::read_to_string(".github/workflows/release.yml").unwrap();
    let workflow: serde_yaml::Value = serde_yaml::from_str(&source).unwrap();
    assert_eq!(workflow["permissions"]["contents"], "read");
    assert_eq!(
        workflow["jobs"]["release"]["permissions"]["contents"],
        "write"
    );
    assert!(workflow["jobs"]["build"]["permissions"].is_null());
}
