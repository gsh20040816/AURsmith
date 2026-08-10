use anyhow::{Context, bail};
use aursmith_protocol::{ArtifactRecord, ReleaseAuthorization, SignedEnvelope};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path, process::Command};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 3 {
        bail!("用法：prepare_release_fixture <软件包> <Signer inbox>");
    }
    let package = Path::new(&arguments[1]);
    let inbox = Path::new(&arguments[2]);
    let metadata = fs::metadata(package)?;
    let output = Command::new("/usr/bin/bsdtar")
        .args(["-xOf"])
        .arg(package)
        .arg(".PKGINFO")
        .output()?;
    if !output.status.success() {
        bail!("测试软件包缺少 .PKGINFO");
    }
    let pkginfo = String::from_utf8(output.stdout)?;
    let field = |name: &str| {
        pkginfo
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .find_map(|(key, value)| (key == name).then_some(value.to_owned()))
            .with_context(|| format!(".PKGINFO 缺少 {name}"))
    };
    let release_id = Uuid::new_v4();
    let directory = inbox.join(release_id.to_string());
    fs::create_dir_all(&directory)?;
    let name = package.file_name().context("软件包缺少文件名")?;
    fs::copy(package, directory.join(name))?;
    let bytes = fs::read(package)?;
    let authorization = ReleaseAuthorization {
        release_id,
        batch_id: Uuid::new_v4(),
        writer_epoch: 1,
        repository_name: "aursmith".into(),
        source_git_commit: "f".repeat(40),
        revision_sha256s: vec!["a".repeat(64)],
        audit_report_sha256s: vec!["b".repeat(64)],
        artifacts: vec![ArtifactRecord {
            path: name.to_string_lossy().into_owned(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: metadata.len(),
            package_name: Some(field("pkgname")?),
            package_version: Some(field("pkgver")?),
            architecture: Some(field("arch")?),
        }],
        removed_package_names: vec![],
        evidence: Default::default(),
        issued_at: Utc::now(),
        expires_at: Utc::now() + Duration::minutes(10),
    };
    let signing_key = SigningKey::from_bytes(&[42_u8; 32]);
    let envelope = SignedEnvelope::sign(
        "aursmith.release_authorization",
        &authorization,
        &signing_key,
    )?;
    fs::write(
        directory.join("authorization.json"),
        serde_json::to_vec(&envelope)?,
    )?;
    fs::write(
        directory.join("artifact-inspections.json"),
        serde_json::to_vec_pretty(&vec![serde_json::json!({
            "artifact_sha256": authorization.artifacts[0].sha256,
            "fixture": true
        })])?,
    )?;
    println!(
        "{}",
        serde_json::json!({
            "release_id": release_id,
            "controller_verifying_key_hex": hex::encode(signing_key.verifying_key().as_bytes())
        })
    );
    Ok(())
}
