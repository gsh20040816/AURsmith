use anyhow::{Context, bail};
use aursmith_domain::{AttemptRef, WorkerRole};
use aursmith_protocol::{GuestResult, JobKind, JobSpec, ResourceLimits, SignedEnvelope};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde_json::json;
use std::{env, fs, path::Path};
use uuid::Uuid;

fn main() -> anyhow::Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if arguments.len() != 2 {
        bail!("用法：prepare_kvm_build_fixture <Fetch 临时运行目录>");
    }
    let runtime = Path::new(&arguments[1]);
    let completed = runtime.join("jobs/completed");
    let source_attempt_directory = fs::read_dir(&completed)?
        .filter_map(Result::ok)
        .find(|entry| entry.path().join("output/build-result.json").is_file())
        .context("没有找到已完成 Fetch Attempt")?;
    let source_attempt_id = Uuid::parse_str(
        source_attempt_directory
            .file_name()
            .to_str()
            .context("Fetch Attempt 目录名无效")?,
    )?;
    let result: GuestResult = serde_json::from_slice(&fs::read(
        source_attempt_directory
            .path()
            .join("output/build-result.json"),
    )?)?;
    let GuestResult::Fetch(fetch) = result else {
        bail!("completed 结果不是 FetchResult");
    };
    let profile_sha256 = fs::read_dir(runtime.join("profiles"))?
        .filter_map(Result::ok)
        .find_map(|entry| entry.file_name().into_string().ok())
        .context("找不到 Profile")?;
    let signing_key = SigningKey::from_bytes(&[42_u8; 32]);
    let job_id = Uuid::new_v4();
    let attempt_id = Uuid::new_v4();
    let now = Utc::now();
    let spec = JobSpec {
        job_id,
        attempt: AttemptRef {
            job_id,
            attempt_id,
            generation: 0,
        },
        required_role: WorkerRole::Builder,
        kind: JobKind::Build,
        revision_sha256: fetch.revision_sha256,
        source_manifest_sha256: Some(fetch.source_manifest_sha256),
        dependency_snapshot_sha256: Some(fetch.dependency_snapshot_sha256),
        profile_sha256: Some(profile_sha256),
        upstream_pkgrel: Some("1".into()),
        published_pkgrel: Some("1".into()),
        source_attempt_id: Some(source_attempt_id),
        dependency_attempt_ids: vec![],
        dependencies: vec![],
        inputs: vec![],
        inline_inputs: vec![],
        expected_outputs: vec!["aursmith-fetch-fixture".into()],
        allow_check: true,
        limits: ResourceLimits {
            cpu_count: 1,
            memory_mib: 1024,
            disk_mib: 4096,
            timeout_seconds: 120,
        },
        issued_at: now,
        expires_at: now + Duration::minutes(10),
    };
    let envelope = SignedEnvelope::sign("aursmith.job_spec", &spec, &signing_key)?;
    fs::write(
        runtime.join("build-envelope.json"),
        serde_json::to_vec(&envelope)?,
    )?;
    println!(
        "{}",
        serde_json::to_string(
            &json!({"job_id": job_id, "attempt_id": attempt_id, "source_attempt_id": source_attempt_id})
        )?
    );
    Ok(())
}
