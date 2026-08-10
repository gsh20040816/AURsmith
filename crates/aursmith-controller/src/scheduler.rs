use crate::{error::ApiError, routes::AppState, transport};
use aursmith_domain::AttemptRef;
use aursmith_protocol::{JobKind, JobSpec, ResourceLimits, SignedEnvelope};
use chrono::{Duration, Utc};
use serde_json::json;
use sqlx::Row;
use std::collections::BTreeSet;
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
        }
    });
}

pub async fn probe_worker(state: &AppState, worker_id: &str) -> Result<String, ApiError> {
    let row = sqlx::query("SELECT endpoint, role FROM workers WHERE id = ?")
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
            let expected_role: String = row.get("role");
            let new_state = if protocol != u64::from(aursmith_protocol::PROTOCOL_MAJOR)
                || remote_role != expected_role
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
        "SELECT id, required_role, revision_sha256, kind, profile_sha256, source_manifest_sha256, dependency_snapshot_sha256, inputs_json, inline_inputs_json, required_labels_json, limits_json FROM jobs WHERE status IN ('queued', 'no_eligible_worker') ORDER BY priority DESC, created_at LIMIT 1",
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
    let selected = workers.into_iter().find(|worker| {
        let labels: BTreeSet<String> =
            serde_json::from_str(worker.get("labels_json")).unwrap_or_default();
        let profiles: BTreeSet<String> =
            serde_json::from_str(worker.get("profiles_json")).unwrap_or_default();
        required_labels.is_subset(&labels)
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
        "SELECT jobs.id, jobs.kind, jobs.profile_sha256, workers.endpoint FROM jobs JOIN workers ON workers.id = jobs.worker_id WHERE jobs.status IN ('uncertain', 'dispatched', 'running') ORDER BY jobs.updated_at LIMIT 1",
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
        "fetch" => Ok(JobKind::Fetch),
        "build" => Ok(JobKind::Build),
        "profile_fixture" => Ok(JobKind::ProfileFixture),
        _ => Err(ApiError::internal("数据库包含未知 Job 类型")),
    }
}
