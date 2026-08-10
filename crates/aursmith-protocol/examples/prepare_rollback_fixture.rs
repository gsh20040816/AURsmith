use anyhow::{Result, bail};
use aursmith_protocol::{ReleaseRollbackAuthorization, SignedEnvelope};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use std::{env, fs};
use uuid::Uuid;

fn main() -> Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 3 {
        bail!("用法：prepare_rollback_fixture <release-id> <envelope-output>");
    }
    let now = Utc::now();
    let authorization = ReleaseRollbackAuthorization {
        release_id: Uuid::parse_str(&arguments[1])?,
        writer_epoch: 1,
        issued_at: now,
        expires_at: now + Duration::minutes(5),
    };
    let key = SigningKey::from_bytes(&[42_u8; 32]);
    let envelope = SignedEnvelope::sign(
        "aursmith.release_rollback_authorization",
        &authorization,
        &key,
    )?;
    fs::write(&arguments[2], serde_json::to_vec(&envelope)?)?;
    println!(
        "{}",
        serde_json::json!({"release_id": authorization.release_id})
    );
    Ok(())
}
