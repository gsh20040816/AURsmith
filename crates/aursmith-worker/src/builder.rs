use anyhow::{Context, bail};
use aursmith_protocol::{
    BuildProfileSpec, GuestResult, JobKind, JobSpec, ManifestEntry, SignedEnvelope,
    SourceEntryKind, SourceManifestEntry,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};
use tokio::{process::Command, time::timeout};

#[derive(Clone)]
pub struct BuilderRuntime {
    profiles_dir: PathBuf,
    jobs_dir: PathBuf,
    fetch_proxy: Option<SocketAddr>,
    build_network: bool,
}

impl BuilderRuntime {
    pub fn new(profiles_dir: PathBuf, jobs_dir: PathBuf, fetch_proxy: Option<SocketAddr>) -> Self {
        Self {
            profiles_dir,
            jobs_dir,
            fetch_proxy,
            build_network: false,
        }
    }

    pub fn with_build_network(mut self, enabled: bool) -> Self {
        self.build_network = enabled;
        self
    }

    pub fn available_profiles(&self) -> Vec<String> {
        let mut profiles = fs::read_dir(&self.profiles_dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name.len() == 64 && name.chars().all(|value| value.is_ascii_hexdigit()))
            .collect::<Vec<_>>();
        profiles.sort();
        profiles
    }

    pub fn jobs_dir(&self) -> &Path {
        &self.jobs_dir
    }

    pub fn completed_result_json(&self, attempt_id: &str) -> anyhow::Result<String> {
        if uuid::Uuid::parse_str(attempt_id).is_err() {
            bail!("INVALID_ATTEMPT_ID");
        }
        fs::read_to_string(
            self.jobs_dir
                .join("completed")
                .join(attempt_id)
                .join("output/build-result.json"),
        )
        .context("COMPLETED_RESULT_MISSING")
    }

    pub fn completed_evidence_files(&self, attempt_id: &str) -> anyhow::Result<Vec<ManifestEntry>> {
        if uuid::Uuid::parse_str(attempt_id).is_err() {
            bail!("INVALID_ATTEMPT_ID");
        }
        let relative_root = PathBuf::from("evidence").join(attempt_id);
        let root = self
            .jobs_dir
            .join("completed")
            .join(attempt_id)
            .join("output")
            .join(&relative_root);
        if !root.is_dir() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::new();
        for name in ["profile.tar.zst", "source.tar.zst", "build-records.tar.zst"] {
            let path = root.join(name);
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("EVIDENCE_FILE_MISSING:{name}"))?;
            if !metadata.file_type().is_file() || metadata.len() == 0 {
                bail!("EVIDENCE_FILE_INVALID:{name}");
            }
            entries.push(ManifestEntry {
                path: relative_root.join(name).to_string_lossy().into_owned(),
                sha256: digest_file(&path)?,
                size: metadata.len(),
            });
        }
        Ok(entries)
    }

    pub fn attempt_logs(
        &self,
        attempt_id: &str,
        succeeded: bool,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        const MAX_CONTENT_PER_FILE: u64 = 128 * 1024;
        const MAX_HASH_FILE_SIZE: u64 = 64 * 1024 * 1024;
        if uuid::Uuid::parse_str(attempt_id).is_err() {
            bail!("INVALID_ATTEMPT_ID");
        }
        let root = self
            .jobs_dir
            .join(if succeeded { "completed" } else { "failed" })
            .join(attempt_id);
        let mut logs = Vec::new();
        let candidates: &[(&str, &str)] = if succeeded {
            &[
                ("qemu.stdout.log", "qemu.stdout.log"),
                ("qemu.stderr.log", "qemu.stderr.log"),
                ("output/fetch.log", "output/fetch.log"),
                ("output/build.log", "output/build.log"),
                ("output/namcap.log", "output/namcap.log"),
            ]
        } else {
            &[
                ("qemu.stdout.log", "qemu.stdout.log"),
                ("qemu.stderr.log", "qemu.stderr.log"),
                ("fetch.log", "output/fetch.log"),
                ("build.log", "output/build.log"),
                ("guest-error.json", "output/guest-error.json"),
            ]
        };
        for (source_path, evidence_path) in candidates {
            let file = root.join(source_path);
            let Ok(metadata) = fs::symlink_metadata(&file) else {
                continue;
            };
            if !metadata.file_type().is_file() {
                bail!("ATTEMPT_LOG_NOT_REGULAR:{evidence_path}");
            }
            if metadata.len() > MAX_HASH_FILE_SIZE {
                logs.push(serde_json::json!({
                    "path": evidence_path,
                    "size": metadata.len(),
                    "sha256": null,
                    "truncated": true,
                    "omitted_reason": "日志超过 64 MiB，未重新读取"
                }));
                continue;
            }
            let bytes = fs::read(&file)?;
            let content_length = bytes.len().min(MAX_CONTENT_PER_FILE as usize);
            let bounded_content = &bytes[..content_length];
            logs.push(serde_json::json!({
                "path": evidence_path,
                "size": metadata.len(),
                "sha256": hex::encode(Sha256::digest(&bytes)),
                "truncated": metadata.len() > MAX_CONTENT_PER_FILE,
                "content_base64": STANDARD.encode(bounded_content),
                "content_utf8": std::str::from_utf8(bounded_content).ok()
            }));
        }
        Ok(logs)
    }

    pub fn materialize_inline_inputs(&self, spec: &JobSpec) -> anyhow::Result<()> {
        const MAX_FILE_COUNT: usize = 256;
        const MAX_TOTAL_SIZE: u64 = 4 * 1024 * 1024;
        if spec.inline_inputs.len() > MAX_FILE_COUNT {
            bail!("TOO_MANY_INLINE_INPUTS");
        }
        let total_size = spec.inline_inputs.iter().try_fold(0_u64, |total, input| {
            total
                .checked_add(input.entry.size)
                .context("INLINE_INPUT_TOO_LARGE")
        })?;
        if total_size > MAX_TOTAL_SIZE {
            bail!("INLINE_INPUT_TOO_LARGE");
        }

        let staging = self
            .jobs_dir
            .join("staging")
            .join(spec.attempt.attempt_id.to_string());
        fs::create_dir_all(self.jobs_dir.join("staging"))?;
        fs::create_dir(&staging).context("STAGING_ALREADY_EXISTS")?;
        let input_root = staging.join("input");
        fs::create_dir(&input_root)?;
        let materialized = (|| -> anyhow::Result<()> {
            for input in &spec.inline_inputs {
                aursmith_protocol::validate_relative_path(&input.entry.path)?;
                if input.entry.path == ".aursmith" || input.entry.path.starts_with(".aursmith/") {
                    bail!("INPUT_RESERVED_PATH:{}", input.entry.path);
                }
                let bytes = STANDARD
                    .decode(&input.content_base64)
                    .context("INLINE_INPUT_INVALID_BASE64")?;
                if bytes.len() as u64 != input.entry.size
                    || hex::encode(Sha256::digest(&bytes)) != input.entry.sha256
                {
                    bail!("INLINE_INPUT_DIGEST_MISMATCH:{}", input.entry.path);
                }
                let path = input_root.join(&input.entry.path);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut options = fs::OpenOptions::new();
                options.write(true).create_new(true);
                use std::io::Write;
                options.open(path)?.write_all(&bytes)?;
            }
            verify_inputs(&input_root, &spec.inputs)
        })();
        if materialized.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        materialized
    }

    pub fn materialize_prepared_source(&self, spec: &JobSpec) -> anyhow::Result<()> {
        let source_attempt = spec.source_attempt_id.context("SOURCE_ATTEMPT_MISSING")?;
        let completed = self
            .jobs_dir
            .join("completed")
            .join(source_attempt.to_string());
        let raw_result = fs::read(completed.join("output/build-result.json"))?;
        let GuestResult::Fetch(fetch) = serde_json::from_slice(&raw_result)? else {
            bail!("SOURCE_ATTEMPT_NOT_FETCH");
        };
        if Some(fetch.source_manifest_sha256.as_str()) != spec.source_manifest_sha256.as_deref() {
            bail!("SOURCE_MANIFEST_MISMATCH");
        }
        validate_source_entries(&completed.join("output"), &fetch.sources)?;
        let staging = self
            .jobs_dir
            .join("staging")
            .join(spec.attempt.attempt_id.to_string());
        fs::create_dir_all(self.jobs_dir.join("staging"))?;
        fs::create_dir(&staging).context("STAGING_ALREADY_EXISTS")?;
        let input_root = staging.join("input");
        fs::create_dir(&input_root)?;
        let copied = copy_prepared_tree(&completed.join("output/prepared"), &input_root);
        let copied = copied.and_then(|()| {
            let dependency_root = input_root.join(".aursmith-batch-dependencies");
            for dependency_attempt in &spec.dependency_attempt_ids {
                let dependency_output = self
                    .jobs_dir
                    .join("completed")
                    .join(dependency_attempt.to_string())
                    .join("output");
                let raw = fs::read(dependency_output.join("build-result.json"))?;
                let GuestResult::Build(result) = serde_json::from_slice(&raw)? else {
                    bail!("DEPENDENCY_ATTEMPT_NOT_BUILD");
                };
                validate_output_entries(
                    &dependency_output,
                    &result
                        .artifacts
                        .iter()
                        .map(|artifact| ManifestEntry {
                            path: artifact.path.clone(),
                            sha256: artifact.sha256.clone(),
                            size: artifact.size,
                        })
                        .collect::<Vec<_>>(),
                )?;
                fs::create_dir_all(&dependency_root)?;
                for artifact in result.artifacts {
                    let name = Path::new(&artifact.path)
                        .file_name()
                        .context("DEPENDENCY_ARTIFACT_NAME_INVALID")?;
                    let destination = dependency_root.join(name);
                    if destination.exists() {
                        bail!("DEPENDENCY_ARTIFACT_COLLISION");
                    }
                    fs::copy(dependency_output.join(&artifact.path), destination)?;
                }
            }
            Ok(())
        });
        if copied.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        copied
    }
}

fn copy_prepared_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    for item in fs::read_dir(source)? {
        let item = item?;
        let target = destination.join(item.file_name());
        let metadata = fs::symlink_metadata(item.path())?;
        if metadata.file_type().is_dir() {
            fs::create_dir(&target)?;
            copy_prepared_tree(&item.path(), &target)?;
        } else if metadata.file_type().is_file() {
            fs::copy(item.path(), target)?;
        } else if metadata.file_type().is_symlink() {
            let link = fs::read_link(item.path())?;
            if link.is_absolute()
                || link.components().any(|part| {
                    matches!(
                        part,
                        std::path::Component::ParentDir
                            | std::path::Component::RootDir
                            | std::path::Component::Prefix(_)
                    )
                })
            {
                bail!("SOURCE_LINK_ESCAPE");
            }
            std::os::unix::fs::symlink(link, target)?;
        } else {
            bail!("SOURCE_SPECIAL_FILE");
        }
    }
    Ok(())
}

pub fn spawn(database: SqlitePool, controller_key: Vec<u8>, runtime: BuilderRuntime) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = execute_one(&database, &controller_key, &runtime).await {
                tracing::warn!(%error, "Builder 执行任务失败");
            }
        }
    });
}

async fn execute_one(
    database: &SqlitePool,
    controller_key: &[u8],
    runtime: &BuilderRuntime,
) -> anyhow::Result<()> {
    let row = sqlx::query("SELECT job_id, attempt_id, spec_json FROM attempts WHERE status = 'queued' ORDER BY received_at LIMIT 1")
        .fetch_optional(database).await?;
    let Some(row) = row else { return Ok(()) };
    let attempt_id: String = row.get("attempt_id");
    let claimed = sqlx::query(
        "UPDATE attempts SET status = 'running' WHERE attempt_id = ? AND status = 'queued'",
    )
    .bind(&attempt_id)
    .execute(database)
    .await?;
    if claimed.rows_affected() == 0 {
        return Ok(());
    }
    let result = execute_attempt(
        controller_key,
        runtime,
        row.get::<String, _>("spec_json").as_str(),
        &attempt_id,
    )
    .await;
    let _ = fs::remove_dir_all(runtime.jobs_dir.join("staging").join(&attempt_id));
    if result.is_err() {
        persist_failure_diagnostics(runtime, &attempt_id);
        let _ = fs::remove_dir_all(runtime.jobs_dir.join("runtime").join(&attempt_id));
    }
    match result {
        Ok(result_sha256) => {
            sqlx::query("UPDATE attempts SET status = 'succeeded', result_sha256 = ?, failure_code = NULL WHERE attempt_id = ? AND status = 'running'")
                .bind(result_sha256).bind(&attempt_id).execute(database).await?;
        }
        Err(error) => {
            let code = classify_failure(&error);
            sqlx::query("UPDATE attempts SET status = 'failed', failure_code = ? WHERE attempt_id = ? AND status = 'running'")
                .bind(code).bind(&attempt_id).execute(database).await?;
            tracing::warn!(attempt_id, failure_code = code, %error, "Builder Attempt 失败");
        }
    }
    Ok(())
}

fn persist_failure_diagnostics(runtime: &BuilderRuntime, attempt_id: &str) {
    let work = runtime.jobs_dir.join("runtime").join(attempt_id);
    let failed = runtime.jobs_dir.join("failed").join(attempt_id);
    let candidates = [
        (work.join("qemu.stdout.log"), "qemu.stdout.log"),
        (work.join("qemu.stderr.log"), "qemu.stderr.log"),
        (work.join("output/guest-error.json"), "guest-error.json"),
        (work.join("output/fetch.log"), "fetch.log"),
        (work.join("output/build.log"), "build.log"),
    ];
    for (source, name) in candidates {
        if source.is_file() {
            let _ = fs::create_dir_all(&failed);
            let _ = fs::copy(source, failed.join(name));
        }
    }
}

async fn execute_attempt(
    controller_key: &[u8],
    runtime: &BuilderRuntime,
    envelope_json: &str,
    attempt_id: &str,
) -> anyhow::Result<String> {
    let envelope: SignedEnvelope = serde_json::from_str(envelope_json)?;
    if envelope.verifying_key != controller_key {
        bail!("UNTRUSTED_CONTROLLER")
    }
    let spec: JobSpec = envelope.verify("aursmith.job_spec")?;
    if spec.attempt.attempt_id.to_string() != attempt_id {
        bail!("ATTEMPT_MISMATCH")
    }
    let profile_sha = spec.profile_sha256.as_deref().context("PROFILE_MISSING")?;
    let profile = VerifiedProfile::load(&runtime.profiles_dir.join(profile_sha), controller_key)?;
    if profile.spec.profile_sha256 != profile_sha {
        bail!("PROFILE_DIGEST_MISMATCH")
    }
    let staging = runtime.jobs_dir.join("staging").join(attempt_id);
    verify_inputs(&staging.join("input"), &spec.inputs)?;
    let control_input = staging.join("input/.aursmith");
    fs::create_dir_all(&control_input)?;
    fs::write(
        control_input.join("job-envelope.json"),
        envelope_json.as_bytes(),
    )?;
    let work = runtime.jobs_dir.join("runtime").join(attempt_id);
    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir_all(work.join("output"))?;
    let overlay = work.join("overlay.qcow2");
    run_checked(
        "/usr/bin/qemu-img",
        &[
            "create".into(),
            "-f".into(),
            "qcow2".into(),
            "-F".into(),
            "qcow2".into(),
            "-b".into(),
            profile.root_image.as_os_str().into(),
            overlay.as_os_str().into(),
        ],
        std::time::Duration::from_secs(30),
    )
    .await?;
    let paths = VmPaths {
        overlay,
        input_directory: staging.join("input"),
        output_directory: work.join("output"),
        control_socket: work.join("control.sock"),
    };
    if spec.kind == JobKind::Fetch && runtime.fetch_proxy.is_none() {
        bail!("Fetch VM 必须配置唯一源码代理");
    }
    let plan = QemuPlan::for_job(
        &profile,
        &spec,
        paths,
        runtime.fetch_proxy,
        runtime.build_network,
    )?;
    let qemu = Command::new(plan.executable)
        .args(&plan.arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let execution = timeout(
        std::time::Duration::from_secs(spec.limits.timeout_seconds),
        qemu.wait_with_output(),
    )
    .await;
    let execution = match execution {
        Ok(result) => result?,
        Err(_) => bail!("VM_TIMEOUT"),
    };
    fs::write(work.join("qemu.stdout.log"), &execution.stdout)?;
    fs::write(work.join("qemu.stderr.log"), &execution.stderr)?;
    if !execution.status.success() {
        let failed = runtime.jobs_dir.join("failed").join(attempt_id);
        fs::create_dir_all(&failed)?;
        fs::write(failed.join("qemu.stdout.log"), &execution.stdout)?;
        fs::write(failed.join("qemu.stderr.log"), &execution.stderr)?;
        if work.join("output/guest-error.json").is_file() {
            let code = classify_guest_failure(&spec, &work.join("output"));
            bail!("{code}")
        }
        let diagnostic = String::from_utf8_lossy(&execution.stderr);
        let diagnostic = diagnostic.chars().take(512).collect::<String>();
        bail!("VM_FAILED:{diagnostic}")
    }
    let result_path = work.join("output/build-result.json");
    if !result_path.is_file() && work.join("output/guest-error.json").is_file() {
        let code = classify_guest_failure(&spec, &work.join("output"));
        bail!("{code}")
    }
    let result = fs::read(&result_path).context("GUEST_RESULT_MISSING")?;
    let guest_result: GuestResult =
        serde_json::from_slice(&result).context("GUEST_RESULT_INVALID")?;
    validate_guest_result(&guest_result, &spec, &work.join("output"))?;
    if spec.kind == JobKind::Build {
        create_build_evidence_archives(runtime, &spec, &work, &staging).await?;
    }
    let digest = hex::encode(Sha256::digest(&result));
    fs::remove_file(work.join("overlay.qcow2")).context("OVERLAY_CLEANUP_FAILED")?;
    if work.join("control.sock").exists() {
        fs::remove_file(work.join("control.sock")).context("CONTROL_SOCKET_CLEANUP_FAILED")?;
    }
    let completed = runtime.jobs_dir.join("completed").join(attempt_id);
    fs::create_dir_all(runtime.jobs_dir.join("completed"))?;
    fs::rename(&work, &completed)?;
    Ok(digest)
}

async fn create_build_evidence_archives(
    runtime: &BuilderRuntime,
    spec: &JobSpec,
    work: &Path,
    staging: &Path,
) -> anyhow::Result<()> {
    let source_attempt = spec.source_attempt_id.context("SOURCE_ATTEMPT_MISSING")?;
    let profile_sha = spec.profile_sha256.as_deref().context("PROFILE_MISSING")?;
    let destination = work
        .join("output/evidence")
        .join(spec.attempt.attempt_id.to_string());
    fs::create_dir_all(&destination)?;

    create_archive(
        &destination.join("profile.tar.zst"),
        &runtime.profiles_dir.join(profile_sha),
        &["."],
    )
    .await
    .context("PROFILE_EVIDENCE_ARCHIVE_FAILED")?;
    create_archive(
        &destination.join("source.tar.zst"),
        &runtime
            .jobs_dir
            .join("completed")
            .join(source_attempt.to_string()),
        &["."],
    )
    .await
    .context("SOURCE_EVIDENCE_ARCHIVE_FAILED")?;

    let records = work.join("evidence-records");
    fs::create_dir(&records)?;
    for (source, name) in [
        (work.join("qemu.stdout.log"), "qemu.stdout.log"),
        (work.join("qemu.stderr.log"), "qemu.stderr.log"),
        (work.join("output/build.log"), "build.log"),
        (work.join("output/namcap.log"), "namcap.log"),
        (work.join("output/build-result.json"), "build-result.json"),
        (
            staging.join("input/.aursmith/job-envelope.json"),
            "job-envelope.json",
        ),
    ] {
        if source.is_file() {
            fs::copy(source, records.join(name))?;
        }
    }
    create_archive(&destination.join("build-records.tar.zst"), &records, &["."])
        .await
        .context("BUILD_RECORDS_ARCHIVE_FAILED")?;
    fs::remove_dir_all(records)?;
    Ok(())
}

async fn create_archive(destination: &Path, root: &Path, entries: &[&str]) -> anyhow::Result<()> {
    if !root.is_dir() || destination.exists() {
        bail!("EVIDENCE_ARCHIVE_PATH_INVALID");
    }
    let temporary = destination.with_extension("tar.zst.partial");
    let mut arguments = vec![
        "-caf".into(),
        temporary.as_os_str().into(),
        "--format=pax".into(),
        "-C".into(),
        root.as_os_str().into(),
    ];
    arguments.extend(entries.iter().map(OsString::from));
    run_checked(
        "/usr/bin/bsdtar",
        &arguments,
        std::time::Duration::from_secs(30 * 60),
    )
    .await?;
    let metadata = fs::symlink_metadata(&temporary)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 {
        bail!("EVIDENCE_ARCHIVE_EMPTY");
    }
    fs::rename(temporary, destination)?;
    Ok(())
}

fn validate_guest_result(
    result: &GuestResult,
    spec: &JobSpec,
    output: &Path,
) -> anyhow::Result<()> {
    if !matches!(
        (spec.kind, result),
        (JobKind::Fetch, GuestResult::Fetch(_))
            | (JobKind::Build, GuestResult::Build(_))
            | (JobKind::ProfileFixture, GuestResult::ProfileFixture(_))
    ) {
        bail!("GUEST_RESULT_KIND_MISMATCH");
    }
    match result {
        GuestResult::Fetch(value) => {
            validate_result_identity(value.job_id, &value.attempt, &value.revision_sha256, spec)?;
            validate_source_entries(output, &value.sources)
        }
        GuestResult::Build(value) | GuestResult::ProfileFixture(value) => {
            validate_result_identity(value.job_id, &value.attempt, &value.revision_sha256, spec)?;
            let entries = value
                .artifacts
                .iter()
                .map(|artifact| ManifestEntry {
                    path: artifact.path.clone(),
                    sha256: artifact.sha256.clone(),
                    size: artifact.size,
                })
                .collect::<Vec<_>>();
            validate_output_entries(output, &entries)
        }
    }
}

fn validate_source_entries(output: &Path, entries: &[SourceManifestEntry]) -> anyhow::Result<()> {
    for entry in entries {
        aursmith_protocol::validate_relative_path(&entry.path)?;
        let path = output.join(&entry.path);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("GUEST_SOURCE_MISSING:{}", entry.path))?;
        let valid = match entry.kind {
            SourceEntryKind::File => {
                metadata.file_type().is_file()
                    && metadata.len() == entry.size
                    && entry.sha256.as_deref() == Some(digest_file(&path)?.as_str())
                    && entry.link_target.is_none()
            }
            SourceEntryKind::Directory => {
                metadata.file_type().is_dir()
                    && entry.size == 0
                    && entry.sha256.is_none()
                    && entry.link_target.is_none()
            }
            SourceEntryKind::Symlink => {
                metadata.file_type().is_symlink()
                    && entry.size == 0
                    && entry.sha256.is_none()
                    && entry.link_target.as_deref() == fs::read_link(&path)?.to_str()
            }
        };
        if !valid {
            bail!("GUEST_SOURCE_MISMATCH:{}", entry.path);
        }
    }
    Ok(())
}

fn validate_result_identity(
    job_id: uuid::Uuid,
    attempt: &aursmith_domain::AttemptRef,
    revision: &str,
    spec: &JobSpec,
) -> anyhow::Result<()> {
    if job_id != spec.job_id || attempt != &spec.attempt || revision != spec.revision_sha256 {
        bail!("GUEST_RESULT_IDENTITY_MISMATCH");
    }
    Ok(())
}

fn validate_output_entries(output: &Path, entries: &[ManifestEntry]) -> anyhow::Result<()> {
    for entry in entries {
        aursmith_protocol::validate_relative_path(&entry.path)?;
        if entry.path == ".aursmith" || entry.path.starts_with(".aursmith/") {
            bail!("INPUT_RESERVED_PATH:{}", entry.path);
        }
        let path = output.join(&entry.path);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("GUEST_ARTIFACT_MISSING:{}", entry.path))?;
        if !metadata.file_type().is_file()
            || metadata.len() != entry.size
            || digest_file(&path)? != entry.sha256
        {
            bail!("GUEST_ARTIFACT_MISMATCH:{}", entry.path);
        }
    }
    Ok(())
}

fn verify_inputs(root: &Path, entries: &[ManifestEntry]) -> anyhow::Result<()> {
    for entry in entries {
        aursmith_protocol::validate_relative_path(&entry.path)?;
        let path = root.join(&entry.path);
        let metadata =
            fs::symlink_metadata(&path).with_context(|| format!("INPUT_MISSING:{}", entry.path))?;
        if !metadata.file_type().is_file() || metadata.len() != entry.size {
            bail!("INPUT_METADATA_MISMATCH:{}", entry.path)
        }
        if digest_file(&path)? != entry.sha256 {
            bail!("INPUT_DIGEST_MISMATCH:{}", entry.path)
        }
    }
    Ok(())
}

async fn run_checked(
    executable: &str,
    arguments: &[OsString],
    deadline: std::time::Duration,
) -> anyhow::Result<()> {
    let output = timeout(deadline, Command::new(executable).args(arguments).output())
        .await
        .context("子进程超时")??;
    if !output.status.success() {
        bail!("子进程失败：{}", executable)
    }
    Ok(())
}

fn classify_failure(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    for code in [
        "PROFILE_MISSING",
        "PROFILE_DIGEST_MISMATCH",
        "ATTEMPT_MISMATCH",
        "VM_TIMEOUT",
        "VM_FAILED",
        "GUEST_RESULT_MISSING",
        "GUEST_RESULT_INVALID",
        "GUEST_RESULT_IDENTITY_MISMATCH",
        "GUEST_RESULT_KIND_MISMATCH",
        "GUEST_BUILD_FAILED",
        "GUEST_FETCH_FAILED",
        "NETWORK_DURING_BUILD",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    if message.contains("INPUT_") {
        "INPUT_INVALID"
    } else {
        "BUILDER_INFRASTRUCTURE"
    }
}

fn classify_guest_failure(spec: &JobSpec, output: &Path) -> &'static str {
    if spec.kind == JobKind::Build
        && fs::read(output.join("build.log"))
            .ok()
            .filter(|bytes| bytes.len() <= 16 * 1024 * 1024)
            .is_some_and(|bytes| network_failure_in_log(&bytes))
    {
        "NETWORK_DURING_BUILD"
    } else if spec.kind == JobKind::Fetch {
        "GUEST_FETCH_FAILED"
    } else {
        "GUEST_BUILD_FAILED"
    }
}

fn network_failure_in_log(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    [
        "could not resolve host",
        "temporary failure in name resolution",
        "network is unreachable",
        "failed to connect",
        "connection timed out",
        "error nu1301",
        "unable to load the service index for source",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub struct VerifiedProfile {
    pub spec: BuildProfileSpec,
    pub root_image: PathBuf,
    pub kernel: PathBuf,
    pub initramfs: PathBuf,
    pub controller_key_hex: String,
}

pub struct VmPaths {
    pub overlay: PathBuf,
    pub input_directory: PathBuf,
    pub output_directory: PathBuf,
    pub control_socket: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub struct QemuPlan {
    pub executable: &'static str,
    pub arguments: Vec<OsString>,
}

impl VerifiedProfile {
    pub fn load(directory: &Path, controller_key: &[u8]) -> anyhow::Result<Self> {
        let envelope_path = directory.join("profile-envelope.json");
        let envelope: SignedEnvelope = serde_json::from_slice(
            &fs::read(&envelope_path)
                .with_context(|| format!("无法读取 {}", envelope_path.display()))?,
        )
        .context("Profile 授权不是有效 JSON")?;
        if envelope.verifying_key != controller_key {
            bail!("Profile 不是由当前 Controller 授权");
        }
        let spec: BuildProfileSpec = envelope.verify("aursmith.build_profile")?;
        let root_image = verify_entry(directory, &spec.root_image, "root.qcow2")?;
        let kernel = verify_entry(directory, &spec.kernel, "vmlinuz-linux")?;
        let initramfs = verify_entry(directory, &spec.initramfs, "initramfs-linux.img")?;
        let manifest_sha = spec.content_sha256()?;
        if manifest_sha != spec.profile_sha256 {
            bail!("Profile 摘要与签名 payload 不一致");
        }
        Ok(Self {
            spec,
            root_image,
            kernel,
            initramfs,
            controller_key_hex: hex::encode(controller_key),
        })
    }
}

fn verify_entry(
    directory: &Path,
    entry: &ManifestEntry,
    expected: &str,
) -> anyhow::Result<PathBuf> {
    if entry.path != expected {
        bail!("Profile 文件名必须是 {expected}");
    }
    let path = directory.join(expected);
    let metadata = fs::symlink_metadata(&path)
        .with_context(|| format!("无法检查 Profile 文件 {}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() != entry.size {
        bail!("Profile 文件类型或大小不匹配：{expected}");
    }
    let digest = digest_file(&path)?;
    if digest != entry.sha256 {
        bail!("Profile 文件摘要不匹配：{expected}");
    }
    Ok(path)
}

fn digest_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

impl QemuPlan {
    pub fn for_job(
        profile: &VerifiedProfile,
        spec: &JobSpec,
        paths: VmPaths,
        fetch_relay: Option<SocketAddr>,
        build_network: bool,
    ) -> anyhow::Result<Self> {
        if spec.required_role != aursmith_domain::WorkerRole::Builder {
            bail!("QEMU 计划只能用于 Builder Job");
        }
        if spec.limits.cpu_count == 0 || spec.limits.memory_mib < 512 {
            bail!("VM 至少需要 1 个 CPU 和 512 MiB 内存");
        }
        if spec.kind == JobKind::Fetch && fetch_relay.is_none() {
            bail!("Fetch VM 必须配置本地源码代理中继");
        }
        let memory = spec.limits.memory_mib.to_string();
        let mut arguments: Vec<OsString> = [
            "-nodefaults",
            "-no-user-config",
            "-machine",
            "q35,accel=kvm",
            "-cpu",
            "host",
            "-smp",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        arguments.push(spec.limits.cpu_count.to_string().into());
        arguments.extend([
            "-m".into(),
            format!("{memory}M").into(),
            "-object".into(),
            format!("memory-backend-memfd,id=mem,size={memory}M,share=on").into(),
            "-numa".into(),
            "node,memdev=mem".into(),
            "-nographic".into(),
            "-serial".into(),
            "stdio".into(),
            "-no-reboot".into(),
            "-kernel".into(),
            profile.kernel.as_os_str().into(),
            "-initrd".into(),
            profile.initramfs.as_os_str().into(),
            "-append".into(),
            format!(
                "root=/dev/vda rw console=ttyS0 panic=1 systemd.unit=aursmith-guest-agent.service aursmith.controller_key={} aursmith.build_network={}",
                profile.controller_key_hex,
                u8::from(spec.kind == JobKind::Build && build_network),
            )
            .into(),
            "-drive".into(),
            format!(
                "file={},if=virtio,format=qcow2,cache=none,discard=unmap",
                paths.overlay.display()
            )
            .into(),
        ]);
        add_virtio_9p(
            &mut arguments,
            "input",
            &paths.input_directory,
            "aursmith-input",
            true,
        );
        add_virtio_9p(
            &mut arguments,
            "output",
            &paths.output_directory,
            "aursmith-output",
            false,
        );
        arguments.extend([
            "-device".into(),
            "virtio-serial-pci".into(),
            "-chardev".into(),
            format!(
                "socket,id=control,path={},server=on,wait=off",
                paths.control_socket.display()
            )
            .into(),
            "-device".into(),
            "virtserialport,chardev=control,name=org.aursmith.control".into(),
        ]);
        match spec.kind {
            JobKind::Fetch => {
                fetch_relay.expect("前置校验保证存在 Fetch proxy");
                arguments.extend([
                    "-nic".into(),
                    "user,model=virtio-net-pci,restrict=on,guestfwd=tcp:10.0.2.100:8080-cmd:/usr/local/bin/aursmithctl tcp-relay".into(),
                ]);
            }
            JobKind::Build if build_network => {
                arguments.extend(["-nic".into(), "user,model=virtio-net-pci".into()]);
            }
            JobKind::Build | JobKind::ProfileFixture => {
                arguments.extend(["-nic".into(), "none".into()]);
            }
        }
        Ok(Self {
            executable: "/usr/bin/qemu-system-x86_64",
            arguments,
        })
    }
}

fn add_virtio_9p(
    arguments: &mut Vec<OsString>,
    id: &str,
    directory: &Path,
    tag: &str,
    readonly: bool,
) {
    let mut fsdev = format!(
        "local,id={id},path={},security_model=mapped-xattr,multidevs=remap",
        directory.display()
    );
    if readonly {
        fsdev.push_str(",readonly=on");
    }
    arguments.extend([
        "-fsdev".into(),
        fsdev.into(),
        "-device".into(),
        format!("virtio-9p-pci,fsdev={id},mount_tag={tag}").into(),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use aursmith_domain::{AttemptRef, WorkerRole};
    use aursmith_protocol::ResourceLimits;
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    fn entry(path: &str, bytes: &[u8]) -> ManifestEntry {
        ManifestEntry {
            path: path.into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            size: u64::try_from(bytes.len()).unwrap(),
        }
    }

    fn profile(root: &Path) -> VerifiedProfile {
        VerifiedProfile {
            spec: BuildProfileSpec {
                profile_sha256: "a".repeat(64),
                root_image: ManifestEntry {
                    path: "root.qcow2".into(),
                    sha256: "b".repeat(64),
                    size: 1,
                },
                kernel: ManifestEntry {
                    path: "vmlinuz-linux".into(),
                    sha256: "c".repeat(64),
                    size: 1,
                },
                initramfs: ManifestEntry {
                    path: "initramfs-linux.img".into(),
                    sha256: "d".repeat(64),
                    size: 1,
                },
                installed_packages: vec![],
                repository_mirror: None,
                created_at: Utc::now(),
            },
            root_image: root.join("root.qcow2"),
            kernel: root.join("vmlinuz-linux"),
            initramfs: root.join("initramfs-linux.img"),
            controller_key_hex: "00".repeat(32),
        }
    }

    fn job(kind: JobKind) -> JobSpec {
        let job_id = Uuid::new_v4();
        JobSpec {
            job_id,
            attempt: AttemptRef {
                job_id,
                attempt_id: Uuid::new_v4(),
                generation: 0,
            },
            required_role: WorkerRole::Builder,
            kind,
            revision_sha256: "e".repeat(64),
            source_manifest_sha256: None,
            dependency_snapshot_sha256: None,
            profile_sha256: Some("a".repeat(64)),
            upstream_pkgrel: None,
            published_pkgrel: None,
            source_attempt_id: None,
            dependency_attempt_ids: vec![],
            dependencies: vec![],
            inputs: vec![],
            inline_inputs: vec![],
            expected_outputs: vec![],
            allow_check: true,
            limits: ResourceLimits {
                cpu_count: 2,
                memory_mib: 1024,
                disk_mib: 4096,
                timeout_seconds: 600,
            },
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        }
    }

    fn paths(root: &Path) -> VmPaths {
        VmPaths {
            overlay: root.join("overlay.qcow2"),
            input_directory: root.join("input"),
            output_directory: root.join("output"),
            control_socket: root.join("control.sock"),
        }
    }

    #[test]
    fn build_vm_has_no_network_device() {
        let root = Path::new("/jobs/attempt");
        let plan = QemuPlan::for_job(
            &profile(root),
            &job(JobKind::Build),
            paths(root),
            None,
            false,
        )
        .unwrap();
        let args: Vec<_> = plan
            .arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect();
        assert!(args.windows(2).any(|pair| pair == ["-nic", "none"]));
        assert!(args.windows(2).any(|pair| pair == ["-m", "1024M"]));
        assert!(
            args.iter()
                .any(|value| value.contains("size=1024M,share=on"))
        );
        assert!(!args.iter().any(|value| value.contains("guestfwd")));
        assert!(
            args.iter()
                .any(|value| value.contains("id=input") && value.contains("readonly=on"))
        );
        assert!(
            args.iter()
                .any(|value| value.contains("id=output") && !value.contains("readonly=on"))
        );
    }

    #[test]
    fn build_vm_can_use_direct_network_when_enabled() {
        let root = Path::new("/jobs/attempt");
        let plan = QemuPlan::for_job(
            &profile(root),
            &job(JobKind::Build),
            paths(root),
            None,
            true,
        )
        .unwrap();
        let args: Vec<_> = plan
            .arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect();
        assert!(
            args.windows(2)
                .any(|pair| pair == ["-nic", "user,model=virtio-net-pci"])
        );
        assert!(
            args.iter()
                .any(|value| value.contains("aursmith.build_network=1"))
        );
        assert!(args.iter().any(|value| {
            value.contains("systemd.unit=aursmith-guest-agent.service")
                && !value.contains("init=/usr/local/bin/aursmith-guest-agent")
        }));
    }

    #[test]
    fn fetch_vm_can_only_reach_the_fixed_proxy_forward() {
        let root = Path::new("/jobs/attempt");
        let proxy: SocketAddr = "127.0.0.1:3129".parse().unwrap();
        let plan = QemuPlan::for_job(
            &profile(root),
            &job(JobKind::Fetch),
            paths(root),
            Some(proxy),
            false,
        )
        .unwrap();
        let args: Vec<_> = plan
            .arguments
            .iter()
            .map(|value| value.to_string_lossy())
            .collect();
        assert!(args.iter().any(|value| value.as_ref()
            == "user,model=virtio-net-pci,restrict=on,guestfwd=tcp:10.0.2.100:8080-cmd:/usr/local/bin/aursmithctl tcp-relay"));
        assert!(!args.iter().any(|value| value.contains("127.0.0.1:3129")));
        assert!(!args.windows(2).any(|pair| pair == ["-nic", "none"]));
    }

    #[test]
    fn inline_inputs_are_verified_before_materialization() {
        let root = tempfile::tempdir().unwrap();
        let runtime =
            BuilderRuntime::new(root.path().join("profiles"), root.path().join("jobs"), None);
        let mut spec = job(JobKind::Fetch);
        let content = b"pkgname=aursmith-fixture\n";
        let manifest = entry("snapshot/PKGBUILD", content);
        spec.inputs.push(manifest.clone());
        spec.inline_inputs.push(aursmith_protocol::InlineInput {
            entry: manifest,
            content_base64: STANDARD.encode(content),
        });

        runtime.materialize_inline_inputs(&spec).unwrap();
        let path = runtime
            .jobs_dir
            .join("staging")
            .join(spec.attempt.attempt_id.to_string())
            .join("input/snapshot/PKGBUILD");
        assert_eq!(fs::read(path).unwrap(), content);
    }

    #[test]
    fn invalid_inline_input_is_removed_without_partial_staging() {
        let root = tempfile::tempdir().unwrap();
        let runtime =
            BuilderRuntime::new(root.path().join("profiles"), root.path().join("jobs"), None);
        let mut spec = job(JobKind::Fetch);
        spec.inputs.push(entry("PKGBUILD", b"trusted"));
        spec.inline_inputs.push(aursmith_protocol::InlineInput {
            entry: entry("PKGBUILD", b"trusted"),
            content_base64: STANDARD.encode(b"tampered"),
        });

        let error = runtime.materialize_inline_inputs(&spec).unwrap_err();
        assert!(error.to_string().contains("INLINE_INPUT_DIGEST_MISMATCH"));
        assert!(
            !runtime
                .jobs_dir
                .join("staging")
                .join(spec.attempt.attempt_id.to_string())
                .exists()
        );
    }

    #[test]
    fn build_source_is_bound_to_a_completed_fetch_attempt() {
        let root = tempfile::tempdir().unwrap();
        let runtime =
            BuilderRuntime::new(root.path().join("profiles"), root.path().join("jobs"), None);
        let source_attempt = Uuid::new_v4();
        let output = runtime
            .jobs_dir
            .join("completed")
            .join(source_attempt.to_string())
            .join("output");
        fs::create_dir_all(output.join("prepared")).unwrap();
        fs::write(output.join("prepared/PKGBUILD"), b"pkgname=fixture\n").unwrap();
        let source_manifest = "a".repeat(64);
        let source_job = Uuid::new_v4();
        let result = GuestResult::Fetch(aursmith_protocol::FetchResult {
            job_id: source_job,
            attempt: AttemptRef {
                job_id: source_job,
                attempt_id: source_attempt,
                generation: 0,
            },
            revision_sha256: "b".repeat(64),
            source_manifest_sha256: source_manifest.clone(),
            sources: vec![
                SourceManifestEntry {
                    path: "prepared".into(),
                    kind: SourceEntryKind::Directory,
                    sha256: None,
                    size: 0,
                    link_target: None,
                },
                SourceManifestEntry {
                    path: "prepared/PKGBUILD".into(),
                    kind: SourceEntryKind::File,
                    sha256: Some(hex::encode(Sha256::digest(b"pkgname=fixture\n"))),
                    size: 16,
                    link_target: None,
                },
            ],
            audit_files: vec![],
            resolved_dependencies: vec![],
            dependency_download_milliseconds: 0,
            resolved_pkgver: None,
            dependency_snapshot_sha256: "c".repeat(64),
            log_sha256: "d".repeat(64),
            finished_at: Utc::now(),
        });
        fs::write(
            output.join("build-result.json"),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        let mut spec = job(JobKind::Build);
        spec.source_attempt_id = Some(source_attempt);
        spec.source_manifest_sha256 = Some(source_manifest);
        let dependency_attempt = Uuid::new_v4();
        let dependency_output = runtime
            .jobs_dir
            .join("completed")
            .join(dependency_attempt.to_string())
            .join("output");
        fs::create_dir_all(&dependency_output).unwrap();
        let dependency_bytes = b"batch dependency";
        let dependency_name = "dependency-1-1-any.pkg.tar.zst";
        fs::write(dependency_output.join(dependency_name), dependency_bytes).unwrap();
        let dependency_job = Uuid::new_v4();
        let dependency_result = GuestResult::Build(aursmith_protocol::BuildResult {
            job_id: dependency_job,
            attempt: AttemptRef {
                job_id: dependency_job,
                attempt_id: dependency_attempt,
                generation: 0,
            },
            revision_sha256: "e".repeat(64),
            source_manifest_sha256: "f".repeat(64),
            dependency_snapshot_sha256: "1".repeat(64),
            profile_sha256: "a".repeat(64),
            artifacts: vec![aursmith_protocol::ArtifactRecord {
                path: dependency_name.into(),
                sha256: hex::encode(Sha256::digest(dependency_bytes)),
                size: dependency_bytes.len() as u64,
                package_name: Some("dependency".into()),
                package_version: Some("1-1".into()),
                architecture: Some("any".into()),
            }],
            provenance: Default::default(),
            log_sha256: "2".repeat(64),
            finished_at: Utc::now(),
        });
        fs::write(
            dependency_output.join("build-result.json"),
            serde_json::to_vec(&dependency_result).unwrap(),
        )
        .unwrap();
        spec.dependency_attempt_ids.push(dependency_attempt);

        runtime.materialize_prepared_source(&spec).unwrap();
        let copied = runtime
            .jobs_dir
            .join("staging")
            .join(spec.attempt.attempt_id.to_string())
            .join("input/PKGBUILD");
        assert_eq!(fs::read(copied).unwrap(), b"pkgname=fixture\n");
        assert_eq!(
            fs::read(
                runtime
                    .jobs_dir
                    .join("staging")
                    .join(spec.attempt.attempt_id.to_string())
                    .join("input/.aursmith-batch-dependencies")
                    .join(dependency_name)
            )
            .unwrap(),
            dependency_bytes
        );
    }

    #[test]
    fn signed_profile_rejects_tampered_root_image() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("root.qcow2"), b"root").unwrap();
        fs::write(directory.path().join("vmlinuz-linux"), b"kernel").unwrap();
        fs::write(directory.path().join("initramfs-linux.img"), b"initramfs").unwrap();
        let mut spec = BuildProfileSpec {
            profile_sha256: String::new(),
            root_image: entry("root.qcow2", b"root"),
            kernel: entry("vmlinuz-linux", b"kernel"),
            initramfs: entry("initramfs-linux.img", b"initramfs"),
            installed_packages: vec!["base-devel=1".into()],
            repository_mirror: Some("https://geo.mirror.pkgbuild.com".into()),
            created_at: Utc::now(),
        };
        spec.profile_sha256 = spec.content_sha256().unwrap();
        let signing_key = ed25519_dalek::SigningKey::from_bytes(&[9; 32]);
        let envelope = SignedEnvelope::sign("aursmith.build_profile", &spec, &signing_key).unwrap();
        fs::write(
            directory.path().join("profile-envelope.json"),
            serde_json::to_vec(&envelope).unwrap(),
        )
        .unwrap();
        assert!(
            VerifiedProfile::load(directory.path(), signing_key.verifying_key().as_bytes()).is_ok()
        );
        fs::write(directory.path().join("root.qcow2"), b"evil").unwrap();
        assert!(
            VerifiedProfile::load(directory.path(), signing_key.verifying_key().as_bytes())
                .is_err()
        );
    }

    #[test]
    fn guest_build_failure_is_deterministic_and_network_attempt_is_named() {
        assert_eq!(
            classify_failure(&anyhow::anyhow!("GUEST_BUILD_FAILED")),
            "GUEST_BUILD_FAILED"
        );
        assert!(network_failure_in_log(
            b"curl: (6) Could not resolve host: example.org"
        ));
        assert!(network_failure_in_log(b"connect: Network is unreachable"));
        assert!(!network_failure_in_log(b"compiler error: missing header"));
    }

    #[test]
    fn completed_logs_are_hashed_and_content_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let attempt = uuid::Uuid::new_v4().to_string();
        let completed = root.path().join("completed").join(&attempt);
        fs::create_dir_all(completed.join("output")).unwrap();
        fs::write(completed.join("qemu.stdout.log"), b"qemu log").unwrap();
        fs::write(
            completed.join("output/build.log"),
            vec![b'x'; 128 * 1024 + 1],
        )
        .unwrap();
        let runtime = BuilderRuntime::new(root.path().join("profiles"), root.path().into(), None);
        let logs = runtime.attempt_logs(&attempt, true).unwrap();
        let qemu = logs
            .iter()
            .find(|log| log["path"] == "qemu.stdout.log")
            .unwrap();
        assert_eq!(qemu["sha256"], hex::encode(Sha256::digest(b"qemu log")));
        assert_eq!(qemu["truncated"], false);
        let build = logs
            .iter()
            .find(|log| log["path"] == "output/build.log")
            .unwrap();
        assert_eq!(build["truncated"], true);
        assert_eq!(
            STANDARD
                .decode(build["content_base64"].as_str().unwrap())
                .unwrap()
                .len(),
            128 * 1024
        );
    }

    #[tokio::test]
    async fn evidence_archives_are_complete_regular_files_with_stable_manifests() {
        let root = tempfile::tempdir().unwrap();
        let attempt = uuid::Uuid::new_v4().to_string();
        let source = root.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("large.log"), vec![b'x'; 1024 * 1024]).unwrap();
        let output = root
            .path()
            .join("jobs/completed")
            .join(&attempt)
            .join("output/evidence")
            .join(&attempt);
        fs::create_dir_all(&output).unwrap();
        for name in ["profile.tar.zst", "source.tar.zst", "build-records.tar.zst"] {
            create_archive(&output.join(name), &source, &["."])
                .await
                .unwrap();
        }
        let runtime =
            BuilderRuntime::new(root.path().join("profiles"), root.path().join("jobs"), None);
        let entries = runtime.completed_evidence_files(&attempt).unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries.iter().all(|entry| {
            entry.path.starts_with(&format!("evidence/{attempt}/"))
                && entry.size > 0
                && entry.sha256.len() == 64
        }));
        let listing = std::process::Command::new("/usr/bin/bsdtar")
            .args(["-tf"])
            .arg(output.join("source.tar.zst"))
            .output()
            .unwrap();
        assert!(listing.status.success());
        assert!(String::from_utf8_lossy(&listing.stdout).contains("large.log"));
    }
}
