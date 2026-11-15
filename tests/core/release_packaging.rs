use anyhow::Context;
use crate::common::Workspace;
use serde_yaml::Value;
use sha2::{Digest, Sha256};
use std::{fs, path::Path, process::Command};

fn workflow(name: &str) -> anyhow::Result<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".github/workflows")
        .join(name);
    let source = fs::read_to_string(&path)
        .with_context(|| format!("read production workflow {}", path.display()))?;
    serde_yaml::from_str(&source).context("production workflow is valid YAML")
}

fn steps(job: &Value) -> &[Value] {
    job["steps"].as_sequence().map(Vec::as_slice).unwrap_or(&[])
}

#[test]
fn release_package_step_produces_installable_archive_and_checksum() {
    let release = workflow("release.yml").or_else(|_| workflow("ci.yml"));
    assert!(
        release.is_ok(),
        "assertion failed: release workflow is required: {release:?}"
    );
    let release = release.unwrap();
    let package = release["jobs"]
        .as_mapping()
        .expect("release jobs")
        .values()
        .flat_map(steps)
        .find(|step| step["id"].as_str() == Some("package"))
        .and_then(|step| step["run"].as_str());
    assert!(
        package.is_some(),
        "release must expose its actual package step"
    );
    let workspace = Workspace::new();
    let target = match (std::env::consts::ARCH, std::env::consts::OS) {
        ("x86_64", "linux") => "x86_64-unknown-linux-gnu",
        ("aarch64", "linux") => "aarch64-unknown-linux-gnu",
        ("x86_64", "macos") => "x86_64-apple-darwin",
        ("aarch64", "macos") => "aarch64-apple-darwin",
        host => panic!("unsupported packaging proof host: {host:?}"),
    };
    let binary = workspace.path(format!("target/{target}/release/acdd"));
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::copy(env!("CARGO_BIN_EXE_acdd"), &binary).unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut companions = Vec::new();
    for entry in walkdir::WalkDir::new(repo.join("skills")).follow_links(true) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(repo).unwrap().to_path_buf();
            let bytes = fs::read(entry.path()).unwrap();
            workspace.write_bytes(&relative, &bytes);
            companions.push((relative, bytes));
        }
    }
    let output = Command::new("bash")
        .args(["-e", "-o", "pipefail", "-c", package.unwrap()])
        .env("TARGET", target)
        .current_dir(workspace.root())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "package step failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let archive_name = format!("contextunity-acdd-{target}.tar.gz");
    let archive = workspace.path(format!("dist/{archive_name}"));
    let bytes = fs::read(&archive).expect("package step creates public archive name");
    let digest = hex::encode(Sha256::digest(&bytes));
    let sums = fs::read_to_string(workspace.path("dist/SHA256SUMS")).expect("release checksums");
    assert!(sums
        .lines()
        .any(|line| line == format!("{digest}  {archive_name}")));
    let unpacked = workspace.path("unpacked");
    fs::create_dir(&unpacked).unwrap();
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked)
        .output()
        .unwrap();
    assert!(output.status.success(), "release archive extracts cleanly");
    let output = Command::new(unpacked.join("acdd"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success(), "archived executable runs");
    assert!(String::from_utf8_lossy(&output.stdout).contains("acdd"));
    for (relative, expected) in companions {
        assert_eq!(
            fs::read(unpacked.join(&relative)).unwrap(),
            expected,
            "companion asset {}",
            relative.display()
        );
    }
    assert!(unpacked.join("skills/install.sh").is_file());
    assert!(unpacked.join("skills/acdd/SKILL.md").is_file());
}
