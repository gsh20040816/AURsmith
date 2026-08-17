use anyhow::{Context, bail};
use aursmith_protocol::{GuestResult, JobKind, JobSpec, ManifestEntry};
use base64::{Engine, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{process::Command, time::timeout};

const BUILD_IMAGE: &str = "aursmith-build:latest";

#[derive(Clone)]
pub struct BuilderRuntime {
    jobs_dir: PathBuf,
}

impl BuilderRuntime {
    pub fn new(jobs_dir: PathBuf) -> Self {
        Self { jobs_dir }
    }

    pub fn jobs_dir(&self) -> &Path {
        &self.jobs_dir
    }

    pub fn completed_result_json(&self, attempt_id: &str) -> anyhow::Result<String> {
        validate_attempt_id(attempt_id)?;
        fs::read_to_string(
            self.jobs_dir
                .join("completed")
                .join(attempt_id)
                .join("output/build-result.json"),
        )
        .context("COMPLETED_RESULT_MISSING")
    }

    pub fn completed_evidence_files(&self, attempt_id: &str) -> anyhow::Result<Vec<ManifestEntry>> {
        validate_attempt_id(attempt_id)?;
        Ok(Vec::new())
    }

    pub fn attempt_logs(
        &self,
        attempt_id: &str,
        succeeded: bool,
    ) -> anyhow::Result<Vec<serde_json::Value>> {
        const MAX_CONTENT_PER_FILE: u64 = 128 * 1024;
        const MAX_HASH_FILE_SIZE: u64 = 64 * 1024 * 1024;
        validate_attempt_id(attempt_id)?;
        let root = self
            .jobs_dir
            .join(if succeeded { "completed" } else { "failed" })
            .join(attempt_id);
        let candidates: &[(&str, &str)] = if succeeded {
            &[
                ("docker.stdout.log", "docker.stdout.log"),
                ("docker.stderr.log", "docker.stderr.log"),
                ("output/build.log", "output/build.log"),
            ]
        } else {
            &[
                ("docker.stdout.log", "docker.stdout.log"),
                ("docker.stderr.log", "docker.stderr.log"),
                ("guest-error.json", "output/guest-error.json"),
                ("build.log", "output/build.log"),
            ]
        };
        let mut logs = Vec::new();
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
        let staging_parent = self.jobs_dir.join("staging");
        let staging = staging_parent.join(spec.attempt.attempt_id.to_string());
        fs::create_dir_all(&staging_parent)?;
        fs::create_dir(&staging).context("STAGING_ALREADY_EXISTS")?;
        let input_root = staging.join("input");
        fs::create_dir(&input_root)?;
        let materialized = (|| -> anyhow::Result<()> {
            for input in &spec.inline_inputs {
                aursmith_protocol::validate_relative_path(&input.entry.path)?;
                if input.entry.path == ".aursmith" || input.entry.path.starts_with(".aursmith/") {
                    bail!("INPUT_RESERVED_PATH:{}", input.entry.path);
                }
                let bytes = STANDARD.decode(&input.content_base64)?;
                if bytes.len() as u64 != input.entry.size
                    || hex::encode(Sha256::digest(&bytes)) != input.entry.sha256
                {
                    bail!("INLINE_INPUT_DIGEST_MISMATCH:{}", input.entry.path);
                }
                let path = input_root.join(&input.entry.path);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }
                use std::io::Write;
                fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(path)?
                    .write_all(&bytes)?;
            }
            verify_inputs(&input_root, &spec.inputs)?;
            if !spec.dependency_attempt_ids.is_empty() {
                let dependency_root = input_root.join(".aursmith-batch-dependencies");
                fs::create_dir(&dependency_root)?;
                for dependency_attempt in &spec.dependency_attempt_ids {
                    let dependency_output = self
                        .jobs_dir
                        .join("completed")
                        .join(dependency_attempt.to_string())
                        .join("output");
                    let raw = fs::read(dependency_output.join("build-result.json"))?;
                    let GuestResult::Build(result) = serde_json::from_slice(&raw)?;
                    let entries = result
                        .artifacts
                        .iter()
                        .map(|artifact| ManifestEntry {
                            path: artifact.path.clone(),
                            sha256: artifact.sha256.clone(),
                            size: artifact.size,
                        })
                        .collect::<Vec<_>>();
                    validate_output_entries(&dependency_output, &entries)?;
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
            }
            Ok(())
        })();
        if materialized.is_err() {
            let _ = fs::remove_dir_all(&staging);
        }
        materialized
    }
}

pub fn spawn(database: SqlitePool, runtime: BuilderRuntime) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(std::time::Duration::from_secs(1));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = execute_one(&database, &runtime).await {
                tracing::warn!(%error, "Builder 执行任务失败");
            }
        }
    });
}

async fn execute_one(database: &SqlitePool, runtime: &BuilderRuntime) -> anyhow::Result<()> {
    let row = sqlx::query("SELECT attempt_id, spec_json FROM attempts WHERE status = 'queued' ORDER BY received_at LIMIT 1")
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
                .bind(&code).bind(&attempt_id).execute(database).await?;
            tracing::warn!(attempt_id, failure_code = code, %error, "Builder Attempt 失败");
        }
    }
    Ok(())
}

fn persist_failure_diagnostics(runtime: &BuilderRuntime, attempt_id: &str) {
    let work = runtime.jobs_dir.join("runtime").join(attempt_id);
    let failed = runtime.jobs_dir.join("failed").join(attempt_id);
    for (source, name) in [
        (work.join("docker.stdout.log"), "docker.stdout.log"),
        (work.join("docker.stderr.log"), "docker.stderr.log"),
        (work.join("output/guest-error.json"), "guest-error.json"),
        (work.join("output/build.log"), "build.log"),
    ] {
        if source.is_file() {
            let _ = fs::create_dir_all(&failed);
            let _ = fs::copy(source, failed.join(name));
        }
    }
}

async fn execute_attempt(
    runtime: &BuilderRuntime,
    spec_json: &str,
    attempt_id: &str,
) -> anyhow::Result<String> {
    let spec: JobSpec = serde_json::from_str(spec_json)?;
    if spec.attempt.attempt_id.to_string() != attempt_id {
        bail!("ATTEMPT_MISMATCH");
    }
    if spec.kind != JobKind::Build {
        bail!("UNSUPPORTED_JOB_KIND");
    }
    let staging = runtime.jobs_dir.join("staging").join(attempt_id);
    verify_inputs(&staging.join("input"), &spec.inputs)?;
    let control_input = staging.join("input/.aursmith");
    fs::create_dir_all(&control_input)?;
    fs::write(control_input.join("job-spec.json"), spec_json)?;

    let work = runtime.jobs_dir.join("runtime").join(attempt_id);
    if work.exists() {
        fs::remove_dir_all(&work)?;
    }
    fs::create_dir_all(work.join("output"))?;
    let container_name = format!("aursmith-build-{attempt_id}");
    let stdout = File::create(work.join("docker.stdout.log"))?;
    let stderr = File::create(work.join("docker.stderr.log"))?;
    let cpus = spec.limits.cpu_count.to_string();
    let memory = format!("{}m", spec.limits.memory_mib);
    let input_mount = format!("{}:/mnt/aursmith-input:ro", staging.join("input").display());
    let output_mount = format!("{}:/mnt/aursmith-output:rw", work.join("output").display());
    let mut command = Command::new("/usr/bin/docker");
    command
        .args([
            "run",
            "--rm",
            "--init",
            "--name",
            &container_name,
            "--network",
            "bridge",
            "--cpus",
            &cpus,
            "--memory",
            &memory,
            "--volume",
            &input_mount,
            "--volume",
            &output_mount,
            BUILD_IMAGE,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true);
    let mut child = command.spawn().context("DOCKER_CLI_UNAVAILABLE")?;
    let execution = timeout(
        std::time::Duration::from_secs(spec.limits.timeout_seconds),
        child.wait(),
    )
    .await;
    let status = match execution {
        Ok(status) => status?,
        Err(_) => {
            remove_container(&container_name).await;
            let _ = child.wait().await;
            bail!("DOCKER_TIMEOUT");
        }
    };
    if !status.success() {
        if let Some(code) = guest_failure_code(&work.join("output")) {
            bail!("{code}");
        }
        let diagnostic = bounded_text(&work.join("docker.stderr.log"), 16 * 1024);
        bail!("{}:{diagnostic}", classify_docker_diagnostic(&diagnostic));
    }
    let result_path = work.join("output/build-result.json");
    let result = fs::read(&result_path).context("GUEST_RESULT_MISSING")?;
    let guest_result: GuestResult =
        serde_json::from_slice(&result).context("GUEST_RESULT_INVALID")?;
    validate_guest_result(&guest_result, &spec, &work.join("output"))?;
    let digest = hex::encode(Sha256::digest(&result));
    let completed = runtime.jobs_dir.join("completed").join(attempt_id);
    fs::create_dir_all(runtime.jobs_dir.join("completed"))?;
    fs::rename(&work, &completed)?;
    Ok(digest)
}

async fn remove_container(name: &str) {
    let _ = timeout(
        std::time::Duration::from_secs(30),
        Command::new("/usr/bin/docker")
            .args(["rm", "--force", name])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

fn validate_guest_result(
    result: &GuestResult,
    spec: &JobSpec,
    output: &Path,
) -> anyhow::Result<()> {
    let GuestResult::Build(value) = result;
    if value.job_id != spec.job_id
        || value.attempt != spec.attempt
        || value.revision_sha256 != spec.revision_sha256
    {
        bail!("GUEST_RESULT_IDENTITY_MISMATCH");
    }
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

fn validate_output_entries(output: &Path, entries: &[ManifestEntry]) -> anyhow::Result<()> {
    for entry in entries {
        aursmith_protocol::validate_relative_path(&entry.path)?;
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
            bail!("INPUT_METADATA_MISMATCH:{}", entry.path);
        }
        if digest_file(&path)? != entry.sha256 {
            bail!("INPUT_DIGEST_MISMATCH:{}", entry.path);
        }
    }
    Ok(())
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

fn guest_failure_code(output: &Path) -> Option<String> {
    let bytes = fs::read(output.join("guest-error.json")).ok()?;
    (bytes.len() <= 64 * 1024)
        .then_some(bytes)
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value["code"].as_str().map(str::to_owned))
}

fn bounded_text(path: &Path, maximum: usize) -> String {
    let bytes = fs::read(path).unwrap_or_default();
    String::from_utf8_lossy(&bytes[..bytes.len().min(maximum)]).into_owned()
}

fn classify_docker_diagnostic(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if message.contains("unauthorized") || message.contains("authentication required") {
        "DOCKER_UNAUTHORIZED"
    } else if message.contains("manifest unknown") || message.contains("no matching manifest") {
        "DOCKER_MANIFEST_INVALID"
    } else if message.contains("permission denied") {
        "DOCKER_PERMISSION_DENIED"
    } else if message.contains("no space left") {
        "DOCKER_DISK_FULL"
    } else if message.contains("cannot connect to the docker daemon")
        || message.contains("is the docker daemon running")
    {
        "DOCKER_DAEMON_UNAVAILABLE"
    } else if message.contains("i/o timeout")
        || message.contains("connection timed out")
        || message.contains("temporary failure in name resolution")
    {
        "DOCKER_NETWORK_TIMEOUT"
    } else if message.contains("pull access denied") || message.contains("failed to pull") {
        "DOCKER_PULL_FAILED"
    } else {
        "DOCKER_RUNTIME_FAILED"
    }
}

fn classify_failure(error: &anyhow::Error) -> String {
    let message = error.to_string();
    for code in [
        "DOCKER_TIMEOUT",
        "DOCKER_DAEMON_UNAVAILABLE",
        "DOCKER_PULL_FAILED",
        "DOCKER_NETWORK_TIMEOUT",
        "DOCKER_UNAUTHORIZED",
        "DOCKER_MANIFEST_INVALID",
        "DOCKER_PERMISSION_DENIED",
        "DOCKER_DISK_FULL",
        "DOCKER_CLI_UNAVAILABLE",
        "GUEST_CHECKSUM_FAILED",
        "GUEST_PGP_FAILED",
        "GUEST_CHECK_FAILED",
        "GUEST_PACKAGE_FAILED",
        "GUEST_OUTPUT_MISMATCH",
        "GUEST_BUILD_FAILED",
        "BUILD_NETWORK_TRANSIENT",
    ] {
        if message.contains(code) {
            return code.to_owned();
        }
    }
    if message.contains("INPUT_") || message.contains("INLINE_INPUT_") {
        "INPUT_INVALID".to_owned()
    } else if message.to_ascii_lowercase().contains("permission denied") {
        "DOCKER_PERMISSION_DENIED".to_owned()
    } else if message.to_ascii_lowercase().contains("no space left") {
        "DOCKER_DISK_FULL".to_owned()
    } else {
        "DOCKER_RUNTIME_FAILED".to_owned()
    }
}

fn validate_attempt_id(attempt_id: &str) -> anyhow::Result<()> {
    uuid::Uuid::parse_str(attempt_id)
        .map(|_| ())
        .map_err(|_| anyhow::anyhow!("INVALID_ATTEMPT_ID"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_failures_have_explicit_retry_boundaries() {
        assert_eq!(
            classify_docker_diagnostic("Cannot connect to the Docker daemon"),
            "DOCKER_DAEMON_UNAVAILABLE"
        );
        assert_eq!(
            classify_docker_diagnostic("unauthorized: authentication required"),
            "DOCKER_UNAUTHORIZED"
        );
        assert_eq!(
            classify_docker_diagnostic("no space left on device"),
            "DOCKER_DISK_FULL"
        );
    }

    #[test]
    fn build_container_uses_the_fixed_image() {
        assert_eq!(BUILD_IMAGE, "aursmith-build:latest");
    }
}
