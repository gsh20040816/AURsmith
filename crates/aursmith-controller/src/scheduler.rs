use crate::{error::ApiError, routes::AppState, transport};
use aursmith_domain::AttemptRef;
use aursmith_protocol::{
    ArtifactRecord, DependencyInput, DependencySource, GuestResult, JobKind, JobSpec,
    ReleaseAuthorization, ResourceLimits, SignedEnvelope,
};
use chrono::{Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::{BTreeMap, BTreeSet};
use tokio::time::{MissedTickBehavior, interval};
use uuid::Uuid;

pub fn spawn(state: AppState) {
    let heartbeat_state = state.clone();
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(30));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = probe_all_workers(&heartbeat_state).await {
                tracing::warn!(%error, "Worker 心跳轮询失败");
            }
        }
    });
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
        let mut timer = interval(std::time::Duration::from_secs(3));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = crate::audits::dispatch_one(&audit_state).await {
                tracing::warn!(%error, "Agent 审计调度失败");
            }
        }
    });
    let profile_state = state.clone();
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(6 * 60 * 60));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) =
                crate::profiles::evaluate_dependencies(&profile_state.database).await
            {
                tracing::warn!(%error, "依赖 Profile 统计评估失败");
            }
        }
    });
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(2));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = dispatch_one(&state).await {
                tracing::warn!(%error, "任务调度失败");
            }
            if let Err(error) = reconcile_one(&state).await {
                tracing::warn!(%error, "不确定任务对账失败");
            }
            if let Err(error) = dispatch_transfer_one(&state).await {
                tracing::warn!(%error, "Artifact 传输调度失败");
            }
            if let Err(error) = dispatch_release_one(&state).await {
                tracing::warn!(%error, "Release 发布调度失败");
            }
            if let Err(error) = dispatch_archive_one(&state).await {
                tracing::warn!(%error, "Release 归档调度失败");
            }
        }
    });
}

async fn dispatch_archive_one(state: &AppState) -> Result<(), ApiError> {
    let cleanup = sqlx::query("SELECT archive_transfers.id, archive_transfers.envelope_json, workers.endpoint FROM archive_transfers JOIN workers ON workers.id = archive_transfers.publisher_worker_id WHERE archive_transfers.state = 'verified' AND archive_transfers.export_cleaned_at IS NULL ORDER BY archive_transfers.updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = cleanup {
        let envelope: SignedEnvelope =
            serde_json::from_str(row.get("envelope_json")).map_err(ApiError::internal)?;
        transport::complete_export(&state.config, row.get("endpoint"), &envelope).await?;
        sqlx::query(
            "UPDATE archive_transfers SET export_cleaned_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(Utc::now())
        .bind(Utc::now())
        .bind(row.get::<String, _>("id"))
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
        return Ok(());
    }
    let expired = sqlx::query("SELECT id, archive_copy_id FROM archive_transfers WHERE state IN ('issued', 'export_ready') AND expires_at <= ?")
        .bind(Utc::now()).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    for row in expired {
        let transfer_id: String = row.get("id");
        let copy_id: String = row.get("archive_copy_id");
        sqlx::query("UPDATE archive_transfers SET state = 'expired', last_error = 'CAPABILITY_EXPIRED', updated_at = ? WHERE id = ?")
            .bind(Utc::now()).bind(&transfer_id).execute(&state.database).await.map_err(ApiError::internal)?;
        sqlx::query("UPDATE archive_copies SET state = 'failed', last_error = 'CAPABILITY_EXPIRED', updated_at = ? WHERE id = ?")
            .bind(Utc::now()).bind(copy_id).execute(&state.database).await.map_err(ApiError::internal)?;
    }
    let pending = sqlx::query("SELECT archive_transfers.id, archive_transfers.archive_copy_id, archive_transfers.state, archive_transfers.envelope_json, archive_transfers.attempt_count, publisher.endpoint AS publisher_endpoint, archiver.endpoint AS archiver_endpoint, archiver.identity_signing_key_hex FROM archive_transfers JOIN workers AS publisher ON publisher.id = archive_transfers.publisher_worker_id JOIN workers AS archiver ON archiver.id = archive_transfers.archiver_worker_id WHERE archive_transfers.state IN ('issued', 'export_ready') AND archive_transfers.expires_at > ? ORDER BY archive_transfers.updated_at LIMIT 1")
        .bind(Utc::now()).fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = pending {
        let transfer_id: String = row.get("id");
        let copy_id: String = row.get("archive_copy_id");
        let envelope: SignedEnvelope =
            serde_json::from_str(row.get("envelope_json")).map_err(ApiError::internal)?;
        if row.get::<String, _>("state") == "issued" {
            match transport::authorize_export(
                &state.config,
                row.get("publisher_endpoint"),
                &envelope,
            )
            .await
            {
                Ok(_) => {
                    let mut transaction =
                        state.database.begin().await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE archive_transfers SET state = 'export_ready', last_error = NULL, updated_at = ? WHERE id = ?")
                        .bind(Utc::now()).bind(transfer_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE archive_copies SET state = 'transferring', updated_at = ? WHERE id = ?")
                        .bind(Utc::now()).bind(copy_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    transaction.commit().await.map_err(ApiError::internal)?;
                }
                Err(error) => {
                    record_archive_failure(
                        state,
                        &transfer_id,
                        &copy_id,
                        row.get("attempt_count"),
                        &error.to_string(),
                    )
                    .await?
                }
            }
        } else {
            match transport::authorize_import(
                &state.config,
                row.get("archiver_endpoint"),
                &envelope,
            )
            .await
            {
                Ok(reply) => {
                    let receipt_envelope: SignedEnvelope =
                        serde_json::from_value(reply.data["receipt"].clone())
                            .map_err(ApiError::internal)?;
                    let expected_key: String = row
                        .get::<Option<String>, _>("identity_signing_key_hex")
                        .ok_or_else(|| ApiError::internal("Archiver 缺少身份公钥"))?;
                    if receipt_envelope.verifying_key
                        != hex::decode(expected_key).map_err(ApiError::internal)?
                    {
                        return Err(ApiError::conflict(
                            "ARCHIVE_RECEIPT_UNTRUSTED",
                            "ArchiveReceipt 身份签名不匹配",
                        ));
                    }
                    let receipt: aursmith_protocol::ArchiveReceipt = receipt_envelope
                        .verify("aursmith.archive_receipt")
                        .map_err(ApiError::internal)?;
                    let capability: aursmith_protocol::TransferCapability = envelope
                        .verify("aursmith.transfer_capability")
                        .map_err(ApiError::internal)?;
                    let release_id = capability
                        .release_id
                        .ok_or_else(|| ApiError::internal("Archive Capability 缺少 Release"))?;
                    let expected_manifest: String =
                        sqlx::query_scalar("SELECT manifest_sha256 FROM releases WHERE id = ?")
                            .bind(release_id.to_string())
                            .fetch_one(&state.database)
                            .await
                            .map_err(ApiError::internal)?;
                    if receipt.release_id != release_id
                        || receipt.archive_worker != capability.destination_worker
                        || receipt.release_manifest_sha256 != expected_manifest
                        || receipt.files != capability.files
                        || receipt.state != aursmith_domain::ArchiveState::Verified
                    {
                        return Err(ApiError::conflict(
                            "ARCHIVE_RECEIPT_MISMATCH",
                            "ArchiveReceipt 与授权文件集合不一致",
                        ));
                    }
                    let mut transaction =
                        state.database.begin().await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE archive_transfers SET state = 'verified', last_error = NULL, updated_at = ? WHERE id = ?")
                        .bind(Utc::now()).bind(&transfer_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE archive_copies SET state = 'verified', receipt_sha256 = ?, last_error = NULL, updated_at = ? WHERE id = ?")
                        .bind(&receipt_envelope.payload_sha256).bind(Utc::now()).bind(&copy_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    transaction.commit().await.map_err(ApiError::internal)?;
                }
                Err(error) => {
                    record_archive_failure(
                        state,
                        &transfer_id,
                        &copy_id,
                        row.get("attempt_count"),
                        &error.to_string(),
                    )
                    .await?
                }
            }
        }
        return Ok(());
    }

    let release = sqlx::query("SELECT releases.id, releases.manifest_sha256, releases.writer_epoch, release_authorizations.publisher_worker_id, workers.endpoint FROM releases JOIN release_authorizations ON release_authorizations.release_id = releases.id JOIN workers ON workers.id = release_authorizations.publisher_worker_id WHERE releases.state = 'committed' AND NOT EXISTS (SELECT 1 FROM archive_copies WHERE archive_copies.release_id = releases.id) ORDER BY releases.committed_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(release) = release else {
        return Ok(());
    };
    let archiver = sqlx::query(
        "SELECT id FROM workers WHERE role = 'archiver' AND state = 'online' ORDER BY name LIMIT 1",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(archiver) = archiver else {
        return Ok(());
    };
    let release_id: String = release.get("id");
    let file_reply =
        transport::release_files(&state.config, release.get("endpoint"), &release_id).await?;
    if file_reply.data["release_manifest_sha256"].as_str()
        != Some(release.get::<String, _>("manifest_sha256").as_str())
    {
        return Err(ApiError::conflict(
            "RELEASE_MANIFEST_MISMATCH",
            "Publisher Release 与 Controller 摘要不一致",
        ));
    }
    let files: Vec<aursmith_protocol::ManifestEntry> =
        serde_json::from_value(file_reply.data["files"].clone()).map_err(ApiError::internal)?;
    if files.is_empty() {
        return Err(ApiError::conflict(
            "RELEASE_EMPTY",
            "Publisher Release 文件集合为空",
        ));
    }
    let transfer_id = Uuid::new_v4();
    let copy_id = Uuid::new_v4();
    let now = Utc::now();
    let capability = aursmith_protocol::TransferCapability {
        id: transfer_id,
        source_worker: Uuid::parse_str(release.get("publisher_worker_id"))
            .map_err(ApiError::internal)?,
        destination_worker: Uuid::parse_str(archiver.get("id")).map_err(ApiError::internal)?,
        attempt: None,
        release_id: Some(Uuid::parse_str(&release_id).map_err(ApiError::internal)?),
        writer_epoch: u64::try_from(release.get::<i64, _>("writer_epoch"))
            .map_err(ApiError::internal)?,
        files,
        expires_at: now + Duration::hours(1),
    };
    let envelope = SignedEnvelope::sign(
        "aursmith.transfer_capability",
        &capability,
        &state.signing_key,
    )
    .map_err(ApiError::internal)?;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO archive_copies(id, release_id, archiver_worker_id, state, created_at, updated_at) VALUES (?, ?, ?, 'pending', ?, ?)")
        .bind(copy_id.to_string()).bind(&release_id).bind(archiver.get::<String,_>("id")).bind(now).bind(now)
        .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO archive_transfers(id, archive_copy_id, publisher_worker_id, archiver_worker_id, state, envelope_json, expires_at, created_at, updated_at) VALUES (?, ?, ?, ?, 'issued', ?, ?, ?, ?)")
        .bind(transfer_id.to_string()).bind(copy_id.to_string()).bind(release.get::<String,_>("publisher_worker_id"))
        .bind(archiver.get::<String,_>("id")).bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
        .bind(capability.expires_at).bind(now).bind(now).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok(())
}

async fn record_archive_failure(
    state: &AppState,
    transfer_id: &str,
    copy_id: &str,
    attempts: i64,
    error: &str,
) -> Result<(), ApiError> {
    let terminal = attempts + 1 >= 3;
    sqlx::query("UPDATE archive_transfers SET state = CASE WHEN ? THEN 'failed' ELSE state END, attempt_count = attempt_count + 1, last_error = ?, updated_at = ? WHERE id = ?")
        .bind(terminal).bind(error).bind(Utc::now()).bind(transfer_id).execute(&state.database).await.map_err(ApiError::internal)?;
    if terminal {
        sqlx::query("UPDATE archive_copies SET state = 'failed', last_error = ?, updated_at = ? WHERE id = ?")
            .bind(error).bind(Utc::now()).bind(copy_id).execute(&state.database).await.map_err(ApiError::internal)?;
        let fingerprint = format!("archive-copy-failed:{copy_id}");
        sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'warning', 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = CASE WHEN alerts.state = 'resolved' THEN 'open' ELSE alerts.state END, details_json = excluded.details_json, resolved_at = NULL")
            .bind(Uuid::new_v4().to_string()).bind(fingerprint).bind("Release 归档失败")
            .bind(json!({"archive_copy_id": copy_id, "error": error}).to_string()).bind(Utc::now())
            .execute(&state.database).await.map_err(ApiError::internal)?;
    }
    Ok(())
}

async fn dispatch_release_one(state: &AppState) -> Result<(), ApiError> {
    let pending = sqlx::query("SELECT release_authorizations.release_id, release_authorizations.state, release_authorizations.envelope_json, release_authorizations.attempt_count, workers.endpoint FROM release_authorizations JOIN workers ON workers.id = release_authorizations.publisher_worker_id WHERE release_authorizations.state IN ('issued', 'awaiting_signer') ORDER BY release_authorizations.updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = pending {
        let release_id: String = row.get("release_id");
        let authorization_state: String = row.get("state");
        let endpoint: String = row.get("endpoint");
        if authorization_state == "issued" {
            let envelope: SignedEnvelope =
                serde_json::from_str(row.get("envelope_json")).map_err(ApiError::internal)?;
            match transport::authorize_release(&state.config, &endpoint, &envelope).await {
                Ok(_) => {
                    sqlx::query("UPDATE release_authorizations SET state = 'awaiting_signer', last_error = NULL, updated_at = ? WHERE release_id = ?")
                        .bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
                }
                Err(error) => {
                    let attempts: i64 = row.get("attempt_count");
                    let terminal = attempts + 1 >= 3;
                    sqlx::query("UPDATE release_authorizations SET state = CASE WHEN ? THEN 'failed' ELSE state END, attempt_count = attempt_count + 1, last_error = ?, updated_at = ? WHERE release_id = ?")
                        .bind(terminal).bind(error.to_string()).bind(Utc::now()).bind(&release_id)
                        .execute(&state.database).await.map_err(ApiError::internal)?;
                    if terminal {
                        fail_release(state, &release_id, &error.to_string()).await?;
                    }
                }
            }
        } else {
            match transport::query_release(&state.config, &endpoint, &release_id).await {
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
                    sqlx::query("UPDATE release_authorizations SET state = 'published', last_error = NULL, updated_at = ? WHERE release_id = ?")
                        .bind(Utc::now()).bind(&release_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
                    sqlx::query("UPDATE release_batches SET state = 'published', current_release_id = ?, failure_reason = NULL, updated_at = ? WHERE id = (SELECT batch_id FROM releases WHERE id = ?)")
                        .bind(&release_id).bind(Utc::now()).bind(&release_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
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
                    sqlx::query("UPDATE release_authorizations SET last_error = ?, updated_at = ? WHERE release_id = ?")
                        .bind(error.to_string()).bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
                }
            }
        }
        return Ok(());
    }

    let batch = sqlx::query("SELECT id FROM release_batches WHERE state = 'artifacts_ready' AND NOT EXISTS (SELECT 1 FROM releases WHERE releases.batch_id = release_batches.id) ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(batch) = batch else {
        return Ok(());
    };
    let publisher = sqlx::query("SELECT id, writer_epoch FROM workers WHERE role = 'publisher' AND state = 'online' ORDER BY name LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(publisher) = publisher else {
        return Ok(());
    };
    let batch_id: String = batch.get("id");
    let artifact_rows = sqlx::query("SELECT artifacts.path, artifacts.sha256, artifacts.size, artifacts.package_name, artifacts.package_version, artifacts.architecture FROM artifacts JOIN jobs ON jobs.id = artifacts.job_id WHERE jobs.batch_id = ? AND jobs.kind = 'build' AND jobs.status = 'succeeded' ORDER BY artifacts.path")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    if artifact_rows.is_empty() {
        return Err(ApiError::conflict(
            "ARTIFACTS_MISSING",
            "ReleaseBatch 没有可发布 Artifact",
        ));
    }
    let previous_rows = sqlx::query("SELECT artifacts.path, artifacts.sha256, artifacts.size, artifacts.package_name, artifacts.package_version, artifacts.architecture FROM artifacts JOIN release_artifacts ON release_artifacts.artifact_sha256 = artifacts.sha256 JOIN releases ON releases.id = release_artifacts.release_id WHERE releases.id = (SELECT id FROM releases WHERE state = 'committed' ORDER BY committed_at DESC LIMIT 1) ORDER BY artifacts.path")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
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
    let changed_artifacts = artifact_rows
        .into_iter()
        .map(parse_artifact)
        .collect::<Result<Vec<_>, _>>()?;
    let artifacts = merge_release_artifacts(previous_artifacts, changed_artifacts);
    let revision_sha256s = sqlx::query_scalar::<_, String>("SELECT DISTINCT revisions.input_sha256 FROM revisions JOIN jobs ON jobs.revision_id = revisions.id WHERE jobs.batch_id = ? ORDER BY revisions.input_sha256")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let audit_report_sha256s = sqlx::query_scalar::<_, String>("SELECT DISTINCT audit_decisions.report_sha256 FROM audit_decisions JOIN jobs ON jobs.revision_id = audit_decisions.revision_id WHERE jobs.batch_id = ? ORDER BY audit_decisions.report_sha256")
        .bind(&batch_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let release_id = Uuid::new_v4();
    let writer_epoch =
        u64::try_from(publisher.get::<i64, _>("writer_epoch")).map_err(ApiError::internal)?;
    let now = Utc::now();
    let authorization = ReleaseAuthorization {
        release_id,
        batch_id: Uuid::parse_str(&batch_id).map_err(ApiError::internal)?,
        writer_epoch,
        repository_name: state.config.repository_name.clone(),
        source_git_commit: state.config.source_git_commit.clone(),
        revision_sha256s,
        audit_report_sha256s,
        artifacts,
        issued_at: now,
        expires_at: now + Duration::hours(1),
    };
    let envelope = SignedEnvelope::sign(
        "aursmith.release_authorization",
        &authorization,
        &state.signing_key,
    )
    .map_err(ApiError::internal)?;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO releases(id, batch_id, state, manifest_sha256, source_git_commit, writer_epoch, created_at) VALUES (?, ?, 'authorizing', ?, ?, ?, ?)")
        .bind(release_id.to_string()).bind(&batch_id).bind(format!("pending:{release_id}"))
        .bind(&state.config.source_git_commit).bind(i64::try_from(writer_epoch).map_err(ApiError::internal)?).bind(now)
        .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO release_authorizations(release_id, publisher_worker_id, state, envelope_json, expires_at, created_at, updated_at) VALUES (?, ?, 'issued', ?, ?, ?, ?)")
        .bind(release_id.to_string()).bind(publisher.get::<String,_>("id"))
        .bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
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
    sqlx::query("UPDATE release_authorizations SET state = 'failed', last_error = ?, updated_at = ? WHERE release_id = ?")
        .bind(error).bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE release_batches SET state = 'publish_failed', failure_reason = ?, updated_at = ? WHERE id = (SELECT batch_id FROM releases WHERE id = ?)")
        .bind(error).bind(Utc::now()).bind(release_id).execute(&state.database).await.map_err(ApiError::internal)?;
    let fingerprint = format!("release-publish-failed:{release_id}");
    sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'warning', 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = CASE WHEN alerts.state = 'resolved' THEN 'open' ELSE alerts.state END, details_json = excluded.details_json, resolved_at = NULL")
        .bind(Uuid::new_v4().to_string()).bind(fingerprint).bind("Release 发布失败")
        .bind(json!({"release_id": release_id, "error": error}).to_string()).bind(Utc::now())
        .execute(&state.database).await.map_err(ApiError::internal)?;
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

async fn dispatch_transfer_one(state: &AppState) -> Result<(), ApiError> {
    let cleanup = sqlx::query("SELECT transfer_capabilities.id, transfer_capabilities.envelope_json, workers.endpoint FROM transfer_capabilities JOIN workers ON workers.id = transfer_capabilities.source_worker_id WHERE transfer_capabilities.state = 'verified' AND transfer_capabilities.export_cleaned_at IS NULL ORDER BY transfer_capabilities.updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = cleanup {
        let envelope: SignedEnvelope =
            serde_json::from_str(row.get("envelope_json")).map_err(ApiError::internal)?;
        transport::complete_export(&state.config, row.get("endpoint"), &envelope).await?;
        sqlx::query(
            "UPDATE transfer_capabilities SET export_cleaned_at = ?, updated_at = ? WHERE id = ?",
        )
        .bind(Utc::now())
        .bind(Utc::now())
        .bind(row.get::<String, _>("id"))
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
        return Ok(());
    }
    sqlx::query("UPDATE transfer_capabilities SET state = 'expired', updated_at = ? WHERE state IN ('issued', 'export_ready') AND expires_at <= ?")
        .bind(Utc::now()).bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
    let pending = sqlx::query("SELECT transfer_capabilities.id, transfer_capabilities.state, transfer_capabilities.envelope_json, transfer_capabilities.attempt_count, source.endpoint AS source_endpoint, destination.endpoint AS destination_endpoint FROM transfer_capabilities JOIN workers AS source ON source.id = transfer_capabilities.source_worker_id JOIN workers AS destination ON destination.id = transfer_capabilities.destination_worker_id WHERE transfer_capabilities.state IN ('issued', 'export_ready') AND transfer_capabilities.expires_at > ? ORDER BY transfer_capabilities.updated_at LIMIT 1")
        .bind(Utc::now()).fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = pending {
        let id: String = row.get("id");
        let transfer_state: String = row.get("state");
        let envelope: SignedEnvelope =
            serde_json::from_str(row.get("envelope_json")).map_err(ApiError::internal)?;
        let result = if transfer_state == "issued" {
            transport::authorize_export(&state.config, row.get("source_endpoint"), &envelope)
                .await
                .map(|_| "export_ready")
        } else {
            transport::authorize_import(&state.config, row.get("destination_endpoint"), &envelope)
                .await
                .map(|_| "verified")
        };
        match result {
            Ok(next_state) => {
                sqlx::query("UPDATE transfer_capabilities SET state = ?, last_error = NULL, updated_at = ? WHERE id = ?")
                    .bind(next_state).bind(Utc::now()).bind(&id).execute(&state.database).await.map_err(ApiError::internal)?;
            }
            Err(error) => {
                let attempts: i64 = row.get("attempt_count");
                sqlx::query("UPDATE transfer_capabilities SET state = CASE WHEN attempt_count + 1 >= 3 THEN 'failed' ELSE state END, attempt_count = attempt_count + 1, last_error = ?, updated_at = ? WHERE id = ?")
                    .bind(error.to_string()).bind(Utc::now()).bind(&id).execute(&state.database).await.map_err(ApiError::internal)?;
                if attempts + 1 >= 3 {
                    sqlx::query("UPDATE release_batches SET state = 'transfer_failed', failure_reason = ?, updated_at = ? WHERE id = (SELECT batch_id FROM transfer_capabilities WHERE id = ?)")
                        .bind(error.to_string()).bind(Utc::now()).bind(&id).execute(&state.database).await.map_err(ApiError::internal)?;
                    let fingerprint = format!("artifact-transfer-failed:{id}");
                    sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'warning', 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = CASE WHEN alerts.state = 'resolved' THEN 'open' ELSE alerts.state END, details_json = excluded.details_json, resolved_at = NULL")
                        .bind(Uuid::new_v4().to_string()).bind(fingerprint).bind("Artifact 传输失败")
                        .bind(json!({"transfer_capability_id": id, "error": error.to_string()}).to_string())
                        .bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
                }
            }
        }
        return Ok(());
    }

    let candidate = sqlx::query("SELECT jobs.id AS job_id, jobs.batch_id, jobs.worker_id AS source_worker_id, workers.endpoint AS source_endpoint FROM jobs JOIN workers ON workers.id = jobs.worker_id WHERE jobs.kind = 'build' AND jobs.status = 'succeeded' AND jobs.batch_id IN (SELECT id FROM release_batches WHERE state = 'ready_to_publish') AND NOT EXISTS (SELECT 1 FROM transfer_capabilities WHERE transfer_capabilities.source_job_id = jobs.id AND transfer_capabilities.state IN ('issued', 'export_ready', 'verified')) ORDER BY jobs.updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = candidate {
        let publisher = sqlx::query("SELECT id, writer_epoch FROM workers WHERE role = 'publisher' AND state = 'online' ORDER BY name LIMIT 1")
            .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
        let Some(publisher) = publisher else {
            return Ok(());
        };
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
        let files = artifacts
            .into_iter()
            .map(|artifact| aursmith_protocol::ManifestEntry {
                path: artifact.get("path"),
                sha256: artifact.get("sha256"),
                size: u64::try_from(artifact.get::<i64, _>("size")).unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        let transfer_id = Uuid::new_v4();
        let now = Utc::now();
        let expires_at = now + Duration::hours(1);
        let capability = aursmith_protocol::TransferCapability {
            id: transfer_id,
            source_worker: Uuid::parse_str(row.get("source_worker_id"))
                .map_err(ApiError::internal)?,
            destination_worker: Uuid::parse_str(publisher.get("id")).map_err(ApiError::internal)?,
            attempt: Some(AttemptRef {
                job_id: Uuid::parse_str(&job_id).map_err(ApiError::internal)?,
                attempt_id: Uuid::parse_str(attempt.get("id")).map_err(ApiError::internal)?,
                generation: u32::try_from(attempt.get::<i64, _>("generation"))
                    .map_err(ApiError::internal)?,
            }),
            release_id: None,
            writer_epoch: u64::try_from(publisher.get::<i64, _>("writer_epoch"))
                .map_err(ApiError::internal)?,
            files,
            expires_at,
        };
        let envelope = SignedEnvelope::sign(
            "aursmith.transfer_capability",
            &capability,
            &state.signing_key,
        )
        .map_err(ApiError::internal)?;
        sqlx::query("INSERT INTO transfer_capabilities(id, batch_id, source_job_id, source_worker_id, destination_worker_id, state, envelope_json, expires_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, 'issued', ?, ?, ?, ?)")
            .bind(transfer_id.to_string()).bind(row.get::<String,_>("batch_id")).bind(&job_id)
            .bind(row.get::<String,_>("source_worker_id")).bind(publisher.get::<String,_>("id"))
            .bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
            .bind(expires_at).bind(now).bind(now).execute(&state.database).await.map_err(ApiError::internal)?;
        return Ok(());
    }

    let batches: Vec<String> = sqlx::query_scalar("SELECT id FROM release_batches WHERE state = 'ready_to_publish' AND EXISTS (SELECT 1 FROM jobs WHERE jobs.batch_id = release_batches.id AND jobs.kind = 'build') AND NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.batch_id = release_batches.id AND jobs.kind = 'build' AND NOT EXISTS (SELECT 1 FROM transfer_capabilities WHERE transfer_capabilities.source_job_id = jobs.id AND transfer_capabilities.state = 'verified'))")
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

pub async fn probe_worker(state: &AppState, worker_id: &str) -> Result<String, ApiError> {
    let row = sqlx::query(
        "SELECT endpoint, role, writer_epoch, identity_signing_key_hex FROM workers WHERE id = ?",
    )
    .bind(worker_id)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("Worker 不存在"))?;
    let endpoint: String = row.get("endpoint");
    match transport::status(&state.config, &endpoint).await {
        Ok(reply) => {
            let protocol = reply.data["protocol_major"].as_u64().unwrap_or_default();
            let remote_role = reply.data["role"].as_str().unwrap_or_default();
            let remote_id = reply.data["instance_id"].as_str().unwrap_or_default();
            let expected_role: String = row.get("role");
            let expected_writer_epoch: i64 = row.get("writer_epoch");
            let expected_signing_key: Option<String> = row.get("identity_signing_key_hex");
            let writer_epoch_mismatch = expected_role == "publisher"
                && reply.data["writer_epoch"].as_u64() != u64::try_from(expected_writer_epoch).ok();
            let new_state = if remote_id != worker_id
                || protocol != u64::from(aursmith_protocol::PROTOCOL_MAJOR)
                || remote_role != expected_role
                || writer_epoch_mismatch
                || reply.data["identity_signing_key_hex"].as_str()
                    != expected_signing_key.as_deref()
            {
                "incompatible"
            } else {
                "online"
            };
            sqlx::query(
                "UPDATE workers SET state = ?, profiles_json = ?, last_seen_at = ?, updated_at = ? WHERE id = ?",
            )
            .bind(new_state)
            .bind(reply.data["profiles"].to_string())
            .bind(Utc::now())
            .bind(Utc::now())
            .bind(worker_id)
            .execute(&state.database)
            .await
            .map_err(ApiError::internal)?;
            Ok(new_state.to_owned())
        }
        Err(error) => {
            sqlx::query(
                "UPDATE workers SET state = CASE WHEN state = 'online' THEN 'degraded' ELSE 'offline' END, updated_at = ? WHERE id = ? AND state != 'draining'",
            )
            .bind(Utc::now())
            .bind(worker_id)
            .execute(&state.database)
            .await
            .map_err(ApiError::internal)?;
            Err(error)
        }
    }
}

async fn probe_all_workers(state: &AppState) -> Result<(), ApiError> {
    let worker_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM workers WHERE state != 'draining'")
            .fetch_all(&state.database)
            .await
            .map_err(ApiError::internal)?;
    for worker_id in worker_ids {
        let _ = probe_worker(state, &worker_id).await;
    }
    Ok(())
}

async fn dispatch_one(state: &AppState) -> Result<(), ApiError> {
    let job = sqlx::query(
        "SELECT id, batch_id, required_role, revision_sha256, kind, profile_sha256, source_manifest_sha256, dependency_snapshot_sha256, preferred_worker_id, source_attempt_id, inputs_json, inline_inputs_json, required_labels_json, limits_json FROM jobs WHERE status IN ('queued', 'no_eligible_worker') ORDER BY priority DESC, created_at LIMIT 1",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(job) = job else { return Ok(()) };
    let job_id: String = job.get("id");
    let role: String = job.get("required_role");
    let required_labels: BTreeSet<String> =
        serde_json::from_str(job.get("required_labels_json")).map_err(ApiError::internal)?;
    let workers = sqlx::query(
        "SELECT id, endpoint, labels_json, profiles_json FROM workers WHERE role = ? AND state = 'online' AND protocol_version = ? ORDER BY name",
    )
    .bind(&role)
    .bind(i64::from(aursmith_protocol::PROTOCOL_MAJOR))
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let required_profile: Option<String> = job.get("profile_sha256");
    let mut preferred_worker_id: Option<String> = job.get("preferred_worker_id");
    if preferred_worker_id.is_none()
        && let Some(batch_id) = job.get::<Option<String>, _>("batch_id")
    {
        preferred_worker_id = sqlx::query_scalar("SELECT worker_id FROM jobs WHERE batch_id = ? AND worker_id IS NOT NULL ORDER BY created_at LIMIT 1")
            .bind(batch_id).fetch_optional(&state.database).await.map_err(ApiError::internal)?.flatten();
    }
    let selected = workers.into_iter().find(|worker| {
        let labels: BTreeSet<String> =
            serde_json::from_str(worker.get("labels_json")).unwrap_or_default();
        let profiles: BTreeSet<String> =
            serde_json::from_str(worker.get("profiles_json")).unwrap_or_default();
        required_labels.is_subset(&labels)
            && preferred_worker_id
                .as_ref()
                .is_none_or(|preferred| worker.get::<String, _>("id") == *preferred)
            && required_profile
                .as_ref()
                .is_none_or(|profile| profiles.contains(profile))
    });
    let Some(worker) = selected else {
        sqlx::query("UPDATE jobs SET status = 'no_eligible_worker', failure_code = 'NO_ELIGIBLE_WORKER', updated_at = ? WHERE id = ?")
            .bind(Utc::now())
            .bind(&job_id)
            .execute(&state.database)
            .await
            .map_err(ApiError::internal)?;
        upsert_no_worker_alert(state, &job_id, &role, &required_labels).await?;
        return Ok(());
    };

    let worker_id: String = worker.get("id");
    let endpoint: String = worker.get("endpoint");
    let generation: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(generation), -1) + 1 FROM attempts WHERE job_id = ?",
    )
    .bind(&job_id)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let parsed_job_id = Uuid::parse_str(&job_id).map_err(ApiError::internal)?;
    let attempt_id = Uuid::new_v4();
    let attempt = AttemptRef {
        job_id: parsed_job_id,
        attempt_id,
        generation: u32::try_from(generation).map_err(ApiError::internal)?,
    };
    let limits: ResourceLimits = serde_json::from_str(
        job.get::<Option<String>, _>("limits_json")
            .as_deref()
            .unwrap_or(
                "{\"cpu_count\":1,\"memory_mib\":1024,\"disk_mib\":4096,\"timeout_seconds\":600}",
            ),
    )
    .map_err(ApiError::internal)?;
    let now = Utc::now();
    let spec = JobSpec {
        job_id: parsed_job_id,
        attempt: attempt.clone(),
        required_role: parse_role(&role)?,
        kind: parse_job_kind(job.get("kind"))?,
        revision_sha256: job
            .get::<Option<String>, _>("revision_sha256")
            .unwrap_or_else(|| "0".repeat(64)),
        source_manifest_sha256: job.get("source_manifest_sha256"),
        dependency_snapshot_sha256: job.get("dependency_snapshot_sha256"),
        profile_sha256: job.get("profile_sha256"),
        source_attempt_id: job
            .get::<Option<String>, _>("source_attempt_id")
            .map(|value| Uuid::parse_str(&value))
            .transpose()
            .map_err(ApiError::internal)?,
        dependency_attempt_ids: load_batch_dependency_attempts(state, &job_id).await?,
        dependencies: load_job_dependencies(state, &job_id).await?,
        inputs: serde_json::from_str(job.get("inputs_json")).map_err(ApiError::internal)?,
        inline_inputs: serde_json::from_str(job.get("inline_inputs_json"))
            .map_err(ApiError::internal)?,
        limits,
        issued_at: now,
        expires_at: now + Duration::minutes(10),
    };
    let envelope = SignedEnvelope::sign("aursmith.job_spec", &spec, &state.signing_key)
        .map_err(ApiError::internal)?;
    let signed_spec = serde_json::to_string(&envelope).map_err(ApiError::internal)?;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO attempts(id, job_id, generation, token_sha256, status) VALUES (?, ?, ?, ?, 'dispatched')",
    )
    .bind(attempt_id.to_string())
    .bind(&job_id)
    .bind(generation)
    .bind(envelope.payload_sha256.clone())
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    sqlx::query("UPDATE jobs SET worker_id = ?, status = 'dispatched', failure_code = NULL, signed_spec_json = ?, updated_at = ? WHERE id = ?")
        .bind(&worker_id)
        .bind(&signed_spec)
        .bind(now)
        .bind(&job_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    transaction.commit().await.map_err(ApiError::internal)?;

    if let Err(error) = transport::submit(&state.config, &endpoint, &envelope).await {
        sqlx::query("UPDATE jobs SET status = 'uncertain', failure_code = 'DISPATCH_UNCERTAIN', updated_at = ? WHERE id = ?")
            .bind(Utc::now())
            .bind(&job_id)
            .execute(&state.database)
            .await
            .map_err(ApiError::internal)?;
        return Err(error);
    }
    resolve_alert(state, &format!("no-eligible-worker:{job_id}")).await?;
    Ok(())
}

async fn reconcile_one(state: &AppState) -> Result<(), ApiError> {
    let row = sqlx::query(
        "SELECT jobs.id, jobs.kind, jobs.profile_sha256, jobs.revision_id, jobs.revision_sha256, jobs.batch_id, workers.endpoint FROM jobs JOIN workers ON workers.id = jobs.worker_id WHERE jobs.status IN ('uncertain', 'dispatched', 'running') ORDER BY jobs.updated_at LIMIT 1",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(row) = row else { return Ok(()) };
    let job_id: String = row.get("id");
    let endpoint: String = row.get("endpoint");
    let reply = transport::query(&state.config, &endpoint, &job_id).await?;
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
    let mut advance_build_batch = false;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE attempts SET status = ?, result_sha256 = ? WHERE id = ?")
        .bind(status)
        .bind(reply.data["result_sha256"].as_str())
        .bind(attempt_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("UPDATE jobs SET status = ?, failure_code = ?, updated_at = ? WHERE id = ?")
        .bind(status)
        .bind(failure)
        .bind(Utc::now())
        .bind(&job_id)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    if row.get::<String, _>("kind") == "profile_fixture" {
        let profile_sha: Option<String> = row.get("profile_sha256");
        if let Some(profile_sha) = profile_sha {
            if status == "succeeded" {
                sqlx::query("UPDATE build_profiles SET last_verified_at = ?, failure_reason = NULL WHERE manifest_sha256 = ?")
                    .bind(Utc::now()).bind(profile_sha).execute(&mut *transaction).await.map_err(ApiError::internal)?;
            } else if status == "failed" {
                sqlx::query("UPDATE build_profiles SET state = 'failed', failure_reason = ? WHERE manifest_sha256 = ?")
                    .bind(failure).bind(profile_sha).execute(&mut *transaction).await.map_err(ApiError::internal)?;
            }
        }
    }
    if row.get::<String, _>("kind") == "fetch" && status == "succeeded" {
        let revision_id: Option<String> = row.get("revision_id");
        let Some(revision_id) = revision_id else {
            return Err(ApiError::internal("Fetch Job 缺少 revision_id"));
        };
        let Some(GuestResult::Fetch(fetch_result)) = guest_result.as_ref() else {
            return Err(ApiError::conflict(
                "RESULT_KIND_MISMATCH",
                "Fetch Job 返回了其他类型的 GuestResult",
            ));
        };
        let expected_revision: Option<String> = row.get("revision_sha256");
        if fetch_result.job_id.to_string() != job_id
            || fetch_result.attempt.attempt_id.to_string() != attempt_id
            || expected_revision.as_deref() != Some(fetch_result.revision_sha256.as_str())
        {
            return Err(ApiError::conflict(
                "RESULT_IDENTITY_MISMATCH",
                "GuestResult 身份与 Controller 的 Job/Attempt/Revision 不一致",
            ));
        }
        crate::packages::complete_fetch(&mut transaction, &revision_id, fetch_result).await?;
        if let Some(batch_id) = row.get::<Option<String>, _>("batch_id") {
            let unfinished: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE batch_id = ? AND kind = 'fetch' AND status != 'succeeded'")
                .bind(&batch_id).fetch_one(&mut *transaction).await.map_err(ApiError::internal)?;
            if unfinished == 0 {
                sqlx::query("UPDATE release_batches SET state = 'awaiting_audit', updated_at = ? WHERE id = ?")
                    .bind(Utc::now()).bind(batch_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
            }
        }
    } else if row.get::<String, _>("kind") == "fetch"
        && status == "failed"
        && let Some(batch_id) = row.get::<Option<String>, _>("batch_id")
    {
        sqlx::query("UPDATE release_batches SET state = 'fetch_failed', failure_reason = ?, updated_at = ? WHERE id = ?")
            .bind(failure).bind(Utc::now()).bind(batch_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
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
        for artifact in &build_result.artifacts {
            sqlx::query("INSERT INTO artifacts(sha256, job_id, path, size, package_name, package_version, architecture, provenance_json, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(sha256) DO NOTHING")
                .bind(&artifact.sha256).bind(&job_id).bind(&artifact.path)
                .bind(i64::try_from(artifact.size).map_err(ApiError::internal)?)
                .bind(&artifact.package_name).bind(&artifact.package_version).bind(&artifact.architecture)
                .bind(serde_json::to_string(&build_result.provenance).map_err(ApiError::internal)?)
                .bind(Utc::now()).execute(&mut *transaction).await.map_err(ApiError::internal)?;
        }
        if let Some(revision_id) = row.get::<Option<String>, _>("revision_id") {
            sqlx::query("UPDATE revisions SET state = 'built' WHERE id = ?")
                .bind(revision_id)
                .execute(&mut *transaction)
                .await
                .map_err(ApiError::internal)?;
        }
        advance_build_batch = true;
    } else if row.get::<String, _>("kind") == "build"
        && status == "failed"
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

async fn upsert_no_worker_alert(
    state: &AppState,
    job_id: &str,
    role: &str,
    labels: &BTreeSet<String>,
) -> Result<(), ApiError> {
    let fingerprint = format!("no-eligible-worker:{job_id}");
    sqlx::query(
        "INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'warning', 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = CASE WHEN alerts.state = 'resolved' THEN 'open' ELSE alerts.state END, resolved_at = NULL",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(&fingerprint)
    .bind("没有符合条件的 Worker")
    .bind(json!({"job_id": job_id, "role": role, "labels": labels}).to_string())
    .bind(Utc::now())
    .execute(&state.database)
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

async fn resolve_alert(state: &AppState, fingerprint: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE alerts SET state = 'resolved', resolved_at = ? WHERE fingerprint = ? AND state != 'resolved'")
        .bind(Utc::now())
        .bind(fingerprint)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

fn parse_role(value: &str) -> Result<aursmith_domain::WorkerRole, ApiError> {
    match value {
        "builder" => Ok(aursmith_domain::WorkerRole::Builder),
        "publisher" => Ok(aursmith_domain::WorkerRole::Publisher),
        "archiver" => Ok(aursmith_domain::WorkerRole::Archiver),
        _ => Err(ApiError::internal("数据库包含未知 Worker 角色")),
    }
}

fn parse_job_kind(value: &str) -> Result<JobKind, ApiError> {
    match value {
        "fetch" => Ok(JobKind::Fetch),
        "build" => Ok(JobKind::Build),
        "profile_fixture" => Ok(JobKind::ProfileFixture),
        _ => Err(ApiError::internal("数据库包含未知 Job 类型")),
    }
}

async fn load_job_dependencies(
    state: &AppState,
    job_id: &str,
) -> Result<Vec<DependencyInput>, ApiError> {
    let rows = sqlx::query("SELECT revision_dependencies.dependency_name, revision_dependencies.dependency_kind, revision_dependencies.target_package_base FROM jobs JOIN revision_dependencies ON revision_dependencies.revision_id = jobs.revision_id WHERE jobs.id = ? ORDER BY revision_dependencies.dependency_name, revision_dependencies.dependency_kind")
        .bind(job_id).fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(rows
        .into_iter()
        .map(|row| DependencyInput {
            name: row.get("dependency_name"),
            kind: row.get("dependency_kind"),
            source: if row
                .get::<Option<String>, _>("target_package_base")
                .is_some()
            {
                DependencySource::AurBatch
            } else {
                DependencySource::Official
            },
        })
        .collect())
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
}
