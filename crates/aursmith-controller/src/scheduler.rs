use crate::{error::ApiError, routes::AppState, transport};
use aursmith_domain::AttemptRef;
use aursmith_protocol::{
    ArtifactRecord, DependencyInput, DependencySource, GuestResult, JobKind, JobSpec, ReleasePlan,
    ResourceLimits,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use chrono::{Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

pub fn spawn(state: AppState) {
    let upstream_state = state.clone();
    tokio::spawn(async move {
        let initial_jitter = 300 + u64::from(std::process::id() % 600);
        tokio::time::sleep(std::time::Duration::from_secs(initial_jitter)).await;
        let mut timer = interval(std::time::Duration::from_secs(60));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = crate::packages::refresh_due(&upstream_state).await {
                tracing::warn!(%error, "AUR 到期订阅轮询失败");
            }
        }
    });
    let audit_state = state.clone();
    tokio::spawn(async move {
        if let Err(error) = crate::audits::recover_interrupted(&audit_state).await {
            tracing::warn!(%error, "中断的 Agent 审计恢复失败");
        }
        let mut timer = interval(std::time::Duration::from_secs(3));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = crate::audits::reconcile_completed(&audit_state).await {
                tracing::warn!(%error, "已完成 Agent 审计对账失败");
            }
            let dispatch_state = audit_state.clone();
            tokio::spawn(async move {
                if let Err(error) = crate::audits::dispatch_one(&dispatch_state).await {
                    tracing::warn!(%error, "Agent 审计调度失败");
                }
            });
        }
    });
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(2));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = reconcile_one(&state).await {
                tracing::warn!(%error, "不确定任务对账失败");
            }
            if let Err(error) = dispatch_transfer_one(&state).await {
                tracing::warn!(%error, "Artifact 传输调度失败");
            }
            if let Err(error) = dispatch_release_one(&state).await {
                tracing::warn!(%error, "Release 发布调度失败");
            }
        }
    });
}

async fn dispatch_release_one(state: &AppState) -> Result<(), ApiError> {
    if publication_backpressure(&state.database).await? {
        return Ok(());
    }
    let pending = sqlx::query("SELECT release_id, state, plan_json, attempt_count FROM release_jobs WHERE state IN ('issued', 'signing') ORDER BY updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = pending {
        let release_id: String = row.get("release_id");
        let authorization_state: String = row.get("state");
        if authorization_state == "issued" {
            let plan: ReleasePlan =
                serde_json::from_str(row.get("plan_json")).map_err(ApiError::internal)?;
            match transport::authorize_release(
                &state.config,
                &state.config.publisher_endpoint,
                &plan,
            )
            .await
            {
                Ok(_) => {
                    sqlx::query("UPDATE release_jobs SET state = 'signing', last_error = NULL, updated_at = ? WHERE release_id = ?")
                        .bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
                }
                Err(error) => {
                    let attempts: i64 = row.get("attempt_count");
                    let terminal = attempts + 1 >= 3;
                    sqlx::query("UPDATE release_jobs SET state = CASE WHEN ? THEN 'failed' ELSE state END, attempt_count = attempt_count + 1, last_error = ?, updated_at = ? WHERE release_id = ?")
                        .bind(terminal).bind(error.to_string()).bind(Utc::now()).bind(&release_id)
                        .execute(&state.database).await.map_err(ApiError::internal)?;
                    if terminal {
                        fail_release(state, &release_id, &error.to_string()).await?;
                    }
                }
            }
        } else {
            match transport::query_release(
                &state.config,
                &state.config.publisher_endpoint,
                &release_id,
            )
            .await
            {
                Ok(reply) if reply.data["state"].as_str() == Some("published") => {
                    let manifest_sha256 = reply.data["manifest_sha256"]
                        .as_str()
                        .filter(|value| value.len() == 64)
                        .ok_or_else(|| {
                            ApiError::internal("Publisher 没有返回有效 Release Manifest 摘要")
                        })?;
                    let mut transaction =
                        state.database.begin().await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE releases SET state = 'committed', manifest_sha256 = ?, committed_at = ? WHERE id = ?")
                        .bind(manifest_sha256).bind(Utc::now()).bind(&release_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE release_jobs SET state = 'published', last_error = NULL, updated_at = ? WHERE release_id = ?")
                        .bind(Utc::now()).bind(&release_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE release_batches SET state = 'published', current_release_id = ?, failure_reason = NULL, updated_at = ? WHERE id = (SELECT batch_id FROM releases WHERE id = ?)")
                        .bind(&release_id).bind(Utc::now()).bind(&release_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE revisions SET state = 'published' WHERE id IN (SELECT release_batch_revisions.revision_id FROM release_batch_revisions JOIN releases ON releases.batch_id = release_batch_revisions.batch_id WHERE releases.id = ?)")
                        .bind(&release_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('current_release_id', ?, ?) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at")
                        .bind(json!(release_id).to_string()).bind(Utc::now()).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    transaction.commit().await.map_err(ApiError::internal)?;
                }
                Ok(reply) if reply.data["state"].as_str() == Some("failed") => {
                    fail_release(
                        state,
                        &release_id,
                        reply.data["last_error"]
                            .as_str()
                            .unwrap_or("Publisher 发布失败"),
                    )
                    .await?;
                }
                Ok(_) => {}
                Err(error) => {
                    sqlx::query("UPDATE release_jobs SET last_error = ?, updated_at = ? WHERE release_id = ?")
                        .bind(error.to_string()).bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
                }
            }
        }
        return Ok(());
    }

    let batch = sqlx::query("SELECT id, state, graph_json FROM release_batches WHERE state IN ('artifacts_ready', 'queued_removal') AND NOT EXISTS (SELECT 1 FROM releases WHERE releases.batch_id = release_batches.id) ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(batch) = batch else {
        return Ok(());
    };
    let batch_id: String = batch.get("id");
    let batch_state: String = batch.get("state");
    let removed_package_names = if batch_state == "queued_removal" {
        let graph: serde_json::Value =
            serde_json::from_str(batch.get("graph_json")).map_err(ApiError::internal)?;
        let package_bases: Vec<String> = serde_json::from_value(
            graph
                .get("remove")
                .cloned()
                .ok_or_else(|| ApiError::internal("清除批次缺少 remove 清单"))?,
        )
        .map_err(ApiError::internal)?;
        if package_bases.is_empty() {
            return Err(ApiError::conflict(
                "REMOVAL_TARGET_MISSING",
                "清除批次没有软件包目标",
            ));
        }
        let mut outputs = BTreeSet::new();
        for package_base in package_bases {
            let snapshots: Vec<String> =
                sqlx::query_scalar("SELECT metadata_json FROM revisions WHERE package_base = ?")
                    .bind(&package_base)
                    .fetch_all(&state.database)
                    .await
                    .map_err(ApiError::internal)?;
            if snapshots.is_empty() {
                return Err(ApiError::conflict(
                    "REMOVAL_METADATA_MISSING",
                    format!("清除目标 {package_base} 没有 Revision 元数据"),
                ));
            }
            for snapshot in snapshots {
                let value: serde_json::Value =
                    serde_json::from_str(&snapshot).map_err(ApiError::internal)?;
                outputs.extend(
                    value["outputs"]
                        .as_array()
                        .ok_or_else(|| ApiError::internal("Revision 元数据缺少 outputs"))?
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned)),
                );
            }
        }
        outputs
    } else {
        BTreeSet::new()
    };
    let artifact_rows = sqlx::query("SELECT artifacts.path, artifacts.sha256, artifacts.size, artifacts.package_name, artifacts.package_version, artifacts.architecture FROM artifacts JOIN jobs ON jobs.id = artifacts.job_id WHERE jobs.batch_id = ? AND jobs.kind = 'build' AND jobs.status = 'succeeded' ORDER BY artifacts.path")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    if artifact_rows.is_empty() && batch_state != "queued_removal" {
        return Err(ApiError::conflict(
            "ARTIFACTS_MISSING",
            "ReleaseBatch 没有可发布 Artifact",
        ));
    }
    let previous_rows = if let Some(current_release_id) =
        current_release_id(&state.database).await?
    {
        sqlx::query("SELECT artifacts.path, artifacts.sha256, artifacts.size, artifacts.package_name, artifacts.package_version, artifacts.architecture FROM artifacts JOIN release_artifacts ON release_artifacts.artifact_sha256 = artifacts.sha256 WHERE release_artifacts.release_id = ? ORDER BY artifacts.path")
            .bind(current_release_id).fetch_all(&state.database).await.map_err(ApiError::internal)?
    } else {
        Vec::new()
    };
    let parse_artifact = |row: sqlx::sqlite::SqliteRow| -> Result<ArtifactRecord, ApiError> {
        Ok(ArtifactRecord {
            path: row.get("path"),
            sha256: row.get("sha256"),
            size: u64::try_from(row.get::<i64, _>("size")).map_err(ApiError::internal)?,
            package_name: row.get("package_name"),
            package_version: row.get("package_version"),
            architecture: row.get("architecture"),
        })
    };
    let previous_artifacts = previous_rows
        .into_iter()
        .map(&parse_artifact)
        .collect::<Result<Vec<_>, _>>()?;
    let previous_artifacts = remove_release_artifacts(previous_artifacts, &removed_package_names);
    let changed_artifacts = artifact_rows
        .into_iter()
        .map(parse_artifact)
        .collect::<Result<Vec<_>, _>>()?;
    let artifacts = merge_release_artifacts(previous_artifacts, changed_artifacts);
    let revision_sha256s = sqlx::query_scalar::<_, String>("SELECT DISTINCT revisions.input_sha256 FROM revisions JOIN jobs ON jobs.revision_id = revisions.id WHERE jobs.batch_id = ? ORDER BY revisions.input_sha256")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let audit_report_sha256s = sqlx::query_scalar::<_, String>("SELECT DISTINCT audit_decisions.report_sha256 FROM audit_decisions JOIN jobs ON jobs.revision_id = audit_decisions.revision_id WHERE jobs.batch_id = ? ORDER BY audit_decisions.report_sha256")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let mut evidence_files = Vec::new();
    let current_evidence = sqlx::query("SELECT job_evidence_files.path, job_evidence_files.sha256, job_evidence_files.size FROM job_evidence_files JOIN jobs ON jobs.id = job_evidence_files.job_id WHERE jobs.batch_id = ? AND jobs.kind = 'build' AND jobs.status = 'succeeded' ORDER BY job_evidence_files.path")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    evidence_files.extend(current_evidence.into_iter().map(|entry| {
        aursmith_protocol::ManifestEntry {
            path: entry.get("path"),
            sha256: entry.get("sha256"),
            size: u64::try_from(entry.get::<i64, _>("size")).unwrap_or_default(),
        }
    }));
    evidence_files.sort_by(|left, right| left.path.cmp(&right.path));
    let unique_count = evidence_files
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    if unique_count != evidence_files.len()
        || evidence_files.iter().any(|entry| {
            aursmith_protocol::validate_relative_path(&entry.path).is_err()
                || !entry.path.starts_with("evidence/")
                || entry.sha256.len() != 64
                || entry.size == 0
        })
    {
        return Err(ApiError::conflict(
            "RELEASE_EVIDENCE_FILES_INVALID",
            "Release 的证据文件清单为空、重复或元数据无效",
        ));
    }
    let release_id = Uuid::new_v4();
    let now = Utc::now();
    let authorization = ReleasePlan {
        release_id,
        batch_id: Uuid::parse_str(&batch_id).map_err(ApiError::internal)?,
        repository_name: state.config.repository_name.clone(),
        source_git_commit: state.config.source_git_commit.clone(),
        revision_sha256s,
        audit_report_sha256s,
        artifacts,
        evidence_files,
        removed_package_names: removed_package_names.into_iter().collect(),
        include_repository_keyring: true,
        issued_at: now,
        expires_at: now + Duration::hours(1),
    };
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO releases(id, batch_id, state, manifest_sha256, source_git_commit, created_at) VALUES (?, ?, 'authorizing', ?, ?, ?)")
        .bind(release_id.to_string()).bind(&batch_id).bind(format!("pending:{release_id}"))
        .bind(&state.config.source_git_commit).bind(now)
        .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO release_jobs(release_id, state, plan_json, expires_at, created_at, updated_at) VALUES (?, 'issued', ?, ?, ?, ?)")
        .bind(release_id.to_string())
        .bind(serde_json::to_string(&authorization).map_err(ApiError::internal)?)
        .bind(authorization.expires_at).bind(now).bind(now).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    for artifact in &authorization.artifacts {
        sqlx::query("INSERT INTO release_artifacts(release_id, artifact_sha256) VALUES (?, ?)")
            .bind(release_id.to_string())
            .bind(&artifact.sha256)
            .execute(&mut *transaction)
            .await
            .map_err(ApiError::internal)?;
    }
    sqlx::query("UPDATE release_batches SET state = 'publishing', updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(batch_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(())
}

async fn fail_release(state: &AppState, release_id: &str, error: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE releases SET state = 'failed' WHERE id = ?")
        .bind(release_id)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("UPDATE release_jobs SET state = 'failed', last_error = ?, updated_at = ? WHERE release_id = ?")
        .bind(error).bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE release_batches SET state = 'publish_failed', failure_reason = ?, updated_at = ? WHERE id = (SELECT batch_id FROM releases WHERE id = ?)")
        .bind(error).bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(())
}

fn merge_release_artifacts(
    previous: Vec<ArtifactRecord>,
    changed: Vec<ArtifactRecord>,
) -> Vec<ArtifactRecord> {
    let mut complete = BTreeMap::new();
    for artifact in previous.into_iter().chain(changed) {
        complete.insert(
            artifact
                .package_name
                .clone()
                .unwrap_or_else(|| artifact.path.clone()),
            artifact,
        );
    }
    complete.into_values().collect()
}

fn remove_release_artifacts(
    artifacts: Vec<ArtifactRecord>,
    removed_package_names: &BTreeSet<String>,
) -> Vec<ArtifactRecord> {
    artifacts
        .into_iter()
        .filter(|artifact| {
            artifact
                .package_name
                .as_ref()
                .is_none_or(|name| !removed_package_names.contains(name))
        })
        .collect()
}

async fn current_release_id(database: &sqlx::SqlitePool) -> Result<Option<String>, ApiError> {
    sqlx::query_scalar("SELECT COALESCE((SELECT json_extract(value_json, '$') FROM system_settings WHERE key = 'current_release_id'), (SELECT id FROM releases WHERE state = 'committed' ORDER BY committed_at DESC LIMIT 1))")
        .fetch_optional(database)
        .await
        .map_err(ApiError::internal)
        .map(Option::flatten)
}

async fn dispatch_transfer_one(state: &AppState) -> Result<(), ApiError> {
    sqlx::query("UPDATE uploads SET state = 'expired', updated_at = ? WHERE state IN ('issued', 'export_ready') AND expires_at <= ?")
        .bind(Utc::now()).bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
    let candidate = sqlx::query("SELECT jobs.id AS job_id, jobs.batch_id FROM jobs WHERE jobs.kind = 'build' AND jobs.status = 'succeeded' AND jobs.batch_id IN (SELECT id FROM release_batches WHERE state = 'ready_to_publish') AND NOT EXISTS (SELECT 1 FROM uploads WHERE uploads.source_job_id = jobs.id AND uploads.state IN ('issued', 'export_ready', 'verified')) ORDER BY jobs.updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = candidate {
        let job_id: String = row.get("job_id");
        let attempt = sqlx::query("SELECT id, generation FROM attempts WHERE job_id = ? AND status = 'succeeded' ORDER BY generation DESC LIMIT 1")
            .bind(&job_id).fetch_one(&state.database).await.map_err(ApiError::internal)?;
        let artifacts =
            sqlx::query("SELECT path, sha256, size FROM artifacts WHERE job_id = ? ORDER BY path")
                .bind(&job_id)
                .fetch_all(&state.database)
                .await
                .map_err(ApiError::internal)?;
        if artifacts.is_empty() {
            return Err(ApiError::conflict(
                "ARTIFACTS_MISSING",
                "成功 Build Job 没有 Artifact 记录",
            ));
        }
        let mut files = artifacts
            .into_iter()
            .map(|artifact| aursmith_protocol::ManifestEntry {
                path: artifact.get("path"),
                sha256: artifact.get("sha256"),
                size: u64::try_from(artifact.get::<i64, _>("size")).unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let evidence_files = sqlx::query(
            "SELECT path, sha256, size FROM job_evidence_files WHERE job_id = ? ORDER BY path",
        )
        .bind(&job_id)
        .fetch_all(&state.database)
        .await
        .map_err(ApiError::internal)?;
        files.extend(
            evidence_files
                .into_iter()
                .map(|entry| aursmith_protocol::ManifestEntry {
                    path: entry.get("path"),
                    sha256: entry.get("sha256"),
                    size: u64::try_from(entry.get::<i64, _>("size")).unwrap_or_default(),
                }),
        );
        let transfer_id = Uuid::new_v4();
        let now = Utc::now();
        let expires_at = now + Duration::hours(1);
        let upload = aursmith_protocol::BuilderUpload {
            id: transfer_id,
            attempt: AttemptRef {
                job_id: Uuid::parse_str(&job_id).map_err(ApiError::internal)?,
                attempt_id: Uuid::parse_str(attempt.get("id")).map_err(ApiError::internal)?,
                generation: u32::try_from(attempt.get::<i64, _>("generation"))
                    .map_err(ApiError::internal)?,
            },
            files,
            expires_at,
        };
        sqlx::query("INSERT INTO uploads(id, batch_id, source_job_id, state, request_json, expires_at, created_at, updated_at) VALUES (?, ?, ?, 'issued', ?, ?, ?, ?)")
            .bind(transfer_id.to_string()).bind(row.get::<String,_>("batch_id")).bind(&job_id)
            .bind(serde_json::to_string(&upload).map_err(ApiError::internal)?)
            .bind(expires_at).bind(now).bind(now).execute(&state.database).await.map_err(ApiError::internal)?;
        return Ok(());
    }

    let batches: Vec<String> = sqlx::query_scalar("SELECT id FROM release_batches WHERE state = 'ready_to_publish' AND EXISTS (SELECT 1 FROM jobs WHERE jobs.batch_id = release_batches.id AND jobs.kind = 'build') AND NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.batch_id = release_batches.id AND jobs.kind = 'build' AND NOT EXISTS (SELECT 1 FROM uploads WHERE uploads.source_job_id = jobs.id AND uploads.state = 'verified'))")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    for batch_id in batches {
        sqlx::query(
            "UPDATE release_batches SET state = 'artifacts_ready', updated_at = ? WHERE id = ?",
        )
        .bind(Utc::now())
        .bind(batch_id)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    }
    Ok(())
}

pub(crate) async fn lease_reverse_job(state: &AppState) -> Result<Option<JobSpec>, ApiError> {
    if publication_backpressure(&state.database).await? {
        return Ok(None);
    }
    if builder_has_active_job(&state.database).await? {
        return Ok(None);
    }
    let selected = sqlx::query(
        "SELECT id FROM jobs WHERE required_role = 'builder' AND status IN ('queued', 'no_eligible_worker') AND (next_attempt_at IS NULL OR next_attempt_at <= ?) ORDER BY priority DESC, created_at LIMIT 1",
    )
    .bind(Utc::now())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let job_id: String = selected.get("id");
    let spec = dispatch_job_to_builder(state, &job_id).await?;
    Ok(Some(spec))
}

async fn builder_has_active_job(database: &sqlx::SqlitePool) -> Result<bool, ApiError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE status IN ('dispatched', 'running', 'uncertain')",
    )
    .fetch_one(database)
    .await
    .map_err(ApiError::internal)?;
    Ok(count > 0)
}

pub(crate) async fn lease_reverse_transfer(
    state: &AppState,
) -> Result<Option<aursmith_protocol::BuilderUpload>, ApiError> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT request_json FROM uploads WHERE state = 'export_ready' AND expires_at > ? ORDER BY updated_at LIMIT 1",
    )
    .bind(Utc::now())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if let Some(value) = existing {
        return serde_json::from_str(&value)
            .map(Some)
            .map_err(ApiError::internal);
    }
    let issued = sqlx::query(
        "SELECT request_json FROM uploads WHERE state = 'issued' AND expires_at > ? ORDER BY updated_at LIMIT 1",
    )
    .bind(Utc::now())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(issued) = issued else {
        return Ok(None);
    };
    let value: String = issued.get("request_json");
    let upload: aursmith_protocol::BuilderUpload =
        serde_json::from_str(&value).map_err(ApiError::internal)?;
    transport::prepare_push_import(&state.config, &state.config.publisher_endpoint, &upload)
        .await?;
    sqlx::query("UPDATE uploads SET state = 'export_ready', updated_at = ? WHERE id = ? AND state = 'issued'")
        .bind(Utc::now()).bind(upload.id.to_string())
        .execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(Some(upload))
}

async fn dispatch_job_to_builder(state: &AppState, job_id: &str) -> Result<JobSpec, ApiError> {
    let job = sqlx::query(
        "SELECT required_role, revision_sha256, kind, source_manifest_sha256, dependency_snapshot_sha256, inputs_json, inline_inputs_json, expected_outputs_json, allow_check, limits_json FROM jobs WHERE id = ? AND status IN ('queued', 'no_eligible_worker')",
    )
    .bind(job_id)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::conflict("JOB_ALREADY_LEASED", "任务已被其他 Builder 领取"))?;
    let generation: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(generation), -1) + 1 FROM attempts WHERE job_id = ?",
    )
    .bind(job_id)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let parsed_job_id = Uuid::parse_str(job_id).map_err(ApiError::internal)?;
    let attempt_id = Uuid::new_v4();
    let attempt = AttemptRef {
        job_id: parsed_job_id,
        attempt_id,
        generation: u32::try_from(generation).map_err(ApiError::internal)?,
    };
    let now = Utc::now();
    let limits: ResourceLimits = serde_json::from_str(
        job.get::<Option<String>, _>("limits_json")
            .as_deref()
            .unwrap_or(
                "{\"cpu_count\":1,\"memory_mib\":1024,\"disk_mib\":4096,\"timeout_seconds\":600}",
            ),
    )
    .map_err(ApiError::internal)?;
    let spec = JobSpec {
        job_id: parsed_job_id,
        attempt: attempt.clone(),
        kind: parse_job_kind(job.get("kind"))?,
        revision_sha256: job
            .get::<Option<String>, _>("revision_sha256")
            .unwrap_or_else(|| "0".repeat(64)),
        source_manifest_sha256: job.get("source_manifest_sha256"),
        dependency_snapshot_sha256: job.get("dependency_snapshot_sha256"),
        dependency_attempt_ids: load_batch_dependency_attempts(state, job_id).await?,
        dependencies: load_job_dependencies(state, job_id).await?,
        inputs: serde_json::from_str(job.get("inputs_json")).map_err(ApiError::internal)?,
        inline_inputs: serde_json::from_str(job.get("inline_inputs_json"))
            .map_err(ApiError::internal)?,
        expected_outputs: serde_json::from_str(job.get("expected_outputs_json"))
            .map_err(ApiError::internal)?,
        allow_check: job.get::<i64, _>("allow_check") != 0,
        limits,
        issued_at: now,
        expires_at: now + Duration::minutes(10),
    };
    let spec_json = serde_json::to_string(&spec).map_err(ApiError::internal)?;
    let spec_sha256 = hex::encode(Sha256::digest(spec_json.as_bytes()));
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO attempts(id, job_id, generation, token_sha256, status) VALUES (?, ?, ?, ?, 'dispatched')")
        .bind(attempt_id.to_string())
        .bind(job_id)
        .bind(generation)
        .bind(spec_sha256)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    let updated = sqlx::query("UPDATE jobs SET worker_id = NULL, status = 'dispatched', failure_code = NULL, next_attempt_at = NULL, signed_spec_json = ?, updated_at = ? WHERE id = ? AND status IN ('queued', 'no_eligible_worker')")
        .bind(spec_json)
        .bind(now)
        .bind(job_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "JOB_ALREADY_LEASED",
            "任务已被其他 Builder 领取",
        ));
    }
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(spec)
}

async fn publication_backpressure(database: &sqlx::SqlitePool) -> Result<bool, ApiError> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value_json FROM system_settings WHERE key = 'publication_backpressure'",
    )
    .fetch_optional(database)
    .await
    .map_err(ApiError::internal)?;
    Ok(value
        .and_then(|value| serde_json::from_str::<bool>(&value).ok())
        .unwrap_or(false))
}

async fn reconcile_one(state: &AppState) -> Result<(), ApiError> {
    let row = sqlx::query(
        "SELECT jobs.id, jobs.kind, jobs.revision_id, jobs.revision_sha256, jobs.batch_id, revisions.upstream_version, builder_reports.response_json FROM jobs LEFT JOIN revisions ON revisions.id = jobs.revision_id LEFT JOIN builder_reports ON builder_reports.job_id = jobs.id WHERE jobs.status IN ('uncertain', 'dispatched', 'running') ORDER BY jobs.updated_at LIMIT 1",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(row) = row else { return Ok(()) };
    let job_id: String = row.get("id");
    let Some(reply) = row
        .get::<Option<String>, _>("response_json")
        .map(|value| serde_json::from_str::<transport::WorkerReply>(&value))
        .transpose()
        .map_err(ApiError::internal)?
    else {
        return Ok(());
    };
    let remote_status = reply.data["status"].as_str().unwrap_or("unknown");
    let (status, failure) = match remote_status {
        "queued" => ("dispatched", None),
        "running" => ("running", None),
        "succeeded" => ("succeeded", None),
        "failed" => (
            "failed",
            reply.data["failure_code"]
                .as_str()
                .or(Some("BUILDER_FAILED")),
        ),
        "cancelled" => ("cancelled", None),
        _ => return Ok(()),
    };
    let attempt_id = reply.data["attempt_id"].as_str().unwrap_or_default();
    let generation = reply.data["generation"].as_i64().unwrap_or(-1);
    let accepted: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM attempts WHERE id = ? AND job_id = ? AND generation = ?",
    )
    .bind(attempt_id)
    .bind(&job_id)
    .bind(generation)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if accepted == 0 {
        tracing::warn!(
            job_id,
            attempt_id,
            generation,
            "拒绝迟到或未知 Attempt 结果"
        );
        return Ok(());
    }
    let guest_result = if status == "succeeded" {
        let raw = reply.data["guest_result_json"]
            .as_str()
            .ok_or_else(|| ApiError::internal("Worker 成功结果缺少 GuestResult"))?;
        let expected_sha256 = reply.data["result_sha256"]
            .as_str()
            .ok_or_else(|| ApiError::internal("Worker 成功结果缺少摘要"))?;
        if hex::encode(Sha256::digest(raw.as_bytes())) != expected_sha256 {
            return Err(ApiError::conflict(
                "RESULT_DIGEST_MISMATCH",
                "Worker 返回的 GuestResult 与 Journal 摘要不一致",
            ));
        }
        Some(serde_json::from_str::<GuestResult>(raw).map_err(ApiError::internal)?)
    } else {
        None
    };
    let evidence_logs = if matches!(status, "succeeded" | "failed") {
        validate_evidence_logs(&reply.data["evidence_logs"])?
    } else {
        json!([])
    };
    let job_kind = row.get::<String, _>("kind");
    let evidence_files =
        validate_evidence_files(&reply.data["evidence_files"], &job_kind, status, attempt_id)?;
    let mut advance_build_batch = false;
    let retry_scheduled =
        status == "failed" && generation < 2 && failure.is_some_and(infrastructure_failure);
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE attempts SET status = ?, result_sha256 = ? WHERE id = ?")
        .bind(status)
        .bind(reply.data["result_sha256"].as_str())
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    let next_attempt_at = retry_scheduled
        .then(|| Utc::now() + Duration::seconds(if generation == 0 { 5 } else { 10 }));
    sqlx::query("UPDATE jobs SET status = ?, failure_code = ?, next_attempt_at = ?, updated_at = ? WHERE id = ?")
        .bind(if retry_scheduled { "queued" } else { status })
        .bind(failure)
        .bind(next_attempt_at)
        .bind(Utc::now())
        .bind(&job_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("DELETE FROM builder_reports WHERE job_id = ?")
        .bind(&job_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    if status == "succeeded"
        || evidence_logs
            .as_array()
            .is_some_and(|logs| !logs.is_empty())
    {
        let document = json!({
            "schema_version": 1,
            "status": status,
            "failure_code": failure,
            "guest_result": guest_result,
            "logs": evidence_logs
        });
        let bytes = serde_json::to_vec(&document).map_err(ApiError::internal)?;
        sqlx::query("INSERT OR REPLACE INTO job_evidence(job_id, kind, document_json, sha256, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&job_id).bind(row.get::<String, _>("kind")).bind(document.to_string())
            .bind(hex::encode(Sha256::digest(bytes))).bind(Utc::now())
            .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    if status == "succeeded" && job_kind == "build" {
        for entry in &evidence_files {
            sqlx::query("INSERT OR REPLACE INTO job_evidence_files(job_id, path, sha256, size, created_at) VALUES (?, ?, ?, ?, ?)")
                .bind(&job_id).bind(&entry.path).bind(&entry.sha256)
                .bind(i64::try_from(entry.size).map_err(ApiError::internal)?)
                .bind(Utc::now()).execute(&mut *transaction).await.map_err(ApiError::internal)?;
        }
    }
    if row.get::<String, _>("kind") == "build" && status == "succeeded" {
        let Some(GuestResult::Build(build_result)) = guest_result.as_ref() else {
            return Err(ApiError::conflict(
                "RESULT_KIND_MISMATCH",
                "Build Job 返回了其他类型的 GuestResult",
            ));
        };
        let expected_revision: Option<String> = row.get("revision_sha256");
        if build_result.job_id.to_string() != job_id
            || build_result.attempt.attempt_id.to_string() != attempt_id
            || expected_revision.as_deref() != Some(build_result.revision_sha256.as_str())
        {
            return Err(ApiError::conflict(
                "RESULT_IDENTITY_MISMATCH",
                "BuildResult 身份与 Controller 的 Job/Attempt/Revision 不一致",
            ));
        }
        let actual_version = accepted_build_version(&build_result.artifacts)?;
        for artifact in &build_result.artifacts {
            sqlx::query("INSERT INTO artifacts(sha256, job_id, path, size, package_name, package_version, architecture, provenance_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(sha256) DO NOTHING")
                .bind(&artifact.sha256).bind(&job_id).bind(&artifact.path)
                .bind(i64::try_from(artifact.size).map_err(ApiError::internal)?)
                .bind(&artifact.package_name).bind(&artifact.package_version).bind(&artifact.architecture)
                .bind(serde_json::to_string(&build_result.provenance).map_err(ApiError::internal)?)
                .bind(Utc::now()).execute(&mut *transaction).await.map_err(ApiError::internal)?;
        }
        if let Some(revision_id) = row.get::<Option<String>, _>("revision_id") {
            sqlx::query("UPDATE revisions SET state = 'built', upstream_version = ?, published_version = ? WHERE id = ?")
                .bind(&actual_version)
                .bind(&actual_version)
                .bind(revision_id)
                .execute(&mut *transaction)
                .await
                .map_err(ApiError::internal)?;
        }
        advance_build_batch = true;
    } else if row.get::<String, _>("kind") == "build"
        && status == "failed"
        && !retry_scheduled
        && let Some(batch_id) = row.get::<Option<String>, _>("batch_id")
    {
        sqlx::query("UPDATE release_batches SET state = 'build_failed', failure_reason = ?, updated_at = ? WHERE id = ?")
            .bind(failure).bind(Utc::now()).bind(batch_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    transaction.commit().await.map_err(ApiError::internal)?;
    if advance_build_batch {
        crate::packages::schedule_ready_builds(&state.database).await?;
    }
    Ok(())
}

fn accepted_build_version(
    artifacts: &[aursmith_protocol::ArtifactRecord],
) -> Result<String, ApiError> {
    let Some(actual_version) = artifacts
        .first()
        .and_then(|artifact| artifact.package_version.as_deref())
    else {
        return Err(ApiError::conflict(
            "PUBLISHED_VERSION_MISSING",
            "makepkg 未返回可发布的软件包版本",
        ));
    };
    if artifacts
        .iter()
        .any(|artifact| artifact.package_version.as_deref() != Some(actual_version))
    {
        return Err(ApiError::conflict(
            "PUBLISHED_VERSION_INCONSISTENT",
            "同一次 makepkg 构建返回了不一致的软件包版本",
        ));
    }
    Ok(actual_version.to_owned())
}

fn infrastructure_failure(code: &str) -> bool {
    matches!(
        code,
        "DOCKER_TIMEOUT"
            | "DOCKER_DAEMON_UNAVAILABLE"
            | "DOCKER_PULL_FAILED"
            | "DOCKER_NETWORK_TIMEOUT"
            | "BUILD_NETWORK_TRANSIENT"
            | "RESULT_UNAVAILABLE"
            | "WORKER_RESTARTED"
            | "WORKER_UNREACHABLE"
    )
}

fn validate_evidence_logs(value: &serde_json::Value) -> Result<serde_json::Value, ApiError> {
    const ALLOWED_PATHS: [&str; 4] = [
        "docker.stdout.log",
        "docker.stderr.log",
        "output/build.log",
        "output/guest-error.json",
    ];
    let logs = value
        .as_array()
        .ok_or_else(|| ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志证据不是数组"))?;
    if logs.len() > ALLOWED_PATHS.len()
        || serde_json::to_vec(value).map_err(ApiError::internal)?.len() > 1024 * 1024
    {
        return Err(ApiError::conflict(
            "INVALID_EVIDENCE_LOGS",
            "Worker 日志证据超过数量或 1 MiB 上限",
        ));
    }
    let mut seen = BTreeSet::new();
    for log in logs {
        let path = log["path"]
            .as_str()
            .ok_or_else(|| ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志缺少路径"))?;
        if !ALLOWED_PATHS.contains(&path) || !seen.insert(path) {
            return Err(ApiError::conflict(
                "INVALID_EVIDENCE_LOGS",
                "Worker 日志路径不允许或重复",
            ));
        }
        let size = log["size"]
            .as_u64()
            .ok_or_else(|| ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志大小无效"))?;
        if size > 64 * 1024 * 1024 {
            if !log["sha256"].is_null()
                || log["omitted_reason"].as_str().is_none()
                || log["truncated"].as_bool() != Some(true)
            {
                return Err(ApiError::conflict(
                    "INVALID_EVIDENCE_LOGS",
                    "超大日志必须明确省略原因",
                ));
            }
            continue;
        }
        let sha256 = log["sha256"]
            .as_str()
            .ok_or_else(|| ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志缺少摘要"))?;
        if sha256.len() != 64
            || !sha256
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(ApiError::conflict(
                "INVALID_EVIDENCE_LOGS",
                "Worker 日志摘要无效",
            ));
        }
        let content = log["content_base64"].as_str().ok_or_else(|| {
            ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志缺少有界内容")
        })?;
        let decoded = BASE64.decode(content).map_err(|_| {
            ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志内容不是 Base64")
        })?;
        let truncated = log["truncated"].as_bool().ok_or_else(|| {
            ApiError::conflict("INVALID_EVIDENCE_LOGS", "Worker 日志缺少截断标记")
        })?;
        let content_shape_valid = if size <= 128 * 1024 {
            decoded.len() as u64 == size && !truncated
        } else {
            decoded.len() == 128 * 1024 && truncated
        };
        if !content_shape_valid {
            return Err(ApiError::conflict(
                "INVALID_EVIDENCE_LOGS",
                "Worker 日志内容长度与截断标记不一致",
            ));
        }
        if let Some(text) = log["content_utf8"].as_str()
            && text.as_bytes() != decoded
        {
            return Err(ApiError::conflict(
                "INVALID_EVIDENCE_LOGS",
                "Worker 日志文本与 Base64 内容不一致",
            ));
        }
        if size <= 128 * 1024 && hex::encode(Sha256::digest(&decoded)) != sha256 {
            return Err(ApiError::conflict(
                "INVALID_EVIDENCE_LOGS",
                "完整 Worker 日志内容与摘要不一致",
            ));
        }
    }
    Ok(value.clone())
}

fn validate_evidence_files(
    value: &serde_json::Value,
    job_kind: &str,
    status: &str,
    _attempt_id: &str,
) -> Result<Vec<aursmith_protocol::ManifestEntry>, ApiError> {
    let entries = serde_json::from_value::<Vec<aursmith_protocol::ManifestEntry>>(value.clone())
        .map_err(|_| ApiError::conflict("INVALID_EVIDENCE_FILES", "Worker 证据文件清单无效"))?;
    if job_kind != "build" || status != "succeeded" {
        if entries.is_empty() {
            return Ok(entries);
        }
        return Err(ApiError::conflict(
            "UNEXPECTED_EVIDENCE_FILES",
            "只有成功 Build Job 可以返回证据文件",
        ));
    }
    let expected: [String; 0] = [];
    let mut paths = BTreeSet::new();
    for entry in &entries {
        aursmith_protocol::validate_relative_path(&entry.path).map_err(ApiError::internal)?;
        if !expected.contains(&entry.path)
            || !paths.insert(entry.path.clone())
            || entry.sha256.len() != 64
            || !entry.sha256.chars().all(|value| value.is_ascii_hexdigit())
            || entry.size == 0
        {
            return Err(ApiError::conflict(
                "INVALID_EVIDENCE_FILES",
                "Build 证据文件路径、摘要或大小无效",
            ));
        }
    }
    Ok(entries)
}

fn parse_job_kind(value: &str) -> Result<JobKind, ApiError> {
    match value {
        "build" => Ok(JobKind::Build),
        _ => Err(ApiError::internal("数据库包含未知 Job 类型")),
    }
}

async fn load_job_dependencies(
    state: &AppState,
    job_id: &str,
) -> Result<Vec<DependencyInput>, ApiError> {
    let rows = sqlx::query("SELECT revision_dependencies.dependency_name, revision_dependencies.dependency_kind, revision_dependencies.target_package_base, package_bases.outputs_json FROM jobs JOIN revisions ON revisions.id = jobs.revision_id JOIN package_bases ON package_bases.name = revisions.package_base JOIN revision_dependencies ON revision_dependencies.revision_id = jobs.revision_id WHERE jobs.id = ? ORDER BY revision_dependencies.dependency_name, revision_dependencies.dependency_kind")
        .bind(job_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let name: String = row.get("dependency_name");
            let outputs: BTreeSet<String> =
                serde_json::from_str(row.get("outputs_json")).unwrap_or_default();
            DependencyInput {
                source: dependency_source(
                    &name,
                    row.get::<Option<String>, _>("target_package_base")
                        .as_deref(),
                    &outputs,
                ),
                name,
                kind: row.get("dependency_kind"),
            }
        })
        .collect())
}

fn dependency_source(
    dependency_name: &str,
    target_package_base: Option<&str>,
    current_outputs: &BTreeSet<String>,
) -> DependencySource {
    if target_package_base.is_some() || current_outputs.contains(dependency_name) {
        DependencySource::AurBatch
    } else {
        DependencySource::Official
    }
}

async fn load_batch_dependency_attempts(
    state: &AppState,
    job_id: &str,
) -> Result<Vec<Uuid>, ApiError> {
    let attempts: Vec<String> = sqlx::query_scalar("SELECT DISTINCT attempts.id FROM jobs AS current_job JOIN revision_dependencies ON revision_dependencies.revision_id = current_job.revision_id JOIN revisions AS dependency_revision ON dependency_revision.package_base = revision_dependencies.target_package_base JOIN jobs AS dependency_job ON dependency_job.batch_id = current_job.batch_id AND dependency_job.revision_id = dependency_revision.id AND dependency_job.kind = 'build' AND dependency_job.status = 'succeeded' JOIN attempts ON attempts.job_id = dependency_job.id AND attempts.status = 'succeeded' WHERE current_job.id = ? ORDER BY attempts.id")
        .bind(job_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    attempts
        .into_iter()
        .map(|value| Uuid::parse_str(&value).map_err(ApiError::internal))
        .collect()
}

#[cfg(test)]
mod release_tests {
    use super::*;

    #[tokio::test]
    async fn reverse_worker_does_not_lease_while_an_attempt_is_active() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let worker_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let now = Utc::now();
        sqlx::query("INSERT INTO workers(id, name, role, state, endpoint, ssh_host_key_sha256, protocol_version, labels_json, connection_mode, created_at, updated_at) VALUES (?, 'builder', 'builder', 'online', '', '', 1, '[]', 'reverse', ?, ?)")
            .bind(worker_id.to_string()).bind(now).bind(now).execute(&database).await.unwrap();
        sqlx::query("INSERT INTO jobs(id, required_role, worker_id, status, priority, kind, inputs_json, inline_inputs_json, required_labels_json, created_at, updated_at) VALUES (?, 'builder', ?, 'dispatched', 1, 'build', '[]', '[]', '[]', ?, ?)")
            .bind(job_id.to_string()).bind(worker_id.to_string()).bind(now).bind(now).execute(&database).await.unwrap();

        assert!(builder_has_active_job(&database).await.unwrap());

        sqlx::query("UPDATE jobs SET status = 'succeeded' WHERE id = ?")
            .bind(job_id.to_string())
            .execute(&database)
            .await
            .unwrap();
        assert!(!builder_has_active_job(&database).await.unwrap());
    }

    #[test]
    fn split_output_dependency_is_not_downloaded_from_official_repositories() {
        let outputs = BTreeSet::from(["demo-cli".to_string(), "demo-lib".to_string()]);

        assert_eq!(
            dependency_source("demo-lib", None, &outputs),
            DependencySource::AurBatch
        );
        assert_eq!(
            dependency_source("glibc", None, &outputs),
            DependencySource::Official
        );
        assert_eq!(
            dependency_source("aur-dependency", Some("aur-dependency"), &outputs),
            DependencySource::AurBatch
        );
    }

    fn state(database: sqlx::SqlitePool) -> AppState {
        AppState::new(
            database,
            crate::config::Config {
                bind_address: "127.0.0.1:0".into(),
                database_url: "sqlite::memory:".into(),
                public_origin: "https://aursmith.test".into(),
                ssh_identity_source_file: "/不存在".into(),
                ssh_identity_file: "/不存在".into(),
                ssh_known_hosts_file: "/不存在".into(),
                session_idle_minutes: 30,
                session_absolute_hours: 1,
                low_agent_endpoints: vec![],
                high_agent_endpoint: String::new(),
                repository_name: "aursmith".into(),
                source_git_commit: "test".into(),
                repository_base_url: "https://repo.test".into(),
                builder_token_sha256: crate::auth::sha256("test-builder-token"),
                publisher_endpoint: "ssh://publisher.test:22".into(),
            },
        )
    }

    fn artifact(name: &str, version: &str) -> ArtifactRecord {
        ArtifactRecord {
            path: format!("{name}-{version}-x86_64.pkg.tar.zst"),
            sha256: format!("{version:0>64}"),
            size: 1,
            package_name: Some(name.into()),
            package_version: Some(version.into()),
            architecture: Some("x86_64".into()),
        }
    }

    #[test]
    fn complete_release_keeps_unchanged_packages_and_replaces_changed_package() {
        let merged = merge_release_artifacts(
            vec![artifact("alpha", "1"), artifact("beta", "1")],
            vec![artifact("alpha", "2")],
        );
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].package_name.as_deref(), Some("alpha"));
        assert_eq!(merged[0].package_version.as_deref(), Some("2"));
        assert_eq!(merged[1].package_name.as_deref(), Some("beta"));
    }

    #[test]
    fn removal_drops_all_selected_split_outputs_and_can_leave_empty_repository() {
        let remaining = remove_release_artifacts(
            vec![
                artifact("demo-cli", "1"),
                artifact("demo-lib", "1"),
                artifact("other", "1"),
            ],
            &BTreeSet::from(["demo-cli".into(), "demo-lib".into()]),
        );
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].package_name.as_deref(), Some("other"));
        assert!(
            remove_release_artifacts(
                vec![artifact("demo-cli", "1")],
                &BTreeSet::from(["demo-cli".into()])
            )
            .is_empty()
        );
    }

    #[tokio::test]
    async fn publication_backpressure_defaults_to_false_and_uses_persisted_value() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        assert!(!publication_backpressure(&database).await.unwrap());
        sqlx::query(
            "INSERT INTO system_settings(key, value_json, updated_at) VALUES ('publication_backpressure', 'true', ?)",
        )
        .bind(Utc::now())
        .execute(&database)
        .await
        .unwrap();
        assert!(publication_backpressure(&database).await.unwrap());
    }

    #[tokio::test]
    async fn new_release_uses_explicitly_rolled_back_release_as_base() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let old_batch = Uuid::new_v4().to_string();
        let new_batch = Uuid::new_v4().to_string();
        let old_release = Uuid::new_v4().to_string();
        let new_release = Uuid::new_v4().to_string();
        let now = Utc::now();
        for batch in [&old_batch, &new_batch] {
            sqlx::query("INSERT INTO release_batches(id, state, graph_json, created_at, updated_at) VALUES (?, 'published', '{}', ?, ?)")
                .bind(batch).bind(now).bind(now).execute(&database).await.unwrap();
        }
        sqlx::query("INSERT INTO releases(id, batch_id, state, manifest_sha256, source_git_commit, committed_at, created_at) VALUES (?, ?, 'committed', ?, 'test', ?, ?), (?, ?, 'committed', ?, 'test', ?, ?)")
            .bind(&old_release).bind(&old_batch).bind("a".repeat(64)).bind(now - Duration::hours(1)).bind(now - Duration::hours(1))
            .bind(&new_release).bind(&new_batch).bind("b".repeat(64)).bind(now).bind(now)
            .execute(&database).await.unwrap();
        assert_eq!(
            current_release_id(&database).await.unwrap(),
            Some(new_release)
        );
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('current_release_id', ?, ?)")
            .bind(json!(old_release).to_string()).bind(now).execute(&database).await.unwrap();
        assert_eq!(
            current_release_id(&database).await.unwrap(),
            Some(old_release)
        );
    }

    #[tokio::test]
    async fn queued_removal_creates_an_empty_release_plan() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let state = state(database.clone());
        let old_batch = Uuid::new_v4().to_string();
        let removal_batch = Uuid::new_v4().to_string();
        let old_release = Uuid::new_v4().to_string();
        let old_job = Uuid::new_v4().to_string();
        let revision = Uuid::new_v4().to_string();
        let now = Utc::now();
        sqlx::query("INSERT INTO package_bases(name, version, outputs_json, dependencies_json, optional_dependencies_json, provides_json, architectures_json, last_synced_at) VALUES ('demo', '1-1', ?, '[]', '[]', '[]', '[\"any\"]', ?)")
            .bind(json!(["demo-cli", "demo-lib"]).to_string()).bind(now).execute(&database).await.unwrap();
        sqlx::query("INSERT INTO revisions(id, package_base, aur_commit, upstream_version, input_sha256, audit_policy_version, state, metadata_json, created_at) VALUES (?, 'demo', ?, '1-1', ?, 'v1', 'published', ?, ?)")
            .bind(&revision).bind("b".repeat(40)).bind("c".repeat(64))
            .bind(json!({"outputs": ["demo-cli", "demo-lib"]}).to_string()).bind(now)
            .execute(&database).await.unwrap();
        sqlx::query("INSERT INTO release_batches(id, state, graph_json, created_at, updated_at) VALUES (?, 'published', '{}', ?, ?), (?, 'queued_removal', ?, ?, ?)")
            .bind(&old_batch).bind(now).bind(now).bind(&removal_batch)
            .bind(json!({"remove": ["demo"]}).to_string()).bind(now).bind(now)
            .execute(&database).await.unwrap();
        sqlx::query("INSERT INTO jobs(id, batch_id, revision_id, required_role, status, priority, revision_sha256, kind, inputs_json, inline_inputs_json, required_labels_json, created_at, updated_at) VALUES (?, ?, ?, 'builder', 'succeeded', 1, ?, 'build', '[]', '[]', '[]', ?, ?)")
            .bind(&old_job).bind(&old_batch).bind(&revision).bind("c".repeat(64)).bind(now).bind(now)
            .execute(&database).await.unwrap();
        sqlx::query("INSERT INTO releases(id, batch_id, state, manifest_sha256, source_git_commit, committed_at, created_at) VALUES (?, ?, 'committed', ?, 'test', ?, ?)")
            .bind(&old_release).bind(&old_batch).bind("d".repeat(64)).bind(now).bind(now)
            .execute(&database).await.unwrap();
        for name in ["demo-cli", "demo-lib"] {
            let digest = hex::encode(Sha256::digest(name.as_bytes()));
            sqlx::query("INSERT INTO artifacts(sha256, job_id, path, size, package_name, package_version, architecture, provenance_json, created_at) VALUES (?, ?, ?, 1, ?, '1-1', 'any', '{}', ?)")
                .bind(&digest).bind(&old_job).bind(format!("{name}-1-1-any.pkg.tar.zst")).bind(name).bind(now)
                .execute(&database).await.unwrap();
            sqlx::query("INSERT INTO release_artifacts(release_id, artifact_sha256) VALUES (?, ?)")
                .bind(&old_release)
                .bind(digest)
                .execute(&database)
                .await
                .unwrap();
        }
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('current_release_id', ?, ?)")
            .bind(json!(old_release).to_string()).bind(now).execute(&database).await.unwrap();

        dispatch_release_one(&state).await.unwrap();

        let plan_json: String = sqlx::query_scalar("SELECT plan_json FROM release_jobs")
            .fetch_one(&database)
            .await
            .unwrap();
        let authorization: ReleasePlan = serde_json::from_str(&plan_json).unwrap();
        assert!(authorization.artifacts.is_empty());
        assert_eq!(
            authorization.removed_package_names,
            ["demo-cli", "demo-lib"]
        );
    }

    #[test]
    fn only_infrastructure_failures_are_automatically_retried() {
        assert!(infrastructure_failure("DOCKER_TIMEOUT"));
        assert!(infrastructure_failure("DOCKER_DAEMON_UNAVAILABLE"));
        assert!(infrastructure_failure("DOCKER_PULL_FAILED"));
        assert!(infrastructure_failure("DOCKER_NETWORK_TIMEOUT"));
        assert!(infrastructure_failure("BUILD_NETWORK_TRANSIENT"));
        assert!(infrastructure_failure("WORKER_RESTARTED"));
        assert!(!infrastructure_failure("DOCKER_UNAUTHORIZED"));
        assert!(!infrastructure_failure("DOCKER_MANIFEST_INVALID"));
        assert!(!infrastructure_failure("DOCKER_PERMISSION_DENIED"));
        assert!(!infrastructure_failure("DOCKER_DISK_FULL"));
        assert!(!infrastructure_failure("INPUT_INVALID"));
        assert!(!infrastructure_failure("AUDIT_REJECTED"));
        assert!(!infrastructure_failure("GUEST_BUILD_FAILED"));
        assert!(!infrastructure_failure("GUEST_CHECKSUM_FAILED"));
        assert!(!infrastructure_failure("GUEST_PGP_FAILED"));
        assert!(!infrastructure_failure("GUEST_OUTPUT_MISMATCH"));
    }

    #[test]
    fn passthrough_build_uses_the_version_reported_by_makepkg() {
        let artifacts = vec![artifact("subtitleedit", "5.1.0-2")];
        assert_eq!(accepted_build_version(&artifacts).unwrap(), "5.1.0-2");
    }

    #[test]
    fn split_outputs_must_report_one_upstream_version() {
        let artifacts = vec![artifact("demo", "1.0-1"), artifact("demo-doc", "1.0-2")];
        let error = accepted_build_version(&artifacts).unwrap_err();
        assert_eq!(error.code, "PUBLISHED_VERSION_INCONSISTENT");
    }

    #[test]
    fn evidence_logs_require_allowed_paths_and_matching_complete_digest() {
        let content = b"build output";
        let valid = json!([{
            "path": "output/build.log",
            "size": content.len(),
            "sha256": hex::encode(Sha256::digest(content)),
            "truncated": false,
            "content_base64": BASE64.encode(content)
        }]);
        assert!(validate_evidence_logs(&valid).is_ok());
        let mut invalid = valid;
        invalid[0]["path"] = json!("../controller-secret");
        assert!(validate_evidence_logs(&invalid).is_err());
    }
}
