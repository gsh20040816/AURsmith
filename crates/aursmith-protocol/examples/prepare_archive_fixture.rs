use anyhow::{Context, bail};
use aursmith_protocol::{ManifestEntry, SignedEnvelope, TransferCapability};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 6 {
        bail!(
            "用法：prepare_archive_fixture <publisher-id> <archiver-id> <release-id> <release-directory> <envelope-output>"
        );
    }
    let root = Path::new(&arguments[4]);
    let mut paths = Vec::new();
    collect(root, root, &mut paths)?;
    paths.sort();
    let files = paths
        .into_iter()
        .map(|path| {
            let file = root.join(&path);
            let bytes = fs::read(&file)?;
            Ok(ManifestEntry {
                path,
                sha256: hex::encode(Sha256::digest(&bytes)),
                size: bytes.len() as u64,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let capability = TransferCapability {
        id: Uuid::new_v4(),
        source_worker: Uuid::parse_str(&arguments[1])?,
        destination_worker: Uuid::parse_str(&arguments[2])?,
        attempt: None,
        release_id: Some(Uuid::parse_str(&arguments[3])?),
        writer_epoch: 1,
        files,
        expires_at: Utc::now() + Duration::minutes(10),
    };
    let key = SigningKey::from_bytes(&[42_u8; 32]);
    let envelope = SignedEnvelope::sign("aursmith.transfer_capability", &capability, &key)?;
    fs::write(&arguments[5], serde_json::to_vec(&envelope)?)?;
    println!(
        "{}",
        serde_json::json!({
            "capability_id": capability.id,
            "controller_verifying_key_hex": hex::encode(key.verifying_key().as_bytes()),
            "files": capability.files.len(),
        })
    );
    Ok(())
}

fn collect(root: &Path, directory: &Path, paths: &mut Vec<String>) -> anyhow::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(root, &entry.path(), paths)?;
        } else if kind.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .context("文件不在 Release 根目录")?
                .to_string_lossy()
                .into_owned();
            aursmith_protocol::validate_relative_path(&relative)?;
            paths.push(relative);
        } else {
            bail!("Release fixture 包含非普通文件");
        }
    }
    Ok(())
}
