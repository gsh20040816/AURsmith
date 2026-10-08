//! 构建调度与家用 Builder 的 HTTPS 协议（租约、回报、分片上传、依赖下载）。
//!
//! 构建状态：queued → running → uploading → succeeded；任一阶段失败进入 failed，
//! 瞬态失败（含租约过期）在同一行上 attempt+1 回到 queued，至多自动重试两次。

use crate::{
    app::AppState,
    aur::Snapshot,
    auth::require_builder,
    error::ApiError,
    packages::{Closure, compute_closure},
};
use aursmith_core::{
    names::validate_artifact_file_name,
    protocol::{
        ArtifactRecord, BuildOutcome, BuildReport, BuildSpec, DependencyArtifact, ErrorClass,
        InputFile, LeaseRequest, LeaseResponse, MAXIMUM_ARTIFACT_BYTES, MAXIMUM_LOG_BYTES,
        MAXIMUM_TRANSIENT_RETRIES, UPLOAD_CHUNK_BYTES, UploadStatus, classify_error_code,
    },
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub fn artifact_dir(state: &AppState, build_id: &str) -> PathBuf {
    state.config.data_dir.join("artifacts").join(build_id)
}

fn parse_artifacts(json: &str) -> Vec<ArtifactRecord> {
    serde_json::from_str(json).unwrap_or_default()
}

/// pkgbase 当前可用的构建：优先最新已批准 revision 的成功构建，其次任意 revision 的最新成功构建
/// （旧版本继续可用）。产物文件必须仍在磁盘上。
pub async fn usable_build(
    state: &AppState,
    package_base: &str,
) -> sqlx::Result<Option<(String, Vec<ArtifactRecord>)>> {
    let rows = sqlx::query(
        "SELECT builds.id, builds.artifacts_json, builds.revision_id = (SELECT id FROM revisions WHERE package_base = ? AND state = 'approved' ORDER BY created_at DESC LIMIT 1) AS current \
         FROM builds WHERE builds.package_base = ? AND builds.state = 'succeeded' ORDER BY current DESC, builds.finished_at DESC LIMIT 5",
    )
    .bind(package_base)
    .bind(package_base)
    .fetch_all(&state.db)
    .await?;
    for row in rows {
        let id: String = row.get("id");
        let artifacts = parse_artifacts(row.get("artifacts_json"));
        let directory = artifact_dir(state, &id);
        if !artifacts.is_empty()
            && artifacts
                .iter()
                .all(|artifact| directory.join(&artifact.file).is_file())
        {
            return Ok(Some((id, artifacts)));
        }
    }
    Ok(None)
}

/// pkgbase 的构建是否在进行中（running/uploading，或 queued 且依赖已就绪）。
pub async fn has_active_build(
    state: &AppState,
    closure: &Closure,
    package_base: &str,
) -> anyhow::Result<bool> {
    let rows = sqlx::query("SELECT state FROM builds WHERE package_base = ? AND state IN ('queued', 'running', 'uploading')")
        .bind(package_base)
        .fetch_all(&state.db)
        .await?;
    for row in rows {
        let build_state: String = row.get("state");
        if build_state != "queued"
            || dependency_builds(state, closure, package_base)
                .await?
                .is_ok()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 构建某个 pkgbase 需要预装的 AUR 依赖构建（传递闭包）。依赖未就绪时返回等待原因。
pub async fn dependency_builds(
    state: &AppState,
    closure: &Closure,
    package_base: &str,
) -> anyhow::Result<Result<Vec<(String, Vec<ArtifactRecord>)>, String>> {
    if let Some(unresolved) = closure.unresolved.get(package_base) {
        return Ok(Err(format!("等待选择 provider：{}", unresolved.join("、"))));
    }
    let mut result = Vec::new();
    for dependency in closure.graph.closure([package_base]) {
        if dependency == package_base {
            continue;
        }
        match usable_build(state, &dependency).await? {
            Some(build) => result.push(build),
            None => return Ok(Err(format!("等待依赖 {dependency} 构建成功"))),
        }
    }
    Ok(Ok(result))
}

/// 为闭包内每个 pkgbase 的最新已批准 revision 排队构建；取消过时或已不需要的排队构建。
pub async fn schedule(state: &AppState, closure: &Closure) -> anyhow::Result<usize> {
    let now = Utc::now();
    let stale = sqlx::query(
        "SELECT builds.id, builds.package_base, builds.revision_id FROM builds WHERE builds.state = 'queued'",
    )
    .fetch_all(&state.db)
    .await?;
    for row in stale {
        let base: String = row.get("package_base");
        let revision: String = row.get("revision_id");
        let latest: Option<String> = sqlx::query_scalar("SELECT id FROM revisions WHERE package_base = ? AND state = 'approved' ORDER BY created_at DESC LIMIT 1")
            .bind(&base)
            .fetch_optional(&state.db)
            .await?;
        let reason = if !closure.members.contains(&base) {
            Some("NOT_REQUIRED")
        } else if latest.as_deref() != Some(revision.as_str()) {
            Some("SUPERSEDED")
        } else {
            None
        };
        if let Some(code) = reason {
            sqlx::query("UPDATE builds SET state = 'failed', error_code = ?, error_class = 'deterministic', finished_at = ? WHERE id = ? AND state = 'queued'")
                .bind(code)
                .bind(now)
                .bind(row.get::<String, _>("id"))
                .execute(&state.db)
                .await?;
        }
    }
    let mut created = 0;
    for base in &closure.members {
        let Some(revision) = sqlx::query("SELECT revisions.id, packages.allow_check FROM revisions JOIN packages ON packages.package_base = revisions.package_base WHERE revisions.package_base = ? AND revisions.state = 'approved' ORDER BY revisions.created_at DESC LIMIT 1")
            .bind(base)
            .fetch_optional(&state.db)
            .await?
        else {
            continue;
        };
        let revision_id: String = revision.get("id");
        let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM builds WHERE revision_id = ?")
            .bind(&revision_id)
            .fetch_one(&state.db)
            .await?;
        if existing == 0 {
            insert_build(
                &state.db,
                base,
                &revision_id,
                revision.get::<i64, _>("allow_check") == 1,
                "approved",
            )
            .await?;
            created += 1;
        }
    }
    Ok(created)
}

pub async fn insert_build(
    db: &SqlitePool,
    package_base: &str,
    revision_id: &str,
    allow_check: bool,
    reason: &str,
) -> sqlx::Result<String> {
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO builds(id, revision_id, package_base, state, reason, allow_check, created_at) VALUES (?, ?, ?, 'queued', ?, ?, ?)")
        .bind(&id)
        .bind(revision_id)
        .bind(package_base)
        .bind(reason)
        .bind(i64::from(allow_check))
        .bind(Utc::now())
        .execute(db)
        .await?;
    Ok(id)
}

/// 失败处理：瞬态失败且未超过重试上限时回到 queued。
async fn fail_build(
    state: &AppState,
    build_id: &str,
    attempt: i64,
    code: &str,
    log: Option<&str>,
) -> anyhow::Result<&'static str> {
    let class = classify_error_code(code);
    let retry = class == ErrorClass::Transient && attempt <= MAXIMUM_TRANSIENT_RETRIES;
    let now = Utc::now();
    if retry {
        sqlx::query("UPDATE builds SET state = 'queued', attempt = attempt + 1, builder_id = NULL, lease_expires_at = NULL, error_code = ?, error_class = ?, log = COALESCE(?, log), artifacts_json = '[]' WHERE id = ?")
            .bind(code)
            .bind(class.as_str())
            .bind(log)
            .bind(build_id)
            .execute(&state.db)
            .await?;
    } else {
        sqlx::query("UPDATE builds SET state = 'failed', lease_expires_at = NULL, error_code = ?, error_class = ?, log = COALESCE(?, log), finished_at = ? WHERE id = ?")
            .bind(code)
            .bind(class.as_str())
            .bind(log)
            .bind(now)
            .bind(build_id)
            .execute(&state.db)
            .await?;
    }
    let directory = artifact_dir(state, build_id);
    if directory.exists() {
        tokio::fs::remove_dir_all(&directory).await.ok();
    }
    state.wake();
    Ok(if retry { "queued" } else { "failed" })
}

/// 租约过期视为瞬态失败（替代旧的“不确定”状态）。
pub async fn expire_leases(state: &AppState) -> anyhow::Result<usize> {
    let expired = sqlx::query("SELECT id, attempt FROM builds WHERE state IN ('running', 'uploading') AND lease_expires_at < ?")
        .bind(Utc::now())
        .fetch_all(&state.db)
        .await?;
    for row in &expired {
        fail_build(
            state,
            row.get("id"),
            row.get("attempt"),
            "LEASE_EXPIRED",
            None,
        )
        .await?;
    }
    Ok(expired.len())
}

// ---------------------------------------------------------------- Builder API

pub async fn lease(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LeaseRequest>,
) -> Result<Json<LeaseResponse>, ApiError> {
    require_builder(&state, &headers)?;
    if request.builder_id.is_empty()
        || request.builder_id.len() > 64
        || !request
            .builder_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        return Err(ApiError::bad_request(
            "INVALID_BUILDER_ID",
            "builder_id 只能包含字母数字和 -_.",
        ));
    }
    state.mark_builder_seen(&request.builder_id);
    expire_leases(&state).await.map_err(ApiError::internal)?;
    let closure = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let queued = sqlx::query("SELECT builds.id, builds.package_base, builds.attempt, builds.allow_check, revisions.id AS revision_id, revisions.aur_commit, revisions.vcs_commit, revisions.tree_sha256, revisions.snapshot_json FROM builds JOIN revisions ON revisions.id = builds.revision_id WHERE builds.state = 'queued' ORDER BY builds.created_at")
        .fetch_all(&state.db)
        .await
        .map_err(ApiError::internal)?;
    for row in queued {
        let package_base: String = row.get("package_base");
        let Ok(dependencies) = dependency_builds(&state, &closure, &package_base)
            .await
            .map_err(ApiError::internal)?
        else {
            continue;
        };
        let build_id: String = row.get("id");
        let lease_expires_at = Utc::now()
            + Duration::seconds(
                i64::try_from(request.lease_seconds.clamp(600, 172_800)).unwrap_or(600),
            );
        let dep_ids: Vec<&String> = dependencies.iter().map(|(id, _)| id).collect();
        let claimed = sqlx::query("UPDATE builds SET state = 'running', builder_id = ?, lease_expires_at = ?, started_at = ?, dep_build_ids_json = ?, error_code = NULL, error_class = NULL WHERE id = ? AND state = 'queued'")
            .bind(&request.builder_id)
            .bind(lease_expires_at)
            .bind(Utc::now())
            .bind(serde_json::to_string(&dep_ids).map_err(ApiError::internal)?)
            .bind(&build_id)
            .execute(&state.db)
            .await
            .map_err(ApiError::internal)?;
        if claimed.rows_affected() != 1 {
            continue;
        }
        let snapshot: Snapshot =
            serde_json::from_str(row.get("snapshot_json")).map_err(ApiError::internal)?;
        let spec = BuildSpec {
            build_id: build_id.clone(),
            attempt: row.get("attempt"),
            package_base,
            revision_id: row.get("revision_id"),
            aur_commit: row.get("aur_commit"),
            vcs_commit: row.get("vcs_commit"),
            tree_sha256: row.get("tree_sha256"),
            expected_outputs: snapshot.info.outputs.clone(),
            allow_check: row.get::<i64, _>("allow_check") == 1,
            files: snapshot
                .files
                .iter()
                .map(|file| InputFile {
                    path: file.path.clone(),
                    sha256: file.sha256.clone(),
                    size: file.size,
                    content_base64: file.content_base64.clone(),
                })
                .collect(),
            dependencies: dependencies
                .iter()
                .flat_map(|(id, artifacts)| {
                    artifacts.iter().map(move |artifact| DependencyArtifact {
                        build_id: id.clone(),
                        file: artifact.file.clone(),
                        sha256: artifact.sha256.clone(),
                        size: artifact.size,
                    })
                })
                .collect(),
            lease_expires_at,
        };
        tracing::info!(build = %build_id, builder = %request.builder_id, "构建已租出");
        return Ok(Json(LeaseResponse { build: Some(spec) }));
    }
    Ok(Json(LeaseResponse { build: None }))
}

struct ActiveBuild {
    attempt: i64,
    state: String,
    artifacts: Vec<ArtifactRecord>,
    expected_outputs: Vec<String>,
}

async fn active_build(state: &AppState, build_id: &str) -> Result<ActiveBuild, ApiError> {
    let row = sqlx::query("SELECT builds.attempt, builds.state, builds.lease_expires_at > ? AS valid, builds.artifacts_json, revisions.snapshot_json FROM builds JOIN revisions ON revisions.id = builds.revision_id WHERE builds.id = ?")
        .bind(Utc::now())
        .bind(build_id)
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("没有这个构建"))?;
    let build_state: String = row.get("state");
    if !matches!(build_state.as_str(), "running" | "uploading") || !row.get::<bool, _>("valid") {
        return Err(ApiError::conflict("LEASE_NOT_ACTIVE", "构建租约已失效"));
    }
    let snapshot: Snapshot =
        serde_json::from_str(row.get("snapshot_json")).map_err(ApiError::internal)?;
    Ok(ActiveBuild {
        attempt: row.get("attempt"),
        state: build_state,
        artifacts: parse_artifacts(row.get("artifacts_json")),
        expected_outputs: snapshot.info.outputs,
    })
}

/// 校验 Builder 回报的产物清单。
pub fn validate_artifacts(
    artifacts: &[ArtifactRecord],
    expected_outputs: &[String],
) -> Result<(), String> {
    if artifacts.is_empty() || artifacts.len() > 64 {
        return Err("产物数量必须在 1 到 64 之间".into());
    }
    let mut files = BTreeSet::new();
    let mut names = BTreeSet::new();
    let version = &artifacts[0].package_version;
    for artifact in artifacts {
        validate_artifact_file_name(&artifact.file).map_err(|error| error.to_string())?;
        if !aursmith_core::is_sha256_hex(&artifact.sha256)
            || artifact.size == 0
            || artifact.size > MAXIMUM_ARTIFACT_BYTES
        {
            return Err(format!("产物摘要或大小无效：{}", artifact.file));
        }
        if !matches!(artifact.architecture.as_str(), "x86_64" | "any")
            || &artifact.package_version != version
        {
            return Err(format!("产物架构或版本无效：{}", artifact.file));
        }
        if !artifact.file.starts_with(&format!(
            "{}-{}-{}.pkg.tar.",
            artifact.package_name, artifact.package_version, artifact.architecture
        )) {
            return Err(format!("产物文件名与元数据不一致：{}", artifact.file));
        }
        if !files.insert(&artifact.file) || !names.insert(&artifact.package_name) {
            return Err(format!("产物重复：{}", artifact.file));
        }
    }
    let expected: BTreeSet<&String> = expected_outputs.iter().collect();
    if names != expected {
        return Err(format!(
            "产物包名 {names:?} 与 .SRCINFO 输出 {expected:?} 不一致"
        ));
    }
    Ok(())
}

pub async fn report(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(build_id): Path<String>,
    Json(report): Json<BuildReport>,
) -> Result<Json<Value>, ApiError> {
    require_builder(&state, &headers)?;
    let build = active_build(&state, &build_id).await?;
    if build.attempt != report.attempt || build.state != "running" {
        return Err(ApiError::conflict(
            "ATTEMPT_MISMATCH",
            "回报与当前构建尝试不一致",
        ));
    }
    let log: String = if report.log.len() > MAXIMUM_LOG_BYTES {
        let mut cut = report.log.len() - MAXIMUM_LOG_BYTES;
        while !report.log.is_char_boundary(cut) {
            cut += 1;
        }
        format!("…（已截断）\n{}", &report.log[cut..])
    } else {
        report.log.clone()
    };
    match report.outcome {
        BuildOutcome::Failed => {
            let code = report
                .error_code
                .as_deref()
                .filter(|code| !code.is_empty() && code.len() <= 64)
                .unwrap_or("BUILD_FAILED");
            let next = fail_build(&state, &build_id, build.attempt, code, Some(&log))
                .await
                .map_err(ApiError::internal)?;
            Ok(Json(json!({"build_id": build_id, "state": next})))
        }
        BuildOutcome::Succeeded => {
            if let Err(message) = validate_artifacts(&report.artifacts, &build.expected_outputs) {
                fail_build(
                    &state,
                    &build_id,
                    build.attempt,
                    "OUTPUT_MISMATCH",
                    Some(&format!("{log}\n{message}")),
                )
                .await
                .map_err(ApiError::internal)?;
                return Err(ApiError::bad_request("OUTPUT_MISMATCH", message));
            }
            sqlx::query("UPDATE builds SET state = 'uploading', artifacts_json = ?, log = ? WHERE id = ? AND state = 'running'")
                .bind(serde_json::to_string(&report.artifacts).map_err(ApiError::internal)?)
                .bind(&log)
                .bind(&build_id)
                .execute(&state.db)
                .await
                .map_err(ApiError::internal)?;
            tokio::fs::create_dir_all(artifact_dir(&state, &build_id))
                .await
                .map_err(ApiError::internal)?;
            Ok(Json(
                json!({"build_id": build_id, "state": "uploading", "chunk_bytes": UPLOAD_CHUNK_BYTES}),
            ))
        }
    }
}

async fn upload_status(
    state: &AppState,
    build_id: &str,
    artifact: &ArtifactRecord,
) -> std::io::Result<UploadStatus> {
    let directory = artifact_dir(state, build_id);
    let complete = tokio::fs::metadata(directory.join(&artifact.file))
        .await
        .is_ok();
    let received = if complete {
        artifact.size
    } else {
        tokio::fs::metadata(directory.join(format!("{}.part", artifact.file)))
            .await
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    };
    Ok(UploadStatus {
        file: artifact.file.clone(),
        size: artifact.size,
        received,
        complete,
    })
}

fn find_artifact<'a>(build: &'a ActiveBuild, file: &str) -> Result<&'a ArtifactRecord, ApiError> {
    if build.state != "uploading" {
        return Err(ApiError::conflict("NOT_UPLOADING", "构建尚未进入上传阶段"));
    }
    build
        .artifacts
        .iter()
        .find(|artifact| artifact.file == file)
        .ok_or_else(|| ApiError::not_found("该文件不在构建回报的产物清单中"))
}

pub async fn upload_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((build_id, file)): Path<(String, String)>,
) -> Result<Json<UploadStatus>, ApiError> {
    require_builder(&state, &headers)?;
    let build = active_build(&state, &build_id).await?;
    let artifact = find_artifact(&build, &file)?;
    Ok(Json(
        upload_status(&state, &build_id, artifact)
            .await
            .map_err(ApiError::internal)?,
    ))
}

#[derive(Deserialize)]
pub struct OffsetQuery {
    offset: u64,
}

/// 追加一个分片。偏移必须等于已接收字节数，因此重传是幂等且可断点续传的。
pub async fn upload_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((build_id, file)): Path<(String, String)>,
    Query(query): Query<OffsetQuery>,
    body: Bytes,
) -> Result<Json<UploadStatus>, ApiError> {
    require_builder(&state, &headers)?;
    let build = active_build(&state, &build_id).await?;
    let artifact = find_artifact(&build, &file)?.clone();
    let status = upload_status(&state, &build_id, &artifact)
        .await
        .map_err(ApiError::internal)?;
    if status.complete {
        return Ok(Json(status));
    }
    let length = u64::try_from(body.len()).map_err(ApiError::internal)?;
    if query.offset != status.received {
        return Err(ApiError::conflict(
            "OFFSET_MISMATCH",
            format!("期望偏移 {}", status.received),
        ));
    }
    if length == 0 || length > UPLOAD_CHUNK_BYTES || query.offset + length > artifact.size {
        return Err(ApiError::bad_request("CHUNK_INVALID", "分片大小无效"));
    }
    let directory = artifact_dir(&state, &build_id);
    let partial = directory.join(format!("{file}.part"));
    let mut handle = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&partial)
        .await
        .map_err(ApiError::internal)?;
    handle.write_all(&body).await.map_err(ApiError::internal)?;
    handle.sync_all().await.map_err(ApiError::internal)?;
    drop(handle);
    let received = query.offset + length;
    if received == artifact.size {
        let bytes_path = partial.clone();
        let digest = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
            let mut file = std::fs::File::open(bytes_path)?;
            let mut hasher = Sha256::new();
            std::io::copy(&mut file, &mut hasher)?;
            Ok(hex::encode(hasher.finalize()))
        })
        .await
        .map_err(ApiError::internal)?
        .map_err(ApiError::internal)?;
        if digest != artifact.sha256 {
            tokio::fs::remove_file(&partial).await.ok();
            return Err(ApiError::bad_request(
                "DIGEST_MISMATCH",
                "上传内容与回报的 SHA-256 不一致，请重新上传",
            ));
        }
        tokio::fs::rename(&partial, directory.join(&file))
            .await
            .map_err(ApiError::internal)?;
    }
    Ok(Json(
        upload_status(&state, &build_id, &artifact)
            .await
            .map_err(ApiError::internal)?,
    ))
}

pub async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(build_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_builder(&state, &headers)?;
    let build = active_build(&state, &build_id).await?;
    if build.state != "uploading" {
        return Err(ApiError::conflict("NOT_UPLOADING", "构建尚未进入上传阶段"));
    }
    for artifact in &build.artifacts {
        if !upload_status(&state, &build_id, artifact)
            .await
            .map_err(ApiError::internal)?
            .complete
        {
            return Err(ApiError::conflict(
                "UPLOAD_INCOMPLETE",
                format!("{} 尚未上传完成", artifact.file),
            ));
        }
    }
    sqlx::query("UPDATE builds SET state = 'succeeded', lease_expires_at = NULL, finished_at = ? WHERE id = ? AND state = 'uploading'")
        .bind(Utc::now())
        .bind(&build_id)
        .execute(&state.db)
        .await
        .map_err(ApiError::internal)?;
    tracing::info!(build = %build_id, "构建成功并完成上传");
    state.wake();
    Ok(Json(json!({"build_id": build_id, "state": "succeeded"})))
}

/// Builder 下载同一期望状态内已构建的 AUR 依赖。
pub async fn download_artifact(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((build_id, file)): Path<(String, String)>,
) -> Result<Response, ApiError> {
    require_builder(&state, &headers)?;
    let artifacts_json: Option<String> = sqlx::query_scalar(
        "SELECT artifacts_json FROM builds WHERE id = ? AND state = 'succeeded'",
    )
    .bind(&build_id)
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::internal)?;
    let artifacts =
        parse_artifacts(&artifacts_json.ok_or_else(|| ApiError::not_found("没有这个成功构建"))?);
    let artifact = artifacts
        .iter()
        .find(|artifact| artifact.file == file)
        .ok_or_else(|| ApiError::not_found("没有这个产物"))?;
    let handle = tokio::fs::File::open(artifact_dir(&state, &build_id).join(&artifact.file))
        .await
        .map_err(|_| ApiError::not_found("产物文件已被清理"))?;
    let stream = tokio_util_stream(handle);
    Ok((
        [
            (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
            (header::CONTENT_LENGTH, artifact.size.to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

fn tokio_util_stream(
    file: tokio::fs::File,
) -> impl futures_util::Stream<Item = std::io::Result<Bytes>> {
    futures_util::stream::unfold(file, |mut file| async move {
        use tokio::io::AsyncReadExt;
        let mut buffer = vec![0_u8; 256 * 1024];
        match file.read(&mut buffer).await {
            Ok(0) => None,
            Ok(count) => {
                buffer.truncate(count);
                Some((Ok(Bytes::from(buffer)), file))
            }
            Err(error) => Some((Err(error), file)),
        }
    })
}

// ---------------------------------------------------------------- 管理 API

pub async fn recent_builds(
    db: &SqlitePool,
    package_base: Option<&str>,
    limit: i64,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query("SELECT builds.*, revisions.version, revisions.aur_commit, revisions.vcs_commit FROM builds JOIN revisions ON revisions.id = builds.revision_id WHERE (? IS NULL OR builds.package_base = ?) ORDER BY builds.created_at DESC LIMIT ?")
        .bind(package_base)
        .bind(package_base)
        .bind(limit)
        .fetch_all(db)
        .await
        .map_err(ApiError::internal)?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "package_base": row.get::<String, _>("package_base"),
                "revision_id": row.get::<String, _>("revision_id"),
                "version": row.get::<String, _>("version"),
                "aur_commit": row.get::<String, _>("aur_commit"),
                "vcs_commit": row.get::<Option<String>, _>("vcs_commit"),
                "state": row.get::<String, _>("state"),
                "attempt": row.get::<i64, _>("attempt"),
                "reason": row.get::<String, _>("reason"),
                "builder_id": row.get::<Option<String>, _>("builder_id"),
                "error_code": row.get::<Option<String>, _>("error_code"),
                "error_class": row.get::<Option<String>, _>("error_class"),
                "artifacts": serde_json::from_str::<Value>(row.get("artifacts_json")).unwrap_or(Value::Null),
                "has_log": row.get::<Option<String>, _>("log").is_some_and(|log| !log.is_empty()),
                "lease_expires_at": row.get::<Option<String>, _>("lease_expires_at"),
                "created_at": row.get::<String, _>("created_at"),
                "started_at": row.get::<Option<String>, _>("started_at"),
                "finished_at": row.get::<Option<String>, _>("finished_at"),
            })
        })
        .collect())
}

pub async fn list(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let mut items = recent_builds(&state.db, None, 200).await?;
    let closure = compute_closure(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let mut waiting: BTreeMap<String, Option<String>> = BTreeMap::new();
    for item in &mut items {
        if item["state"] == "queued" {
            let base = item["package_base"].as_str().unwrap_or_default().to_owned();
            if !waiting.contains_key(&base) {
                let reason = dependency_builds(&state, &closure, &base)
                    .await
                    .map_err(ApiError::internal)?
                    .err();
                waiting.insert(base.clone(), reason);
            }
            item["waiting_reason"] = json!(waiting[&base]);
        }
    }
    Ok(Json(json!({"items": items})))
}

pub async fn log(
    State(state): State<AppState>,
    Path(build_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT log, error_code, state FROM builds WHERE id = ?")
        .bind(&build_id)
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("没有这个构建"))?;
    Ok(Json(json!({
        "build_id": build_id,
        "state": row.get::<String, _>("state"),
        "error_code": row.get::<Option<String>, _>("error_code"),
        "log": row.get::<Option<String>, _>("log").unwrap_or_default(),
    })))
}

/// 手工重建当前已批准 revision。
pub async fn rebuild(
    State(state): State<AppState>,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let revision = sqlx::query("SELECT revisions.id, packages.allow_check FROM revisions JOIN packages ON packages.package_base = revisions.package_base WHERE revisions.package_base = ? AND revisions.state = 'approved' ORDER BY revisions.created_at DESC LIMIT 1")
        .bind(&package_base)
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::conflict("NO_APPROVED_REVISION", "没有已批准的 revision，无法重建"))?;
    let active: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM builds WHERE package_base = ? AND state IN ('queued', 'running', 'uploading')")
        .bind(&package_base)
        .fetch_one(&state.db)
        .await
        .map_err(ApiError::internal)?;
    if active > 0 {
        return Err(ApiError::conflict(
            "BUILD_ALREADY_ACTIVE",
            "该软件包已有排队或进行中的构建",
        ));
    }
    let id = insert_build(
        &state.db,
        &package_base,
        &revision.get::<String, _>("id"),
        revision.get::<i64, _>("allow_check") == 1,
        "manual",
    )
    .await
    .map_err(ApiError::internal)?;
    state.wake();
    Ok(Json(
        json!({"package_base": package_base, "build_id": id, "state": "queued"}),
    ))
}
