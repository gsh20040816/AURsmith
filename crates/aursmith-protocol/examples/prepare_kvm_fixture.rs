use anyhow::{Context, bail};
use aursmith_domain::{AttemptRef, WorkerRole};
use aursmith_protocol::{
    BuildProfileSpec, DependencyInput, DependencySource, JobKind, JobSpec, ResourceLimits,
    SignedEnvelope,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{Duration, Utc};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{env, fs, path::Path};
use uuid::Uuid;

#[derive(Deserialize)]
struct Candidate {
    spec: BuildProfileSpec,
}

fn main() -> anyhow::Result<()> {
    let arguments = env::args().collect::<Vec<_>>();
    if !(arguments.len() == 3 || arguments.len() == 4) {
        bail!(
            "用法：prepare_kvm_fixture <Profile 导出目录> <临时运行目录> [profile_fixture|fetch|fetch_dependency]"
        );
    }
    let fixture_kind = arguments
        .get(3)
        .map(String::as_str)
        .unwrap_or("profile_fixture");
    if !matches!(
        fixture_kind,
        "profile_fixture" | "fetch" | "fetch_dependency"
    ) {
        bail!("fixture 类型只能是 profile_fixture、fetch 或 fetch_dependency");
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
    let fetch_job = matches!(fixture_kind, "fetch" | "fetch_dependency");
    let package_build = if fixture_kind == "fetch_dependency" {
        b"pkgname=aursmith-fetch-fixture\npkgver=1\npkgrel=1\narch=('any')\nmakedepends=('tree')\nsource=()\nsha256sums=()\npackage() { install -Dm644 /usr/lib/os-release \"$pkgdir/usr/share/aursmith-fetch-fixture/os-release\"; }\n".as_slice()
    } else {
        b"pkgname=aursmith-fetch-fixture\npkgver=1\npkgrel=1\narch=('any')\nsource=()\nsha256sums=()\npackage() { install -Dm644 /usr/lib/os-release \"$pkgdir/usr/share/aursmith-fetch-fixture/os-release\"; }\n".as_slice()
    };
    let package_entry = aursmith_protocol::ManifestEntry {
        path: "PKGBUILD".into(),
        sha256: hex::encode(Sha256::digest(package_build)),
        size: package_build.len() as u64,
    };
    let job = JobSpec {
        job_id,
        attempt: AttemptRef {
            job_id,
            attempt_id,
            generation: 0,
        },
        required_role: WorkerRole::Builder,
        kind: if fetch_job {
            JobKind::Fetch
        } else {
            JobKind::ProfileFixture
        },
        revision_sha256: candidate.spec.profile_sha256.clone(),
        source_manifest_sha256: Some("0".repeat(64)),
        dependency_snapshot_sha256: Some("0".repeat(64)),
        profile_sha256: Some(candidate.spec.profile_sha256.clone()),
        upstream_pkgrel: None,
        published_pkgrel: None,
        source_attempt_id: None,
        dependency_attempt_ids: vec![],
        dependencies: if fixture_kind == "fetch_dependency" {
            vec![DependencyInput {
                name: "tree".into(),
                kind: "makedepends".into(),
                source: DependencySource::Official,
            }]
        } else {
            vec![]
        },
        inputs: if fetch_job {
            vec![package_entry.clone()]
        } else {
            vec![]
        },
        inline_inputs: if fetch_job {
            vec![aursmith_protocol::InlineInput {
                entry: package_entry,
                content_base64: STANDARD.encode(package_build),
            }]
        } else {
            vec![]
        },
        expected_outputs: if fetch_job {
            vec![]
        } else {
            vec!["aursmith-profile-fixture".into()]
        },
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
            ,"fixture_kind": fixture_kind
        }))?
    );
    Ok(())
}
