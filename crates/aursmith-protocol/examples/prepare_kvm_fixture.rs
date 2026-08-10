use anyhow::{Context, bail};
use aursmith_domain::{AttemptRef, WorkerRole};
use aursmith_protocol::{BuildProfileSpec, JobKind, JobSpec, ResourceLimits, SignedEnvelope};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use serde_json::json;
use std::{env, fs, path::Path};
use uuid::Uuid;

#[derive(Deserialize)]
struct Candidate {
    spec: BuildProfileSpec,
}

fn main() -> anyhow::Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 3 {
        bail!("用法：prepare_kvm_fixture <Profile 导出目录> <临时运行目录>");
    }
    let source = Path::new(&arguments[1]);
    let runtime = Path::new(&arguments[2]);
    fs::create_dir(runtime)?;
    let candidate: Candidate = serde_json::from_slice(
        &fs::read(source.join("profile-candidate.json")).context("缺少 Profile candidate")?,
    )?;
    if candidate.spec.content_sha256()? != candidate.spec.profile_sha256 {
        bail!("Profile candidate 内容摘要不匹配");
    }
    let signing_key = SigningKey::from_bytes(&[42_u8; 32]);
    let profile_directory = runtime
        .join("profiles")
        .join(&candidate.spec.profile_sha256);
    fs::create_dir_all(&profile_directory)?;
    for name in ["root.qcow2", "vmlinuz-linux", "initramfs-linux.img"] {
        fs::copy(source.join(name), profile_directory.join(name))?;
    }
    let profile_envelope =
        SignedEnvelope::sign("aursmith.build_profile", &candidate.spec, &signing_key)?;
    fs::write(
        profile_directory.join("profile-envelope.json"),
        serde_json::to_vec(&profile_envelope)?,
    )?;

    let job_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let now = Utc::now();
    let job = JobSpec {
        job_id,
        attempt: AttemptRef {
            job_id,
            attempt_id,
            generation: 0,
        },
        required_role: WorkerRole::Builder,
        kind: JobKind::ProfileFixture,
        revision_sha256: candidate.spec.profile_sha256.clone(),
        source_manifest_sha256: Some("0".repeat(64)),
        dependency_snapshot_sha256: Some("0".repeat(64)),
        profile_sha256: Some(candidate.spec.profile_sha256.clone()),
        source_attempt_id: None,
        dependency_attempt_ids: vec![],
        dependencies: vec![],
        inputs: vec![],
        inline_inputs: vec![],
        limits: ResourceLimits {
            cpu_count: 1,
            memory_mib: 1024,
            disk_mib: 4096,
            timeout_seconds: 120,
        },
        issued_at: now,
        expires_at: now + Duration::minutes(10),
    };
    let job_envelope = SignedEnvelope::sign("aursmith.job_spec", &job, &signing_key)?;
    fs::write(
        runtime.join("job-envelope.json"),
        serde_json::to_vec(&job_envelope)?,
    )?;
    println!(
        "{}",
        serde_json::to_string(&json!({
            "controller_verifying_key_hex": hex::encode(signing_key.verifying_key().as_bytes()),
            "profile_sha256": candidate.spec.profile_sha256,
            "job_id": job_id,
            "attempt_id": attempt_id
        }))?
    );
    Ok(())
}
