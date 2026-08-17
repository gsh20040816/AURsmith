use crate::{error::ApiError, routes::AppState, transport};
use aursmith_domain::AttemptRef;
use aursmith_protocol::{
    ArtifactRecord, DependencyInput, DependencySource, GuestResult, JobKind, JobSpec,
    ManifestEntry, ReleaseAuthorization, ReleaseEvidence, ReleaseEvidenceRecord, ResourceLimits,
    SignedEnvelope,
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
    let external_archiver_enabled = state.config.external_archiver_enabled;
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
    let notification_state = state.clone();
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(10));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = crate::notifications::dispatch_one(&notification_state).await {
                tracing::warn!(%error, "告警通知调度失败");
            }
        }
    });
    let backup_state = state.clone();
    tokio::spawn(async move {
        let mut timer = interval(std::time::Duration::from_secs(60 * 60));
        timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            if let Err(error) = crate::backups::create_if_due(&backup_state).await {
                tracing::warn!(%error, "控制面自动备份失败");
            }
        }
    });
    if external_archiver_enabled {
        let inventory_state = state.clone();
        tokio::spawn(async move {
            let mut timer = interval(std::time::Duration::from_secs(6 * 60 * 60));
            timer.set_missed_tick_behavior(MissedTickBehavior::Skip);
            loop {
                timer.tick().await;
                if let Err(error) = run_archive_inventory_if_due(&inventory_state).await {
                    tracing::warn!(%error, "Archiver 库存巡检失败");
                }
            }
        });
    }
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
            if external_archiver_enabled {
                if let Err(error) = dispatch_archive_one(&state).await {
                    tracing::warn!(%error, "Release 归档调度失败");
                }
                if let Err(error) = dispatch_backup_archive_one(&state).await {
                    tracing::warn!(%error, "控制面备份归档调度失败");
                }
            }
        }
    });
}

fn manifest_entries_match(left: &[ManifestEntry], right: &[ManifestEntry]) -> bool {
    fn index(entries: &[ManifestEntry]) -> Option<BTreeMap<String, (String, u64)>> {
        let mut indexed = BTreeMap::new();
        for entry in entries {
            if indexed
                .insert(entry.path.clone(), (entry.sha256.clone(), entry.size))
                .is_some()
            {
                return None;
            }
        }
        Some(indexed)
    }

    left.len() == right.len() && index(left).is_some_and(|left| Some(left) == index(right))
}

async fn dispatch_backup_archive_one(state: &AppState) -> Result<(), ApiError> {
    let pending = sqlx::query("SELECT control_plane_backup_archives.id, control_plane_backup_archives.backup_id, control_plane_backup_archives.envelope_json, control_plane_backup_archives.export_directory, control_plane_backup_archives.attempt_count, control_plane_backup_archives.expires_at, workers.endpoint, workers.identity_signing_key_hex FROM control_plane_backup_archives JOIN workers ON workers.id = control_plane_backup_archives.archiver_worker_id WHERE control_plane_backup_archives.state = 'issued' ORDER BY control_plane_backup_archives.updated_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    if let Some(row) = pending {
        let transfer_id: String = row.get("id");
        let backup_id: String = row.get("backup_id");
        let expires_at: String = row.get("expires_at");
        if expires_at
            .parse::<chrono::DateTime<Utc>>()
            .is_ok_and(|value| value <= Utc::now())
        {
            sqlx::query("UPDATE control_plane_backup_archives SET state = 'failed', last_error = 'CAPABILITY_EXPIRED', updated_at = ? WHERE id = ?")
                .bind(Utc::now()).bind(&transfer_id).execute(&state.database).await.map_err(ApiError::internal)?;
            return Ok(());
        }
        let envelope: SignedEnvelope =
            serde_json::from_str(row.get("envelope_json")).map_err(ApiError::internal)?;
        match transport::authorize_import(&state.config, row.get("endpoint"), &envelope).await {
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
                        "BACKUP_RECEIPT_UNTRUSTED",
                        "备份归档 Receipt 身份签名不匹配",
                    ));
                }
                let receipt: aursmith_protocol::BackupArchiveReceipt = receipt_envelope
                    .verify("aursmith.backup_archive_receipt")
                    .map_err(ApiError::internal)?;
                let capability: aursmith_protocol::TransferCapability = envelope
                    .verify("aursmith.transfer_capability")
                    .map_err(ApiError::internal)?;
                if receipt.backup_id.to_string() != backup_id
                    || receipt.archive_worker != capability.destination_worker
                    || !manifest_entries_match(&receipt.files, &capability.files)
                {
                    return Err(ApiError::conflict(
                        "BACKUP_RECEIPT_MISMATCH",
                        "备份归档 Receipt 与 Capability 不一致",
                    ));
                }
                sqlx::query("UPDATE control_plane_backup_archives SET state = 'verified', receipt_sha256 = ?, last_error = NULL, updated_at = ? WHERE id = ?")
                    .bind(&receipt_envelope.payload_sha256).bind(Utc::now()).bind(&transfer_id)
                    .execute(&state.database).await.map_err(ApiError::internal)?;
                let export = std::path::PathBuf::from(row.get::<String, _>("export_directory"));
                if export.starts_with(&state.config.backup_export_dir) {
                    let _ = std::fs::remove_dir_all(export);
                }
            }
            Err(error) => {
                let attempts: i64 = row.get("attempt_count");
                let terminal = attempts + 1 >= 3;
                sqlx::query("UPDATE control_plane_backup_archives SET state = CASE WHEN ? THEN 'failed' ELSE state END, attempt_count = attempt_count + 1, last_error = ?, updated_at = ? WHERE id = ?")
                    .bind(terminal).bind(error.to_string()).bind(Utc::now()).bind(&transfer_id)
                    .execute(&state.database).await.map_err(ApiError::internal)?;
                if terminal {
                    upsert_operational_alert(
                        state,
                        &format!("backup-archive:{backup_id}"),
                        "warning",
                        "控制面备份归档失败",
                        json!({"backup_id": backup_id, "error": error.to_string()}),
                    )
                    .await?;
                }
            }
        }
        return Ok(());
    }
    let backup_id: Option<String> = sqlx::query_scalar("SELECT id FROM control_plane_backups WHERE state = 'verified' AND NOT EXISTS (SELECT 1 FROM control_plane_backup_archives WHERE control_plane_backup_archives.backup_id = control_plane_backups.id) ORDER BY created_at LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(backup_id) = backup_id else {
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
    let transfer_id = Uuid::new_v4();
    let parsed_backup_id = Uuid::parse_str(&backup_id).map_err(ApiError::internal)?;
    let (files, export_directory) =
        crate::backups::prepare_export(state, parsed_backup_id, transfer_id).await?;
    let now = Utc::now();
    let capability = aursmith_protocol::TransferCapability {
        id: transfer_id,
        source_worker: crate::backups::transfer_source_id(state),
        destination_worker: Uuid::parse_str(archiver.get("id")).map_err(ApiError::internal)?,
        attempt: None,
        release_id: None,
        backup_id: Some(parsed_backup_id),
        writer_epoch: 0,
        files,
        expires_at: now + Duration::hours(1),
    };
    let envelope = SignedEnvelope::sign(
        "aursmith.transfer_capability",
        &capability,
        &state.signing_key,
    )
    .map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO control_plane_backup_archives(id, backup_id, archiver_worker_id, state, envelope_json, export_directory, expires_at, created_at, updated_at) VALUES (?, ?, ?, 'issued', ?, ?, ?, ?, ?)")
        .bind(transfer_id.to_string()).bind(&backup_id).bind(archiver.get::<String,_>("id"))
        .bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
        .bind(export_directory.to_string_lossy().as_ref()).bind(capability.expires_at).bind(now).bind(now)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(())
}

async fn run_archive_inventory_if_due(state: &AppState) -> Result<(), ApiError> {
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT checked_at FROM archive_inventories ORDER BY checked_at DESC LIMIT 1",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if latest
        .and_then(|value| value.parse::<chrono::DateTime<Utc>>().ok())
        .is_some_and(|value| value > Utc::now() - Duration::days(7))
    {
        return Ok(());
    }
    let latest_full: Option<String> = sqlx::query_scalar("SELECT checked_at FROM archive_inventories WHERE full_digest = 1 ORDER BY checked_at DESC LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let full_digest = latest_full
        .and_then(|value| value.parse::<chrono::DateTime<Utc>>().ok())
        .is_none_or(|value| value <= Utc::now() - Duration::days(90));
    let worker = sqlx::query("SELECT id, endpoint, identity_signing_key_hex FROM workers WHERE role = 'archiver' AND state = 'online' ORDER BY name LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(worker) = worker else { return Ok(()) };
    let worker_id: String = worker.get("id");
    let expected_key: String = worker
        .get::<Option<String>, _>("identity_signing_key_hex")
        .ok_or_else(|| ApiError::internal("Archiver 缺少身份公钥"))?;
    let reply =
        transport::archive_inventory(&state.config, worker.get("endpoint"), full_digest).await?;
    let envelope: SignedEnvelope =
        serde_json::from_value(reply.data["report"].clone()).map_err(ApiError::internal)?;
    if envelope.verifying_key != hex::decode(expected_key).map_err(ApiError::internal)? {
        return Err(ApiError::conflict(
            "ARCHIVE_INVENTORY_UNTRUSTED",
            "库存报告身份签名不匹配",
        ));
    }
    let report: aursmith_protocol::ArchiveInventory = envelope
        .verify("aursmith.archive_inventory")
        .map_err(ApiError::internal)?;
    if report.archive_worker.to_string() != worker_id || report.full_digest != full_digest {
        return Err(ApiError::conflict(
            "ARCHIVE_INVENTORY_MISMATCH",
            "库存报告与请求不一致",
        ));
    }
    sqlx::query("INSERT INTO archive_inventories(id, archiver_worker_id, full_digest, release_count, backup_count, file_count, byte_count, failure_count, envelope_json, checked_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)")
        .bind(Uuid::new_v4().to_string()).bind(&worker_id).bind(full_digest)
        .bind(i64::try_from(report.release_count).map_err(ApiError::internal)?)
        .bind(i64::try_from(report.backup_count).map_err(ApiError::internal)?)
        .bind(i64::try_from(report.file_count).map_err(ApiError::internal)?)
        .bind(i64::try_from(report.byte_count).map_err(ApiError::internal)?)
        .bind(i64::try_from(report.failures.len()).map_err(ApiError::internal)?)
        .bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
        .bind(report.checked_at).execute(&state.database).await.map_err(ApiError::internal)?;
    if report.failures.is_empty() {
        resolve_alert(state, &format!("archive-inventory:{worker_id}")).await?;
    } else {
        upsert_operational_alert(state, &format!("archive-inventory:{worker_id}"), "critical", "Archiver 库存巡检发现损坏", json!({"worker_id": worker_id, "full_digest": full_digest, "failures": report.failures})).await?;
    }
    Ok(())
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
                        || !manifest_entries_match(&receipt.files, &capability.files)
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
        backup_id: None,
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
    if publication_backpressure(&state.database).await? {
        return Ok(());
    }
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
                    sqlx::query("UPDATE release_authorizations SET last_error = ?, updated_at = ? WHERE release_id = ?")
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
    let publisher = sqlx::query("SELECT id, writer_epoch FROM workers WHERE role = 'publisher' AND state = 'online' ORDER BY name LIMIT 1")
        .fetch_optional(&state.database).await.map_err(ApiError::internal)?;
    let Some(publisher) = publisher else {
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
    let evidence = build_release_evidence(&state.database, &batch_id).await?;
    let mut evidence_files = Vec::new();
    if let Some(current_release) = current_release_id(&state.database).await? {
        let previous: Option<String> = sqlx::query_scalar(
            "SELECT envelope_json FROM release_authorizations WHERE release_id = ?",
        )
        .bind(current_release)
        .fetch_optional(&state.database)
        .await
        .map_err(ApiError::internal)?;
        if let Some(previous) = previous {
            let envelope: SignedEnvelope =
                serde_json::from_str(&previous).map_err(ApiError::internal)?;
            let previous: ReleaseAuthorization = envelope
                .verify("aursmith.release_authorization")
                .map_err(ApiError::internal)?;
            evidence_files.extend(previous.evidence_files);
        }
    }
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
        evidence_files,
        removed_package_names: removed_package_names.into_iter().collect(),
        include_repository_keyring: true,
        evidence,
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

async fn build_release_evidence(
    database: &sqlx::SqlitePool,
    batch_id: &str,
) -> Result<ReleaseEvidence, ApiError> {
    let mut records = Vec::new();
    let batch = sqlx::query("SELECT state, graph_json, failure_reason, created_at, updated_at FROM release_batches WHERE id = ?")
        .bind(batch_id).fetch_one(database).await.map_err(ApiError::internal)?;
    push_evidence(
        &mut records,
        "release_batch",
        batch_id,
        json!({
            "state": batch.get::<String, _>("state"),
            "graph": serde_json::from_str::<serde_json::Value>(batch.get("graph_json")).map_err(ApiError::internal)?,
            "failure_reason": batch.get::<Option<String>, _>("failure_reason"),
            "created_at": batch.get::<String, _>("created_at"),
            "updated_at": batch.get::<String, _>("updated_at")
        }),
    )?;
    let revisions = sqlx::query("SELECT DISTINCT revisions.id, revisions.package_base, revisions.aur_commit, revisions.vcs_commit, revisions.upstream_version, revisions.published_version, revisions.input_sha256, revisions.source_manifest_sha256, revisions.dependency_snapshot_sha256, revisions.audit_policy_version, revisions.metadata_json FROM revisions JOIN jobs ON jobs.revision_id = revisions.id WHERE jobs.batch_id = ? ORDER BY revisions.package_base")
        .bind(batch_id).fetch_all(database).await.map_err(ApiError::internal)?;
    for row in revisions {
        let identity: String = row.get("id");
        push_evidence(
            &mut records,
            "revision",
            &identity,
            json!({
                "id": &identity,
                "package_base": row.get::<String, _>("package_base"),
                "aur_commit": row.get::<String, _>("aur_commit"),
                "vcs_commit": row.get::<Option<String>, _>("vcs_commit"),
                "upstream_version": row.get::<String, _>("upstream_version"),
                "published_version": row.get::<Option<String>, _>("published_version"),
                "input_sha256": row.get::<String, _>("input_sha256"),
                "source_manifest_sha256": row.get::<Option<String>, _>("source_manifest_sha256"),
                "dependency_snapshot_sha256": row.get::<Option<String>, _>("dependency_snapshot_sha256"),
                "audit_policy_version": row.get::<String, _>("audit_policy_version"),
                "snapshot": serde_json::from_str::<serde_json::Value>(row.get("metadata_json")).map_err(ApiError::internal)?
            }),
        )?;
    }
    let audits = sqlx::query("SELECT DISTINCT audit_bundles.sha256, audit_bundles.policy_version, audit_bundles.payload_json, audit_bundles.coverage_json, audit_bundles.deterministic_findings_json, audit_bundles.state FROM audit_bundles JOIN jobs ON jobs.revision_id = audit_bundles.revision_id WHERE jobs.batch_id = ? ORDER BY audit_bundles.sha256")
        .bind(batch_id).fetch_all(database).await.map_err(ApiError::internal)?;
    for row in audits {
        let identity: String = row.get("sha256");
        push_evidence(
            &mut records,
            "audit_bundle",
            &identity,
            json!({
                "sha256": &identity,
                "policy_version": row.get::<String, _>("policy_version"),
                "state": row.get::<String, _>("state"),
                "payload": serde_json::from_str::<serde_json::Value>(row.get("payload_json")).map_err(ApiError::internal)?,
                "coverage": serde_json::from_str::<serde_json::Value>(row.get("coverage_json")).map_err(ApiError::internal)?,
                "deterministic_findings": serde_json::from_str::<serde_json::Value>(row.get("deterministic_findings_json")).map_err(ApiError::internal)?
            }),
        )?;
    }
    let reports = sqlx::query("SELECT DISTINCT agent_runs.id, agent_runs.tier, agent_runs.slot, agent_runs.attempt, agent_runs.adapter, agent_runs.provider, agent_runs.model, agent_runs.adapter_version, agent_runs.prompt_version, agent_runs.verdict, agent_runs.report_json, agent_runs.raw_output_json, agent_runs.report_sha256, agent_runs.started_at, agent_runs.finished_at FROM agent_runs JOIN audit_bundles ON audit_bundles.sha256 = agent_runs.audit_bundle_sha256 JOIN jobs ON jobs.revision_id = audit_bundles.revision_id WHERE jobs.batch_id = ? AND agent_runs.status = 'succeeded' ORDER BY agent_runs.tier, agent_runs.slot, agent_runs.attempt")
        .bind(batch_id).fetch_all(database).await.map_err(ApiError::internal)?;
    for row in reports {
        let identity: String = row.get("id");
        push_evidence(
            &mut records,
            "agent_report",
            &identity,
            json!({
                "id": &identity, "tier": row.get::<String, _>("tier"), "slot": row.get::<i64, _>("slot"),
                "attempt": row.get::<i64, _>("attempt"), "adapter": row.get::<String, _>("adapter"),
                "provider": row.get::<String, _>("provider"), "model": row.get::<String, _>("model"),
                "adapter_version": row.get::<String, _>("adapter_version"), "prompt_version": row.get::<String, _>("prompt_version"),
                "verdict": row.get::<Option<String>, _>("verdict"),
                "report": row.get::<Option<String>, _>("report_json").map(|value| serde_json::from_str::<serde_json::Value>(&value)).transpose().map_err(ApiError::internal)?,
                "raw_output": row.get::<Option<String>, _>("raw_output_json").map(|value| serde_json::from_str::<serde_json::Value>(&value)).transpose().map_err(ApiError::internal)?,
                "report_sha256": row.get::<Option<String>, _>("report_sha256"),
                "started_at": row.get::<Option<String>, _>("started_at"), "finished_at": row.get::<Option<String>, _>("finished_at")
            }),
        )?;
    }
    let jobs = sqlx::query("SELECT job_evidence.job_id, job_evidence.kind, job_evidence.document_json, job_evidence.sha256 FROM job_evidence JOIN jobs ON jobs.id = job_evidence.job_id WHERE jobs.batch_id = ? ORDER BY jobs.created_at")
        .bind(batch_id).fetch_all(database).await.map_err(ApiError::internal)?;
    for row in jobs {
        let identity: String = row.get("job_id");
        push_evidence(
            &mut records,
            "job_result",
            &identity,
            json!({
                "job_id": &identity,
                "kind": row.get::<String, _>("kind"),
                "stored_sha256": row.get::<String, _>("sha256"),
                "result": serde_json::from_str::<serde_json::Value>(row.get("document_json")).map_err(ApiError::internal)?
            }),
        )?;
    }
    if records.len() > 10_000 {
        return Err(ApiError::conflict(
            "RELEASE_EVIDENCE_TOO_LARGE",
            "Release 证据记录超过 10000 条",
        ));
    }
    let evidence = ReleaseEvidence {
        schema_version: 1,
        records,
    };
    if serde_json::to_vec(&evidence)
        .map_err(ApiError::internal)?
        .len()
        > 16 * 1024 * 1024
    {
        return Err(ApiError::conflict(
            "RELEASE_EVIDENCE_TOO_LARGE",
            "Release 证据超过 16 MiB",
        ));
    }
    Ok(evidence)
}

fn push_evidence(
    records: &mut Vec<ReleaseEvidenceRecord>,
    kind: &str,
    identity: &str,
    document: serde_json::Value,
) -> Result<(), ApiError> {
    let sha256 = hex::encode(Sha256::digest(
        serde_json::to_vec(&document).map_err(ApiError::internal)?,
    ));
    records.push(ReleaseEvidenceRecord {
        kind: kind.to_owned(),
        identity: identity.to_owned(),
        sha256,
        document,
    });
    Ok(())
}

async fn dispatch_transfer_one(state: &AppState) -> Result<(), ApiError> {
    let cleanup = sqlx::query("SELECT transfer_capabilities.id, transfer_capabilities.envelope_json, workers.endpoint FROM transfer_capabilities JOIN workers ON workers.id = transfer_capabilities.source_worker_id WHERE transfer_capabilities.state = 'verified' AND transfer_capabilities.export_cleaned_at IS NULL AND workers.connection_mode = 'direct' ORDER BY transfer_capabilities.updated_at LIMIT 1")
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
    let pending = sqlx::query("SELECT transfer_capabilities.id, transfer_capabilities.state, transfer_capabilities.envelope_json, transfer_capabilities.attempt_count, source.endpoint AS source_endpoint, destination.endpoint AS destination_endpoint FROM transfer_capabilities JOIN workers AS source ON source.id = transfer_capabilities.source_worker_id JOIN workers AS destination ON destination.id = transfer_capabilities.destination_worker_id WHERE transfer_capabilities.state IN ('issued', 'export_ready') AND transfer_capabilities.expires_at > ? AND source.connection_mode = 'direct' ORDER BY transfer_capabilities.updated_at LIMIT 1")
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
            backup_id: None,
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
            let remote_time = reply.data["time"]
                .as_str()
                .and_then(|value| value.parse::<chrono::DateTime<Utc>>().ok());
            let clock_skew_seconds = remote_time.map(|value| (Utc::now() - value).num_seconds());
            sqlx::query(
                "UPDATE workers SET state = ?, status_json = ?, clock_skew_seconds = ?, last_seen_at = ?, updated_at = ? WHERE id = ?",
            )
            .bind(new_state)
            .bind(reply.data.to_string())
            .bind(clock_skew_seconds)
            .bind(Utc::now())
            .bind(Utc::now())
            .bind(worker_id)
            .execute(&state.database)
            .await
            .map_err(ApiError::internal)?;
            evaluate_worker_health(
                state,
                worker_id,
                &expected_role,
                &reply.data,
                clock_skew_seconds,
            )
            .await?;
            resolve_alert(state, &format!("worker-unreachable:{worker_id}")).await?;
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
            upsert_operational_alert(
                state,
                &format!("worker-unreachable:{worker_id}"),
                "critical",
                "Worker 无法访问",
                json!({"worker_id": worker_id, "error": error.to_string()}),
            )
            .await?;
            Err(error)
        }
    }
}

async fn evaluate_worker_health(
    state: &AppState,
    worker_id: &str,
    role: &str,
    status: &serde_json::Value,
    clock_skew_seconds: Option<i64>,
) -> Result<(), ApiError> {
    let available_percent = status["storage"]["available_percent"].as_u64();
    let disk_fingerprint = format!("worker-disk-low:{worker_id}");
    match available_percent {
        Some(percent) if percent < 15 => {
            upsert_operational_alert(
                state,
                &disk_fingerprint,
                if percent < 10 { "critical" } else { "warning" },
                "Worker 磁盘空间不足",
                json!({"worker_id": worker_id, "role": role, "available_percent": percent}),
            )
            .await?;
        }
        Some(_) => resolve_alert(state, &disk_fingerprint).await?,
        None => {}
    }
    let skew_fingerprint = format!("worker-clock-skew:{worker_id}");
    if clock_skew_seconds.is_some_and(|value| value.unsigned_abs() > 60) {
        upsert_operational_alert(
            state,
            &skew_fingerprint,
            "warning",
            "Worker 时钟偏差过大",
            json!({"worker_id": worker_id, "seconds": clock_skew_seconds}),
        )
        .await?;
    } else {
        resolve_alert(state, &skew_fingerprint).await?;
    }
    for (field, title) in [("cgroup_v2", "Worker 缺少 cgroup v2")] {
        let fingerprint = format!("worker-capability:{field}:{worker_id}");
        if status[field].as_bool() == Some(false) {
            upsert_operational_alert(
                state,
                &fingerprint,
                "critical",
                title,
                json!({"worker_id": worker_id, "role": role}),
            )
            .await?;
        } else {
            resolve_alert(state, &fingerprint).await?;
        }
    }
    if role == "publisher" {
        let backpressure = available_percent.is_some_and(|percent| percent < 10);
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('publication_backpressure', ?, ?) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at")
            .bind(json!(backpressure).to_string()).bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
        if state.config.external_archiver_enabled {
            let unarchived_bytes: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(artifacts.size), 0) FROM artifacts JOIN release_artifacts ON release_artifacts.artifact_sha256 = artifacts.sha256 JOIN releases ON releases.id = release_artifacts.release_id WHERE releases.state = 'committed' AND NOT EXISTS (SELECT 1 FROM archive_copies WHERE archive_copies.release_id = releases.id AND archive_copies.state = 'verified')")
                .fetch_one(&state.database).await.map_err(ApiError::internal)?;
            if unarchived_bytes > 20 * 1024 * 1024 * 1024_i64 {
                upsert_operational_alert(
                    state,
                    "publisher-unarchived-bytes",
                    "warning",
                    "Publisher 未归档数据超过 20 GiB",
                    json!({"unarchived_bytes": unarchived_bytes}),
                )
                .await?;
            } else {
                resolve_alert(state, "publisher-unarchived-bytes").await?;
            }
        } else {
            resolve_alert(state, "publisher-unarchived-bytes").await?;
        }
    }
    Ok(())
}

async fn upsert_operational_alert(
    state: &AppState,
    fingerprint: &str,
    severity: &str,
    title: &str,
    details: serde_json::Value,
) -> Result<(), ApiError> {
    let previous: Option<String> =
        sqlx::query_scalar("SELECT state FROM alerts WHERE fingerprint = ?")
            .bind(fingerprint)
            .fetch_optional(&state.database)
            .await
            .map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, ?, 'open', ?, ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET severity = excluded.severity, state = CASE WHEN alerts.state = 'resolved' THEN 'open' ELSE alerts.state END, title = excluded.title, details_json = excluded.details_json, resolved_at = NULL")
        .bind(Uuid::new_v4().to_string()).bind(fingerprint).bind(severity).bind(title)
        .bind(details.to_string()).bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
    if previous.as_deref().is_none_or(|value| value == "resolved") {
        tracing::warn!(%fingerprint, %severity, %title, "系统告警已打开");
    }
    Ok(())
}

async fn probe_all_workers(state: &AppState) -> Result<(), ApiError> {
    sqlx::query("UPDATE workers SET state = CASE WHEN last_seen_at < ? THEN 'offline' ELSE 'degraded' END, updated_at = ? WHERE connection_mode = 'reverse' AND state NOT IN ('draining', 'incompatible') AND (last_seen_at IS NULL OR last_seen_at < ?)")
        .bind(Utc::now() - Duration::minutes(5))
        .bind(Utc::now())
        .bind(Utc::now() - Duration::minutes(1))
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    let worker_ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM workers WHERE state != 'draining' AND connection_mode = 'direct'",
    )
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    for worker_id in worker_ids {
        let _ = probe_worker(state, &worker_id).await;
    }
    Ok(())
}

pub(crate) async fn lease_reverse_job(
    state: &AppState,
    worker_id: Uuid,
) -> Result<Option<SignedEnvelope>, ApiError> {
    if publication_backpressure(&state.database).await? {
        return Ok(None);
    }
    let worker = sqlx::query(
        "SELECT labels_json FROM workers WHERE id = ? AND role = 'builder' AND connection_mode = 'reverse' AND state = 'online'",
    )
    .bind(worker_id.to_string())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(worker) = worker else {
        return Ok(None);
    };
    if reverse_worker_has_active_job(&state.database, worker_id).await? {
        return Ok(None);
    }
    let labels: BTreeSet<String> =
        serde_json::from_str(worker.get("labels_json")).unwrap_or_default();
    let rows = sqlx::query(
        "SELECT id, batch_id, preferred_worker_id, required_labels_json FROM jobs WHERE required_role = 'builder' AND status IN ('queued', 'no_eligible_worker') AND (next_attempt_at IS NULL OR next_attempt_at <= ?) ORDER BY priority DESC, created_at",
    )
    .bind(Utc::now())
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let mut selected = None;
    for row in rows {
        let required_labels: BTreeSet<String> =
            serde_json::from_str(row.get("required_labels_json")).unwrap_or_default();
        let explicit_preference: Option<String> = row.get("preferred_worker_id");
        let batch_preference = if let Some(batch_id) = row.get::<Option<String>, _>("batch_id") {
            sqlx::query_scalar::<_, String>("SELECT worker_id FROM jobs WHERE batch_id = ? AND worker_id IS NOT NULL ORDER BY created_at LIMIT 1")
                .bind(batch_id).fetch_optional(&state.database).await.map_err(ApiError::internal)?
        } else {
            None
        };
        let preferred_worker = explicit_preference.or(batch_preference);
        let eligible = worker_matches_job(
            &worker_id.to_string(),
            &labels,
            &required_labels,
            preferred_worker.as_deref(),
        );
        if eligible {
            selected = Some(row);
            break;
        }
    }
    let Some(selected) = selected else {
        return Ok(None);
    };
    let job_id: String = selected.get("id");
    let envelope = dispatch_job_to_worker(state, &job_id, worker_id).await?;
    resolve_alert(state, &format!("no-eligible-worker:{job_id}")).await?;
    Ok(Some(envelope))
}

async fn reverse_worker_has_active_job(
    database: &sqlx::SqlitePool,
    worker_id: Uuid,
) -> Result<bool, ApiError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM jobs WHERE worker_id = ? AND status IN ('dispatched', 'running', 'uncertain')",
    )
    .bind(worker_id.to_string())
    .fetch_one(database)
    .await
    .map_err(ApiError::internal)?;
    Ok(count > 0)
}

pub(crate) async fn lease_reverse_transfer(
    state: &AppState,
    worker_id: Uuid,
) -> Result<Option<SignedEnvelope>, ApiError> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT envelope_json FROM transfer_capabilities WHERE source_worker_id = ? AND state = 'export_ready' AND expires_at > ? ORDER BY updated_at LIMIT 1",
    )
    .bind(worker_id.to_string())
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
        "SELECT envelope_json FROM transfer_capabilities WHERE source_worker_id = ? AND state = 'issued' AND expires_at > ? ORDER BY updated_at LIMIT 1",
    )
    .bind(worker_id.to_string())
    .bind(Utc::now())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(issued) = issued else {
        return Ok(None);
    };
    let value: String = issued.get("envelope_json");
    let envelope: SignedEnvelope = serde_json::from_str(&value).map_err(ApiError::internal)?;
    let capability: aursmith_protocol::TransferCapability = envelope
        .verify("aursmith.transfer_capability")
        .map_err(ApiError::internal)?;
    let publisher = sqlx::query(
        "SELECT endpoint FROM workers WHERE id = ? AND role = 'publisher' AND state = 'online'",
    )
    .bind(capability.destination_worker.to_string())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(publisher) = publisher else {
        return Ok(None);
    };
    transport::prepare_push_import(&state.config, publisher.get("endpoint"), &envelope).await?;
    sqlx::query("UPDATE transfer_capabilities SET state = 'export_ready', updated_at = ? WHERE id = ? AND state = 'issued'")
        .bind(Utc::now()).bind(capability.id.to_string())
        .execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(Some(envelope))
}

async fn dispatch_job_to_worker(
    state: &AppState,
    job_id: &str,
    worker_id: Uuid,
) -> Result<SignedEnvelope, ApiError> {
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
        required_role: parse_role(job.get("required_role"))?,
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
    let envelope = SignedEnvelope::sign("aursmith.job_spec", &spec, &state.signing_key)
        .map_err(ApiError::internal)?;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO attempts(id, job_id, generation, token_sha256, status) VALUES (?, ?, ?, ?, 'dispatched')")
        .bind(attempt_id.to_string())
        .bind(job_id)
        .bind(generation)
        .bind(&envelope.payload_sha256)
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    let updated = sqlx::query("UPDATE jobs SET worker_id = ?, status = 'dispatched', failure_code = NULL, next_attempt_at = NULL, signed_spec_json = ?, updated_at = ? WHERE id = ? AND status IN ('queued', 'no_eligible_worker')")
        .bind(worker_id.to_string())
        .bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
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
    Ok(envelope)
}

async fn dispatch_one(state: &AppState) -> Result<(), ApiError> {
    if publication_backpressure(&state.database).await? {
        return Ok(());
    }
    let job = sqlx::query(
        "SELECT id, batch_id, required_role, revision_sha256, kind, source_manifest_sha256, dependency_snapshot_sha256, preferred_worker_id, inputs_json, inline_inputs_json, expected_outputs_json, allow_check, required_labels_json, limits_json FROM jobs WHERE status IN ('queued', 'no_eligible_worker') AND (next_attempt_at IS NULL OR next_attempt_at <= ?) ORDER BY priority DESC, created_at LIMIT 1",
    )
    .bind(Utc::now())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(job) = job else { return Ok(()) };
    let job_id: String = job.get("id");
    let role: String = job.get("required_role");
    let required_labels: BTreeSet<String> =
        serde_json::from_str(job.get("required_labels_json")).map_err(ApiError::internal)?;
    let workers = sqlx::query(
        "SELECT id, endpoint, labels_json FROM workers WHERE role = ? AND state = 'online' AND protocol_version = ? AND connection_mode = 'direct' ORDER BY name",
    )
    .bind(&role)
    .bind(i64::from(aursmith_protocol::PROTOCOL_MAJOR))
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
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
        worker_matches_job(
            worker.get::<String, _>("id").as_str(),
            &labels,
            &required_labels,
            preferred_worker_id.as_deref(),
        )
    });
    let Some(worker) = selected else {
        let reverse_workers = if role == "builder" {
            sqlx::query("SELECT id, labels_json FROM workers WHERE role = 'builder' AND state = 'online' AND protocol_version = ? AND connection_mode = 'reverse'")
                .bind(i64::from(aursmith_protocol::PROTOCOL_MAJOR))
                .fetch_all(&state.database)
                .await
                .map_err(ApiError::internal)?
        } else {
            Vec::new()
        };
        let reverse_eligible = reverse_workers.iter().any(|worker| {
            let labels: BTreeSet<String> =
                serde_json::from_str(worker.get("labels_json")).unwrap_or_default();
            worker_matches_job(
                worker.get::<String, _>("id").as_str(),
                &labels,
                &required_labels,
                preferred_worker_id.as_deref(),
            )
        });
        if reverse_eligible {
            return Ok(());
        }
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
        dependency_attempt_ids: load_batch_dependency_attempts(state, &job_id).await?,
        dependencies: load_job_dependencies(state, &job_id).await?,
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
    sqlx::query("UPDATE jobs SET worker_id = ?, status = 'dispatched', failure_code = NULL, next_attempt_at = NULL, signed_spec_json = ?, updated_at = ? WHERE id = ?")
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

fn worker_matches_job(
    worker_id: &str,
    worker_labels: &BTreeSet<String>,
    required_labels: &BTreeSet<String>,
    preferred_worker_id: Option<&str>,
) -> bool {
    required_labels.is_subset(worker_labels)
        && preferred_worker_id.is_none_or(|preferred| preferred == worker_id)
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
        "SELECT jobs.id, jobs.kind, jobs.status AS controller_status, jobs.revision_id, jobs.revision_sha256, jobs.batch_id, revisions.upstream_version, workers.endpoint, workers.connection_mode, reverse_worker_reports.response_json FROM jobs JOIN workers ON workers.id = jobs.worker_id LEFT JOIN revisions ON revisions.id = jobs.revision_id LEFT JOIN reverse_worker_reports ON reverse_worker_reports.worker_id = workers.id AND reverse_worker_reports.job_id = jobs.id WHERE jobs.status IN ('uncertain', 'dispatched', 'running') AND (workers.connection_mode = 'reverse' OR jobs.status != 'uncertain' OR jobs.updated_at <= ?) ORDER BY jobs.updated_at LIMIT 1",
    )
    .bind(Utc::now() - Duration::minutes(30))
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let Some(row) = row else { return Ok(()) };
    let job_id: String = row.get("id");
    let connection_mode: String = row.get("connection_mode");
    let remote_reply = if connection_mode == "reverse" {
        row.get::<Option<String>, _>("response_json")
            .map(|value| serde_json::from_str::<transport::WorkerReply>(&value))
            .transpose()
            .map_err(ApiError::internal)?
    } else {
        None
    };
    let endpoint: String = row.get("endpoint");
    let reply = match if let Some(reply) = remote_reply {
        Ok(reply)
    } else if connection_mode == "reverse" {
        return Ok(());
    } else {
        transport::query(&state.config, &endpoint, &job_id).await
    } {
        Ok(reply) => reply,
        Err(error) if row.get::<String, _>("controller_status") == "uncertain" => {
            handle_uncertain_timeout(
                state,
                &job_id,
                row.get::<Option<String>, _>("batch_id"),
                &error.to_string(),
            )
            .await?;
            return Ok(());
        }
        Err(error) => {
            sqlx::query("UPDATE jobs SET status = 'uncertain', failure_code = 'DISPATCH_UNCERTAIN', updated_at = ? WHERE id = ? AND status IN ('dispatched', 'running')")
                .bind(Utc::now()).bind(&job_id).execute(&state.database).await.map_err(ApiError::internal)?;
            return Err(error);
        }
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
    if connection_mode == "reverse" {
        sqlx::query("DELETE FROM reverse_worker_reports WHERE worker_id = (SELECT worker_id FROM jobs WHERE id = ?) AND job_id = ?")
            .bind(&job_id)
            .bind(&job_id)
            .execute(&mut *transaction)
            .await
            .map_err(ApiError::internal)?;
    }
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

async fn handle_uncertain_timeout(
    state: &AppState,
    job_id: &str,
    batch_id: Option<String>,
    error: &str,
) -> Result<(), ApiError> {
    let attempt = sqlx::query(
        "SELECT id, generation FROM attempts WHERE job_id = ? ORDER BY generation DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let generation: i64 = attempt.get("generation");
    let retry = generation < 2;
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    sqlx::query("UPDATE attempts SET status = 'failed' WHERE id = ?")
        .bind(attempt.get::<String, _>("id"))
        .execute(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    sqlx::query("UPDATE jobs SET status = ?, failure_code = 'WORKER_UNREACHABLE', next_attempt_at = ?, updated_at = ? WHERE id = ? AND status = 'uncertain'")
        .bind(if retry { "queued" } else { "failed" })
        .bind(retry.then(|| Utc::now() + Duration::seconds(if generation == 0 { 5 } else { 10 })))
        .bind(Utc::now()).bind(job_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    if !retry && let Some(batch_id) = batch_id {
        sqlx::query("UPDATE release_batches SET state = 'build_failed', failure_reason = 'WORKER_UNREACHABLE', updated_at = ? WHERE id = ?")
            .bind(Utc::now()).bind(batch_id).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    if !retry {
        sqlx::query("INSERT INTO alerts(id, fingerprint, severity, state, title, details_json, opened_at) VALUES (?, ?, 'warning', 'open', 'Worker 任务状态无法确认', ?, ?) ON CONFLICT(fingerprint) DO UPDATE SET state = CASE WHEN alerts.state = 'resolved' THEN 'open' ELSE alerts.state END, details_json = excluded.details_json, resolved_at = NULL")
            .bind(Uuid::new_v4().to_string()).bind(format!("job-uncertain:{job_id}"))
            .bind(json!({"job_id": job_id, "attempt_generation": generation, "error": error}).to_string())
            .bind(Utc::now()).execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    transaction.commit().await.map_err(ApiError::internal)?;
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

    #[test]
    fn worker_eligibility_is_shared_by_direct_and_reverse_dispatch() {
        let labels = BTreeSet::from(["fast".to_string(), "x86_64".to_string()]);
        let required = BTreeSet::from(["x86_64".to_string()]);

        assert!(worker_matches_job(
            "builder-1",
            &labels,
            &required,
            Some("builder-1"),
        ));
        assert!(!worker_matches_job(
            "builder-1",
            &labels,
            &required,
            Some("builder-2"),
        ));
    }

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

        assert!(
            reverse_worker_has_active_job(&database, worker_id)
                .await
                .unwrap()
        );

        sqlx::query("UPDATE jobs SET status = 'succeeded' WHERE id = ?")
            .bind(job_id.to_string())
            .execute(&database)
            .await
            .unwrap();
        assert!(
            !reverse_worker_has_active_job(&database, worker_id)
                .await
                .unwrap()
        );
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

    #[test]
    fn receipt_manifest_comparison_ignores_order_but_rejects_duplicate_paths() {
        let entry = |path: &str, digest: &str| ManifestEntry {
            path: path.into(),
            sha256: digest.into(),
            size: 1,
        };
        let capability = vec![
            entry("controller.db", "a"),
            entry("backup-envelope.json", "b"),
        ];
        let receipt = vec![
            entry("backup-envelope.json", "b"),
            entry("controller.db", "a"),
        ];
        assert!(manifest_entries_match(&capability, &receipt));

        let duplicates = vec![entry("controller.db", "a"), entry("controller.db", "a")];
        assert!(!manifest_entries_match(&duplicates, &duplicates));
    }

    fn state(database: sqlx::SqlitePool) -> AppState {
        AppState::new(
            database,
            crate::config::Config {
                bind_address: "127.0.0.1:0".into(),
                database_url: "sqlite::memory:".into(),
                public_origin: "https://aursmith.test".into(),
                signing_key_file: "/不存在".into(),
                ssh_identity_source_file: "/不存在".into(),
                ssh_identity_file: "/不存在".into(),
                ssh_known_hosts_file: "/不存在".into(),
                session_idle_minutes: 30,
                session_absolute_hours: 1,
                low_agent_endpoints: vec![],
                high_agent_endpoint: String::new(),
                agent_daily_call_limit: 300,
                agent_monthly_call_limit: 3000,
                agent_monthly_cost_limit_microusd: 5_000_000,
                agent_random_high_cost_review_basis_points: 0,
                repository_name: "aursmith".into(),
                source_git_commit: "test".into(),
                repository_base_url: "https://repo.test".into(),
                client_ca_certificate_file: None,
                webhook_url: None,
                webhook_hmac_secret_file: "/不存在".into(),
                ntfy_url: None,
                backup_dir: "/不存在".into(),
                backup_export_dir: "/不存在".into(),
                backup_export_socket: "/不存在".into(),
                external_archiver_enabled: false,
            },
            ed25519_dalek::SigningKey::from_bytes(&[7_u8; 32]),
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
        sqlx::query("INSERT INTO releases(id, batch_id, state, manifest_sha256, source_git_commit, writer_epoch, committed_at, created_at) VALUES (?, ?, 'committed', ?, 'test', 1, ?, ?), (?, ?, 'committed', ?, 'test', 1, ?, ?)")
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
    async fn queued_removal_creates_an_empty_signed_authorization() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let state = state(database.clone());
        let old_batch = Uuid::new_v4().to_string();
        let removal_batch = Uuid::new_v4().to_string();
        let old_release = Uuid::new_v4().to_string();
        let old_job = Uuid::new_v4().to_string();
        let worker = Uuid::new_v4().to_string();
        let revision = Uuid::new_v4().to_string();
        let now = Utc::now();
        sqlx::query("INSERT INTO workers(id, name, role, state, endpoint, ssh_host_key_sha256, protocol_version, labels_json, writer_epoch, created_at, updated_at) VALUES (?, 'publisher', 'publisher', 'online', 'ssh://aursmith@publisher:2222', ?, 1, '[]', 1, ?, ?)")
            .bind(&worker).bind("a".repeat(64)).bind(now).bind(now).execute(&database).await.unwrap();
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
        sqlx::query("INSERT INTO releases(id, batch_id, state, manifest_sha256, source_git_commit, writer_epoch, committed_at, created_at) VALUES (?, ?, 'committed', ?, 'test', 1, ?, ?)")
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

        let envelope_json: String =
            sqlx::query_scalar("SELECT envelope_json FROM release_authorizations")
                .fetch_one(&database)
                .await
                .unwrap();
        let envelope: SignedEnvelope = serde_json::from_str(&envelope_json).unwrap();
        let authorization: ReleaseAuthorization =
            envelope.verify("aursmith.release_authorization").unwrap();
        assert!(authorization.artifacts.is_empty());
        assert_eq!(
            authorization.removed_package_names,
            ["demo-cli", "demo-lib"]
        );
        assert_eq!(authorization.evidence.schema_version, 1);
        assert!(
            authorization
                .evidence
                .records
                .iter()
                .any(|record| record.kind == "release_batch" && record.identity == removal_batch)
        );
    }

    #[tokio::test]
    async fn uncertain_job_retries_twice_then_alerts() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let state = state(database.clone());
        let job_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        sqlx::query("INSERT INTO jobs(id, required_role, status, priority, kind, inputs_json, inline_inputs_json, required_labels_json, created_at, updated_at) VALUES (?, 'builder', 'uncertain', 1, 'build', '[]', '[]', '[]', ?, ?)")
            .bind(&job_id).bind(now).bind(now).execute(&database).await.unwrap();
        for generation in 0..=2 {
            sqlx::query("INSERT INTO attempts(id, job_id, generation, token_sha256, status) VALUES (?, ?, ?, ?, 'dispatched')")
                .bind(Uuid::new_v4().to_string()).bind(&job_id).bind(generation)
                .bind(hex::encode(Sha256::digest(format!("token-{generation}"))))
                .execute(&database).await.unwrap();
            sqlx::query(
                "UPDATE jobs SET status = 'uncertain', next_attempt_at = NULL WHERE id = ?",
            )
            .bind(&job_id)
            .execute(&database)
            .await
            .unwrap();
            handle_uncertain_timeout(&state, &job_id, None, "ssh timeout")
                .await
                .unwrap();
            let row = sqlx::query("SELECT status, next_attempt_at FROM jobs WHERE id = ?")
                .bind(&job_id)
                .fetch_one(&database)
                .await
                .unwrap();
            if generation < 2 {
                assert_eq!(row.get::<String, _>("status"), "queued");
                assert!(row.get::<Option<String>, _>("next_attempt_at").is_some());
            } else {
                assert_eq!(row.get::<String, _>("status"), "failed");
            }
        }
        let alerts: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM alerts WHERE fingerprint = ?")
            .bind(format!("job-uncertain:{job_id}"))
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(alerts, 1);
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
