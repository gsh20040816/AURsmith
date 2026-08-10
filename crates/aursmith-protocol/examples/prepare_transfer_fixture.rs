use anyhow::{Context, bail};
use aursmith_domain::AttemptRef;
use aursmith_protocol::{ManifestEntry, SignedEnvelope, TransferCapability};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 7 {
        bail!(
            "用法：prepare_transfer_fixture <source-worker-id> <destination-worker-id> <job-id> <attempt-id> <artifact> <envelope-output>"
        );
    }
    let artifact = Path::new(&arguments[5]);
    let bytes = fs::read(artifact)?;
    let capability = TransferCapability {
        id: Uuid::new_v4(),
        source_worker: Uuid::parse_str(&arguments[1])?,
        destination_worker: Uuid::parse_str(&arguments[2])?,
        attempt: Some(AttemptRef {
            job_id: Uuid::parse_str(&arguments[3])?,
            attempt_id: Uuid::parse_str(&arguments[4])?,
            generation: 0,
        }),
        release_id: None,
        writer_epoch: 0,
        files: vec![ManifestEntry {
            path: artifact
                .file_name()
                .context("Artifact 缺少文件名")?
                .to_string_lossy()
                .into_owned(),
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: bytes.len() as u64,
        }],
        expires_at: Utc::now() + Duration::minutes(10),
    };
    let key = SigningKey::from_bytes(&[42_u8; 32]);
    let envelope = SignedEnvelope::sign("aursmith.transfer_capability", &capability, &key)?;
    fs::write(&arguments[6], serde_json::to_vec(&envelope)?)?;
    println!(
        "{}",
        serde_json::json!({
            "capability_id": capability.id,
            "controller_verifying_key_hex": hex::encode(key.verifying_key().as_bytes())
        })
    );
    Ok(())
}
