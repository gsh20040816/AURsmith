//! Builder 守护进程：领取 → 准备输入 → docker run → 回报 → 分片上传 → 完成。
//!
//! 每个构建一个工作目录 `<jobs_dir>/<build_id>/`：
//!
//! ```text
//! state.json         {build_id, attempt}
//! input/             AUR 快照文件 + .aursmith/guest.json + .aursmith/deps/*.pkg.tar.*
//! output/            构建容器写入：build.log、guest-result.json / guest-error.json、软件包
//! docker.log         docker run 的 stdout/stderr
//! report.json        已生成的 BuildReport（存在即表示构建阶段已结束）
//! reported           回报已被服务端接受（重启后跳过回报，直接续传）
//! ```
//!
//! Builder 重启后会扫描遗留目录：已有 report.json 的继续回报/续传；构建中途被打断的
//! 以 BUILDER_RESTARTED（瞬态，服务端自动重试）回报失败。

use crate::{
    client::{Client, is_gone, server_code},
    config::BuilderConfig,
};
use anyhow::{Context, bail};
use aursmith_core::{
    names::{validate_artifact_file_name, validate_relative_path},
    protocol::{
        ArtifactRecord, BuildOutcome, BuildReport, BuildSpec, GuestError, GuestResult, GuestSpec,
        MAXIMUM_ARTIFACT_BYTES, MAXIMUM_LOG_BYTES, UPLOAD_CHUNK_BYTES,
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use tokio::{process::Command, sync::Semaphore, time::timeout};

const MAXIMUM_INPUT_FILES: usize = 512;
const MAXIMUM_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const MAXIMUM_GUEST_RESULT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct JobState {
    build_id: String,
    attempt: i64,
}

pub async fn run(config: BuilderConfig) -> anyhow::Result<()> {
    fs::create_dir_all(&config.jobs_dir)
        .with_context(|| format!("无法创建 {}", config.jobs_dir.display()))?;
    let client = Client::new(config.server_url.clone(), config.token.clone())?;
    tracing::info!(server = %config.server_url, builder = %config.builder_id, "Builder 启动");
    recover(&config, &client).await;
    let config = Arc::new(config);
    let slots = Arc::new(Semaphore::new(config.concurrency));
    loop {
        let permit = slots.clone().acquire_owned().await?;
        match client
            .lease(&config.builder_id, config.lease_seconds())
            .await
        {
            Ok(Some(spec)) => {
                let config = config.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    let build_id = spec.build_id.clone();
                    if let Err(error) = execute(&config, &client, spec).await {
                        tracing::warn!(build = %build_id, error = %format!("{error:#}"), "构建处理失败");
                    }
                    drop(permit);
                });
            }
            Ok(None) => {
                drop(permit);
                tokio::time::sleep(Duration::from_secs(config.idle_seconds)).await;
            }
            Err(error) => {
                drop(permit);
                tracing::warn!(error = %format!("{error:#}"), "领取构建失败");
                tokio::time::sleep(Duration::from_secs(config.idle_seconds * 4)).await;
            }
        }
    }
}

fn validate_build_id(value: &str) -> anyhow::Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        bail!("服务端返回的 build_id 无效");
    }
    Ok(())
}

fn job_dir(config: &BuilderConfig, build_id: &str) -> PathBuf {
    config.jobs_dir.join(build_id)
}

/// 处理一个刚领取的构建。
pub async fn execute(
    config: &BuilderConfig,
    client: &Client,
    spec: BuildSpec,
) -> anyhow::Result<()> {
    validate_build_id(&spec.build_id)?;
    let dir = job_dir(config, &spec.build_id);
    if dir.exists() {
        fs::remove_dir_all(&dir)?;
    }
    fs::create_dir_all(&dir)?;
    write_json(
        &dir.join("state.json"),
        &JobState {
            build_id: spec.build_id.clone(),
            attempt: spec.attempt,
        },
    )?;
    tracing::info!(build = %spec.build_id, package = %spec.package_base, attempt = spec.attempt, "开始构建");
    let report = build(config, client, &spec, &dir).await;
    write_json(&dir.join("report.json"), &report)?;
    deliver_with_retries(client, &spec.build_id, &dir, &report).await
}

/// 执行构建并生成回报；所有失败都折算成带失败码的回报。
async fn build(
    config: &BuilderConfig,
    client: &Client,
    spec: &BuildSpec,
    dir: &Path,
) -> BuildReport {
    let failed = |code: &str, detail: String, dir: &Path| {
        let (log, log_truncated) = collect_log(dir, &detail);
        BuildReport {
            attempt: spec.attempt,
            outcome: BuildOutcome::Failed,
            artifacts: Vec::new(),
            error_code: Some(code.to_owned()),
            log,
            log_truncated,
        }
    };
    let input = dir.join("input");
    if let Err(error) = materialize_inputs(spec, &input) {
        return failed("INPUT_INVALID", format!("{error:#}"), dir);
    }
    let deps = input.join(".aursmith/deps");
    let mut dependency_files = Vec::new();
    for dependency in &spec.dependencies {
        let result = async {
            validate_build_id(&dependency.build_id)?;
            validate_artifact_file_name(&dependency.file)?;
            client
                .download(
                    &dependency.build_id,
                    &dependency.file,
                    dependency.size,
                    &dependency.sha256,
                    &deps.join(&dependency.file),
                )
                .await
        }
        .await;
        if let Err(error) = result {
            return failed("DEPENDENCY_DOWNLOAD_FAILED", format!("{error:#}"), dir);
        }
        dependency_files.push(dependency.file.clone());
    }
    let guest = GuestSpec {
        build_id: spec.build_id.clone(),
        package_base: spec.package_base.clone(),
        expected_outputs: spec.expected_outputs.clone(),
        allow_check: spec.allow_check,
        dependency_files,
    };
    if let Err(error) = write_json(&input.join(".aursmith/guest.json"), &guest) {
        return failed("BUILDER_CONFIG", format!("{error:#}"), dir);
    }
    let output = dir.join("output");
    if let Err(error) = fs::create_dir_all(&output) {
        return failed("BUILDER_CONFIG", error.to_string(), dir);
    }
    if let Err(code) = run_container(config, &spec.build_id, dir).await {
        return failed(&code, String::new(), dir);
    }
    match read_guest_result(&output, spec) {
        Ok(artifacts) => {
            let (log, log_truncated) = collect_log(dir, "");
            BuildReport {
                attempt: spec.attempt,
                outcome: BuildOutcome::Succeeded,
                artifacts,
                error_code: None,
                log,
                log_truncated,
            }
        }
        Err(error) => failed("GUEST_OUTPUT_MISMATCH", format!("{error:#}"), dir),
    }
}

/// 把内联 AUR 文件写入输入目录，逐个校验大小与 SHA-256。
pub fn materialize_inputs(spec: &BuildSpec, input: &Path) -> anyhow::Result<()> {
    if spec.files.is_empty() || spec.files.len() > MAXIMUM_INPUT_FILES {
        bail!("输入文件数量无效");
    }
    let total = spec.files.iter().map(|file| file.size).sum::<u64>();
    if total > MAXIMUM_INPUT_BYTES {
        bail!("输入文件总大小超过上限");
    }
    fs::create_dir_all(input.join(".aursmith/deps"))?;
    for file in &spec.files {
        validate_relative_path(&file.path)?;
        if file.path == ".aursmith" || file.path.starts_with(".aursmith/") {
            bail!("输入使用了保留路径 {}", file.path);
        }
        let bytes = STANDARD.decode(&file.content_base64)?;
        if bytes.len() as u64 != file.size || aursmith_core::sha256_hex(&bytes) != file.sha256 {
            bail!("输入文件 {} 的大小或 SHA-256 与服务端声明不一致", file.path);
        }
        let path = input.join(&file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        use std::io::Write;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("输入文件重复或无法创建：{}", file.path))?
            .write_all(&bytes)?;
    }
    if !input.join("PKGBUILD").is_file() || !input.join(".SRCINFO").is_file() {
        bail!("输入缺少 PKGBUILD 或 .SRCINFO");
    }
    Ok(())
}

pub fn docker_arguments(config: &BuilderConfig, build_id: &str, dir: &Path) -> Vec<String> {
    vec![
        "run".into(),
        "--rm".into(),
        "--init".into(),
        "--name".into(),
        container_name(build_id),
        "--network".into(),
        "bridge".into(),
        "--cpus".into(),
        config.cpus.to_string(),
        "--memory".into(),
        format!("{}m", config.memory_mib),
        "--volume".into(),
        format!("{}:/mnt/aursmith-input:ro", dir.join("input").display()),
        "--volume".into(),
        format!("{}:/mnt/aursmith-output:rw", dir.join("output").display()),
        config.image.clone(),
    ]
}

fn container_name(build_id: &str) -> String {
    format!("aursmith-build-{build_id}")
}

/// 运行一次性构建容器；返回失败码。
async fn run_container(config: &BuilderConfig, build_id: &str, dir: &Path) -> Result<(), String> {
    let log = fs::File::create(dir.join("docker.log")).map_err(|_| "BUILDER_CONFIG".to_owned())?;
    let log_err = log.try_clone().map_err(|_| "BUILDER_CONFIG".to_owned())?;
    let mut child = Command::new(&config.docker)
        .args(docker_arguments(config, build_id, dir))
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "DOCKER_CLI_UNAVAILABLE".to_owned())?;
    let status = match timeout(Duration::from_secs(config.timeout_seconds), child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => return Err("DOCKER_RUNTIME_FAILED".into()),
        Err(_) => {
            remove_container(config, build_id).await;
            let _ = child.wait().await;
            return Err("BUILD_TIMEOUT".into());
        }
    };
    if status.success() {
        return Ok(());
    }
    if let Some(code) = guest_error_code(&dir.join("output")) {
        return Err(code);
    }
    Err(classify_docker_diagnostic(&tail_text(&dir.join("docker.log"), 16 * 1024)).into())
}

async fn remove_container(config: &BuilderConfig, build_id: &str) {
    let _ = timeout(
        Duration::from_secs(30),
        Command::new(&config.docker)
            .args(["rm", "--force", &container_name(build_id)])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await;
}

fn guest_error_code(output: &Path) -> Option<String> {
    let path = output.join("guest-error.json");
    if fs::metadata(&path).ok()?.len() > 64 * 1024 {
        return None;
    }
    let error: GuestError = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let valid = !error.code.is_empty()
        && error.code.len() <= 64
        && error
            .code
            .chars()
            .all(|character| character.is_ascii_uppercase() || character == '_');
    valid.then_some(error.code)
}

pub fn classify_docker_diagnostic(message: &str) -> &'static str {
    let message = message.to_ascii_lowercase();
    if message.contains("unable to find image") {
        "BUILD_IMAGE_MISSING"
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
    } else if message.contains("oomkilled") || message.contains("exit code 137") {
        "BUILD_OUT_OF_MEMORY"
    } else {
        "DOCKER_RUNTIME_FAILED"
    }
}

/// 读取 guest 结果；由 Builder 重新计算每个产物的大小与摘要，不信任 guest 自报。
pub fn read_guest_result(output: &Path, spec: &BuildSpec) -> anyhow::Result<Vec<ArtifactRecord>> {
    let path = output.join("guest-result.json");
    let metadata = fs::symlink_metadata(&path).context("构建容器没有写出 guest-result.json")?;
    if !metadata.is_file() || metadata.len() > MAXIMUM_GUEST_RESULT_BYTES {
        bail!("guest-result.json 无效");
    }
    let result: GuestResult = serde_json::from_slice(&fs::read(&path)?)?;
    if result.build_id != spec.build_id {
        bail!("guest-result.json 的 build_id 不匹配");
    }
    let mut artifacts = Vec::new();
    for artifact in result.artifacts {
        validate_artifact_file_name(&artifact.file)?;
        let file = output.join(&artifact.file);
        let metadata = fs::symlink_metadata(&file)
            .with_context(|| format!("产物 {} 不存在", artifact.file))?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAXIMUM_ARTIFACT_BYTES {
            bail!("产物 {} 不是有效的普通文件", artifact.file);
        }
        let sha256 = digest_file(&file)?;
        if sha256 != artifact.sha256 || metadata.len() != artifact.size {
            bail!("产物 {} 的摘要与 guest 回报不一致", artifact.file);
        }
        artifacts.push(ArtifactRecord {
            sha256,
            size: metadata.len(),
            ..artifact
        });
    }
    artifacts.sort();
    Ok(artifacts)
}

/// 日志 = build.log + docker.log + 补充说明，整体只保留末尾 MAXIMUM_LOG_BYTES。
fn collect_log(dir: &Path, detail: &str) -> (String, bool) {
    let mut text = String::new();
    for (title, path) in [
        ("build.log", dir.join("output/build.log")),
        ("docker", dir.join("docker.log")),
    ] {
        if path.is_file() {
            text.push_str(&format!("==> {title}\n"));
            text.push_str(&tail_text(&path, MAXIMUM_LOG_BYTES));
            text.push('\n');
        }
    }
    if !detail.is_empty() {
        text.push_str("==> aursmith-builder\n");
        text.push_str(detail);
        text.push('\n');
    }
    truncate_tail(text, MAXIMUM_LOG_BYTES)
}

fn truncate_tail(text: String, maximum: usize) -> (String, bool) {
    if text.len() <= maximum {
        return (text, false);
    }
    let mut cut = text.len() - maximum;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    (text[cut..].to_owned(), true)
}

fn tail_text(path: &Path, maximum: usize) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let length = file.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    let start = length.saturating_sub(maximum as u64);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    let _ = file.take(maximum as u64).read_to_end(&mut bytes);
    String::from_utf8_lossy(&bytes).into_owned()
}

fn digest_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

fn write_json(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write(&partial, serde_json::to_vec_pretty(value)?)?;
    fs::rename(partial, path)?;
    Ok(())
}

enum Delivery {
    Done,
    Discarded,
}

/// 回报 + 上传 + 完成。网络错误按退避重试；服务端明确拒绝（409/400/404）时丢弃本地目录，
/// 由服务端的租约过期 / 重试逻辑接管。
async fn deliver_with_retries(
    client: &Client,
    build_id: &str,
    dir: &Path,
    report: &BuildReport,
) -> anyhow::Result<()> {
    let delays = [5_u64, 30, 60, 120, 300, 600];
    let mut last_error = None;
    for (index, delay) in delays.iter().enumerate() {
        match deliver(client, build_id, dir, report).await {
            Ok(Delivery::Done) => {
                tracing::info!(build = %build_id, outcome = ?report.outcome, "构建结果已交付");
                fs::remove_dir_all(dir).ok();
                return Ok(());
            }
            Ok(Delivery::Discarded) => {
                tracing::warn!(build = %build_id, "服务端已不再接受该构建，丢弃本地工作目录");
                fs::remove_dir_all(dir).ok();
                return Ok(());
            }
            Err(error) => {
                tracing::warn!(build = %build_id, attempt = index + 1, error = %format!("{error:#}"), "交付失败，稍后重试");
                last_error = Some(error);
                tokio::time::sleep(Duration::from_secs(*delay)).await;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("交付失败")))
        .context("多次交付失败；工作目录保留，Builder 重启后会继续")
}

async fn deliver(
    client: &Client,
    build_id: &str,
    dir: &Path,
    report: &BuildReport,
) -> anyhow::Result<Delivery> {
    let marker = dir.join("reported");
    let mut uploading = marker.exists();
    if !uploading {
        match client.report(build_id, report).await {
            Ok(response) => {
                fs::write(&marker, b"")?;
                uploading = response["state"] == "uploading";
            }
            Err(error) if is_gone(&error) => return Ok(Delivery::Discarded),
            Err(error) => return Err(error),
        }
    }
    if report.outcome == BuildOutcome::Failed || !uploading {
        return Ok(Delivery::Done);
    }
    for artifact in &report.artifacts {
        match upload_file(
            client,
            build_id,
            &dir.join("output").join(&artifact.file),
            artifact,
            UPLOAD_CHUNK_BYTES,
        )
        .await
        {
            Ok(()) => {}
            Err(error) if is_gone(&error) => return Ok(Delivery::Discarded),
            Err(error) => return Err(error),
        }
    }
    match client.complete(build_id).await {
        Ok(_) => Ok(Delivery::Done),
        Err(error) if is_gone(&error) => Ok(Delivery::Discarded),
        Err(error) => Err(error),
    }
}

/// 断点续传一个产物：先询问服务端已收到多少字节，再从该偏移按分片 PUT。
pub async fn upload_file(
    client: &Client,
    build_id: &str,
    path: &Path,
    artifact: &ArtifactRecord,
    chunk_bytes: u64,
) -> anyhow::Result<()> {
    let mut restarted = false;
    let mut status = client.upload_status(build_id, &artifact.file).await?;
    while !status.complete {
        let offset = status.received;
        let length = chunk_bytes.min(artifact.size.saturating_sub(offset));
        if length == 0 {
            bail!("服务端报告的偏移超过文件大小");
        }
        let chunk = {
            let path = path.to_owned();
            tokio::task::spawn_blocking(move || -> std::io::Result<Vec<u8>> {
                let mut file = fs::File::open(path)?;
                file.seek(SeekFrom::Start(offset))?;
                let mut buffer = vec![0_u8; length as usize];
                file.read_exact(&mut buffer)?;
                Ok(buffer)
            })
            .await??
        };
        match client
            .upload_chunk(build_id, &artifact.file, offset, chunk)
            .await
        {
            Ok(next) => status = next,
            Err(error) if server_code(&error) == Some("OFFSET_MISMATCH") => {
                status = client.upload_status(build_id, &artifact.file).await?;
            }
            Err(error) if server_code(&error) == Some("DIGEST_MISMATCH") && !restarted => {
                restarted = true;
                status = client.upload_status(build_id, &artifact.file).await?;
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// 处理上次运行遗留的工作目录。
async fn recover(config: &BuilderConfig, client: &Client) {
    let Ok(entries) = fs::read_dir(&config.jobs_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let state: Option<JobState> = fs::read(dir.join("state.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let Some(state) = state.filter(|state| validate_build_id(&state.build_id).is_ok()) else {
            fs::remove_dir_all(&dir).ok();
            continue;
        };
        let report: Option<BuildReport> = fs::read(dir.join("report.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok());
        let report = match report {
            Some(report) => report,
            None => {
                remove_container(config, &state.build_id).await;
                let (log, log_truncated) = collect_log(&dir, "Builder 在构建过程中重启");
                BuildReport {
                    attempt: state.attempt,
                    outcome: BuildOutcome::Failed,
                    artifacts: Vec::new(),
                    error_code: Some("BUILDER_RESTARTED".into()),
                    log,
                    log_truncated,
                }
            }
        };
        tracing::info!(build = %state.build_id, "恢复遗留构建");
        if let Err(error) = deliver_with_retries(client, &state.build_id, &dir, &report).await {
            tracing::warn!(build = %state.build_id, error = %format!("{error:#}"), "恢复遗留构建失败");
        }
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
