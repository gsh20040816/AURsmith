use crate::{auth, config::Config, error::ApiError, transport};
use aursmith_domain::{REQUIREMENTS, WorkerRole, WorkerState};
use aursmith_protocol::{InlineInput, JobKind, ManifestEntry, ResourceLimits};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::Utc;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub database: SqlitePool,
    pub config: Arc<Config>,
    pub signing_key: Arc<SigningKey>,
}

impl AppState {
    pub fn new(database: SqlitePool, config: Config, signing_key: SigningKey) -> Self {
        Self {
            database,
            config: Arc::new(config),
            signing_key: Arc::new(signing_key),
        }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup", post(setup))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/requirements", get(requirements))
        .route("/api/v1/settings", get(settings).put(update_settings))
        .route("/api/v1/client-bootstrap", get(client_bootstrap))
        .route("/api/v1/doctor", get(doctor_status))
        .route("/api/v1/metrics", get(metrics_status))
        .route("/api/v1/alerts", get(list_alerts))
        .route("/api/v1/alerts/{id}/acknowledge", post(acknowledge_alert))
        .route("/api/v1/backups", get(list_backups).post(create_backup))
        .route("/api/v1/backups/{id}/verify", post(verify_backup))
        .route("/api/v1/archive-inventories", get(list_archive_inventories))
        .route(
            "/api/v1/rebuild-recommendations",
            get(list_rebuild_recommendations),
        )
        .route(
            "/api/v1/rebuild-recommendations/{package_base}/disable",
            post(disable_rebuild_recommendation),
        )
        .route(
            "/api/v1/rebuild-recommendations/{package_base}/schedule",
            post(schedule_rebuild_recommendation),
        )
        .route("/api/v1/workers", get(list_workers).post(register_worker))
        .route("/api/v1/workers/{id}/drain", post(drain_worker))
        .route("/api/v1/workers/{id}/probe", post(probe_worker))
        .route("/api/v1/jobs", get(list_jobs).post(create_job))
        .route(
            "/api/v1/profiles",
            get(crate::profiles::list).post(crate::profiles::authorize),
        )
        .route(
            "/api/v1/profiles/{id}/activate",
            post(crate::profiles::activate),
        )
        .route(
            "/api/v1/profile-recommendations",
            get(crate::profiles::recommendations),
        )
        .route("/api/v1/audits", get(crate::audits::list))
        .route(
            "/api/v1/audits/{bundle}/manual-decision",
            post(crate::audits::manual_decision),
        )
        .route("/api/v1/aur/search", get(crate::packages::search))
        .route(
            "/api/v1/subscriptions",
            get(crate::packages::list_subscriptions).post(crate::packages::subscribe),
        )
        .route(
            "/api/v1/subscriptions/{package_base}/pause",
            post(crate::packages::pause),
        )
        .route(
            "/api/v1/subscriptions/{package_base}/resume",
            post(crate::packages::resume),
        )
        .route(
            "/api/v1/subscriptions/{package_base}/unsubscribe",
            post(crate::packages::unsubscribe),
        )
        .route(
            "/api/v1/subscriptions/{package_base}/purge",
            post(crate::packages::purge),
        )
        .route(
            "/api/v1/packages/{package_base}",
            get(crate::packages::package_detail),
        )
        .route(
            "/api/v1/packages/{package_base}/refresh",
            post(crate::packages::refresh_package),
        )
        .route(
            "/api/v1/packages/{package_base}/build-policy",
            post(crate::packages::set_build_policy),
        )
        .route(
            "/api/v1/packages/{package_base}/providers/{dependency_name}",
            post(crate::packages::select_provider),
        )
        .route(
            "/api/v1/release-batches",
            get(crate::packages::list_batches),
        )
        .route("/api/v1/releases", get(crate::packages::list_releases))
        .route(
            "/api/v1/releases/{id}/rollback",
            post(crate::packages::rollback_release),
        )
        .route("/api/v1/archives", get(crate::packages::list_archives))
        .with_state(state)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::new(
            axum::http::HeaderName::from_static("x-request-id"),
            MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http())
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "controller", "version": env!("CARGO_PKG_VERSION")}))
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsRequest {
    agent_daily_call_limit: i64,
    agent_monthly_call_limit: i64,
    agent_monthly_cost_limit_microusd: i64,
}

async fn settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let daily_limit = effective_i64_setting(
        &state,
        "agent_daily_call_limit",
        state.config.agent_daily_call_limit,
    )
    .await?;
    let monthly_limit = effective_i64_setting(
        &state,
        "agent_monthly_call_limit",
        state.config.agent_monthly_call_limit,
    )
    .await?;
    let cost_limit = effective_i64_setting(
        &state,
        "agent_monthly_cost_limit_microusd",
        state.config.agent_monthly_cost_limit_microusd,
    )
    .await?;
    let daily_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_runs WHERE started_at >= datetime('now', 'start of day')",
    )
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let monthly_used: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM agent_runs WHERE started_at >= datetime('now', 'start of month')",
    )
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let monthly_cost: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(cost_microusd), 0) FROM agent_runs WHERE started_at >= datetime('now', 'start of month')")
        .fetch_one(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({
        "agents": {
            "supported_adapters": ["codex", "claude_code"],
            "low_runner_count": state.config.low_agent_endpoints.len(),
            "high_runner_configured": !state.config.high_agent_endpoint.is_empty(),
            "configuration_source": "docker_compose_environment_and_secrets",
            "api_keys_exposed": false
        },
        "budget": {
            "agent_daily_call_limit": daily_limit,
            "agent_monthly_call_limit": monthly_limit,
            "agent_monthly_cost_limit_microusd": cost_limit,
            "daily_used": daily_used,
            "monthly_used": monthly_used,
            "monthly_cost_microusd": monthly_cost
        },
        "notifications": {
            "webhook_configured": state.config.webhook_url.is_some(),
            "ntfy_configured": state.config.ntfy_url.is_some()
        },
        "repository": {
            "name": state.config.repository_name,
            "base_url": state.config.repository_base_url,
            "publisher_compatibility_days": 30
        }
    })))
}

async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UpdateSettingsRequest>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let values = [
        ("agent_daily_call_limit", request.agent_daily_call_limit),
        ("agent_monthly_call_limit", request.agent_monthly_call_limit),
        (
            "agent_monthly_cost_limit_microusd",
            request.agent_monthly_cost_limit_microusd,
        ),
    ];
    if values
        .iter()
        .any(|(_, value)| !(0..=1_000_000_000).contains(value))
    {
        return Err(ApiError::bad_request(
            "INVALID_AGENT_BUDGET",
            "Agent 调用与成本限制必须位于 0 到 1000000000",
        ));
    }
    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    for (key, value) in values {
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES (?, ?, ?) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at")
            .bind(key).bind(json!(value).to_string()).bind(Utc::now())
            .execute(&mut *transaction).await.map_err(ApiError::internal)?;
    }
    append_event_in_transaction(
        &mut transaction,
        "system_settings",
        "agent_budget",
        "agent_budget_changed",
        json!({
            "daily": request.agent_daily_call_limit,
            "monthly": request.agent_monthly_call_limit,
            "monthly_cost_microusd": request.agent_monthly_cost_limit_microusd
        }),
        &actor,
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    settings(State(state), headers).await
}

pub(crate) async fn effective_i64_setting(
    state: &AppState,
    key: &str,
    default: i64,
) -> Result<i64, ApiError> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value_json FROM system_settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&state.database)
            .await
            .map_err(ApiError::internal)?;
    value.map_or(Ok(default), |value| {
        serde_json::from_str(&value).map_err(ApiError::internal)
    })
}

async fn client_bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let value: String = sqlx::query_scalar(
        "SELECT value_json FROM system_settings WHERE key = 'repository_gpg_fingerprint'",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| {
        ApiError::conflict(
            "REPOSITORY_KEY_NOT_READY",
            "请先注册并验证 Publisher 仓库 GPG 指纹",
        )
    })?;
    let fingerprint: String = serde_json::from_str(&value).map_err(ApiError::internal)?;
    let base = state.config.repository_base_url.trim_end_matches('/');
    let repository_config =
        format!("[aursmith]\nSigLevel = Required DatabaseRequired\nServer = {base}/$arch");
    Ok(Json(json!({
        "repository_config": repository_config,
        "gpg_fingerprint": fingerprint,
        "gpg_key_url": format!("{base}/x86_64/aursmith-repository-key.asc"),
        "commands": [
            format!("curl --fail --output /tmp/aursmith-repository-key.asc '{base}/x86_64/aursmith-repository-key.asc'"),
            "sudo pacman-key --add /tmp/aursmith-repository-key.asc".to_owned(),
            format!("sudo pacman-key --lsign-key {fingerprint}"),
            "sudo pacman -Syu".to_owned(),
        ],
        "warnings": [
            "执行导入前必须人工核对页面显示的完整 GPG 指纹。",
            "AURsmith 仓库必须放在官方仓库之后。",
            "内部 CA 证书需由管理员通过首次设置页面导出并安装。",
        ]
    })))
}

async fn doctor_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let workers = sqlx::query("SELECT id, name, role, state, status_json, clock_skew_seconds, last_seen_at FROM workers ORDER BY role, name")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let mut checks = Vec::new();
    for role in ["builder", "publisher", "archiver"] {
        let online = workers
            .iter()
            .filter(|row| {
                row.get::<String, _>("role") == role && row.get::<String, _>("state") == "online"
            })
            .count();
        checks.push(json!({"id": format!("worker-{role}"), "ok": online > 0, "message": format!("{role} 在线实例：{online}")}));
    }
    for row in &workers {
        let status = row
            .get::<Option<String>, _>("status_json")
            .and_then(|value| serde_json::from_str::<Value>(&value).ok())
            .unwrap_or(Value::Null);
        let available = status["storage"]["available_percent"].as_u64();
        let skew = row.get::<Option<i64>, _>("clock_skew_seconds");
        checks.push(json!({
            "id": format!("worker-health-{}", row.get::<String,_>("id")),
            "ok": row.get::<String,_>("state") == "online"
                && available.is_none_or(|value| value >= 10)
                && skew.is_none_or(|value| value.unsigned_abs() <= 60),
            "message": format!("{}：状态 {}，可用空间 {}%，时钟偏差 {} 秒", row.get::<String,_>("name"), row.get::<String,_>("state"), available.map(|value| value.to_string()).unwrap_or_else(|| "未知".into()), skew.map(|value| value.to_string()).unwrap_or_else(|| "未知".into())),
        }));
    }
    let profile_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM build_profiles WHERE state = 'active' AND last_verified_at IS NOT NULL")
        .fetch_one(&state.database).await.map_err(ApiError::internal)?;
    checks.push(json!({"id": "active-profile", "ok": profile_count > 0, "message": format!("已验证活跃 Profile：{profile_count}")}));
    let fingerprint_ready: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM system_settings WHERE key = 'repository_gpg_fingerprint'",
    )
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    checks.push(json!({"id": "repository-gpg", "ok": fingerprint_ready == 1, "message": "仓库 GPG 指纹已由 Publisher 固定"}));
    checks.push(json!({"id": "agent-low", "ok": state.config.low_agent_endpoints.len() == 3, "message": format!("低成本 Agent Runner：{}", state.config.low_agent_endpoints.len())}));
    checks.push(json!({"id": "agent-high", "ok": !state.config.high_agent_endpoint.is_empty(), "message": "高成本 Agent Runner 已配置"}));
    let ready = checks.iter().all(|check| check["ok"] == true);
    Ok(Json(
        json!({"ready": ready, "checked_at": Utc::now(), "checks": checks}),
    ))
}

async fn metrics_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let queue = sqlx::query("SELECT status, COUNT(*) AS count FROM jobs GROUP BY status")
        .fetch_all(&state.database)
        .await
        .map_err(ApiError::internal)?;
    let stages = sqlx::query("SELECT jobs.kind, COUNT(*) AS count, CAST(AVG((julianday(attempts.finished_at) - julianday(attempts.started_at)) * 86400000) AS INTEGER) AS average_milliseconds FROM attempts JOIN jobs ON jobs.id = attempts.job_id WHERE attempts.status = 'succeeded' AND attempts.started_at IS NOT NULL AND attempts.finished_at IS NOT NULL GROUP BY jobs.kind")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    let agent = sqlx::query("SELECT COUNT(*) AS calls, COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0) AS failures, COALESCE(SUM(cost_microusd), 0) AS cost_microusd FROM agent_runs")
        .fetch_one(&state.database).await.map_err(ApiError::internal)?;
    let dependencies = sqlx::query("SELECT COUNT(*) AS observations, COALESCE(SUM(cache_hit), 0) AS cache_hits, COALESCE(SUM(download_bytes), 0) AS download_bytes, COALESCE(SUM(download_milliseconds), 0) AS download_milliseconds FROM dependency_observations")
        .fetch_one(&state.database).await.map_err(ApiError::internal)?;
    let archives = sqlx::query("SELECT COUNT(*) AS copies, COALESCE(SUM(CASE WHEN state = 'verified' THEN 1 ELSE 0 END), 0) AS verified, COALESCE(SUM(CASE WHEN state = 'failed' THEN 1 ELSE 0 END), 0) AS failed FROM archive_copies")
        .fetch_one(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({
        "queue": queue.into_iter().map(|row| (row.get::<String,_>("status"), row.get::<i64,_>("count"))).collect::<BTreeMap<_,_>>(),
        "stage_durations": stages.into_iter().map(|row| json!({"kind": row.get::<String,_>("kind"), "count": row.get::<i64,_>("count"), "average_milliseconds": row.get::<Option<i64>,_>("average_milliseconds")})).collect::<Vec<_>>(),
        "agent": {"calls": agent.get::<i64,_>("calls"), "failures": agent.get::<i64,_>("failures"), "cost_microusd": agent.get::<i64,_>("cost_microusd")},
        "dependencies": {"observations": dependencies.get::<i64,_>("observations"), "cache_hits": dependencies.get::<i64,_>("cache_hits"), "download_bytes": dependencies.get::<i64,_>("download_bytes"), "download_milliseconds": dependencies.get::<i64,_>("download_milliseconds")},
        "archives": {"copies": archives.get::<i64,_>("copies"), "verified": archives.get::<i64,_>("verified"), "failed": archives.get::<i64,_>("failed")},
        "generated_at": Utc::now(),
    })))
}

async fn list_alerts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query("SELECT id, fingerprint, severity, state, title, details_json, opened_at, acknowledged_at, resolved_at FROM alerts ORDER BY CASE state WHEN 'open' THEN 0 WHEN 'acknowledged' THEN 1 ELSE 2 END, opened_at DESC LIMIT 500")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"items": rows.into_iter().map(|row| json!({
        "id": row.get::<String,_>("id"), "fingerprint": row.get::<String,_>("fingerprint"),
        "severity": row.get::<String,_>("severity"), "state": row.get::<String,_>("state"),
        "title": row.get::<String,_>("title"),
        "details": serde_json::from_str::<Value>(row.get("details_json")).unwrap_or(Value::Null),
        "opened_at": row.get::<String,_>("opened_at"),
        "acknowledged_at": row.get::<Option<String>,_>("acknowledged_at"),
        "resolved_at": row.get::<Option<String>,_>("resolved_at"),
    })).collect::<Vec<_>>() })))
}

async fn acknowledge_alert(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let result = sqlx::query("UPDATE alerts SET state = 'acknowledged', acknowledged_at = ? WHERE id = ? AND state = 'open'")
        .bind(Utc::now()).bind(&id).execute(&state.database).await.map_err(ApiError::internal)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::conflict("ALERT_NOT_OPEN", "告警不存在或已经处理"));
    }
    append_event(
        &state.database,
        "alert",
        &id,
        "alert_acknowledged",
        json!({}),
        &actor,
    )
    .await?;
    Ok(Json(json!({"id": id, "state": "acknowledged"})))
}

async fn list_backups(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    Ok(Json(crate::backups::list(&state.database).await?))
}

async fn create_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let result = crate::backups::create(&state).await?;
    append_event(
        &state.database,
        "control_plane_backup",
        result["id"].as_str().unwrap_or("unknown"),
        "control_plane_backup_created",
        result.clone(),
        &actor,
    )
    .await?;
    Ok(Json(result))
}

async fn verify_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let result = crate::backups::verify_record(&state, &id).await?;
    append_event(
        &state.database,
        "control_plane_backup",
        &id,
        "control_plane_backup_verified",
        result.clone(),
        &actor,
    )
    .await?;
    Ok(Json(result))
}

async fn list_archive_inventories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query("SELECT archive_inventories.id, archive_inventories.full_digest, archive_inventories.release_count, archive_inventories.backup_count, archive_inventories.file_count, archive_inventories.byte_count, archive_inventories.failure_count, archive_inventories.checked_at, workers.name FROM archive_inventories JOIN workers ON workers.id = archive_inventories.archiver_worker_id ORDER BY checked_at DESC LIMIT 100")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"items": rows.into_iter().map(|row| json!({
        "id": row.get::<String,_>("id"), "archiver_name": row.get::<String,_>("name"),
        "full_digest": row.get::<bool,_>("full_digest"), "release_count": row.get::<i64,_>("release_count"),
        "backup_count": row.get::<i64,_>("backup_count"),
        "file_count": row.get::<i64,_>("file_count"), "byte_count": row.get::<i64,_>("byte_count"),
        "failure_count": row.get::<i64,_>("failure_count"), "checked_at": row.get::<String,_>("checked_at"),
    })).collect::<Vec<_>>() })))
}

async fn list_rebuild_recommendations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query("SELECT package_base, state, reason, changes_json, detected_at, updated_at FROM rebuild_recommendations ORDER BY CASE state WHEN 'suggested' THEN 0 WHEN 'disabled' THEN 1 ELSE 2 END, updated_at DESC")
        .fetch_all(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(json!({"items": rows.into_iter().map(|row| json!({
        "package_base": row.get::<String,_>("package_base"), "state": row.get::<String,_>("state"),
        "reason": row.get::<String,_>("reason"),
        "changes": serde_json::from_str::<Value>(row.get("changes_json")).unwrap_or_else(|_| json!([])),
        "detected_at": row.get::<String,_>("detected_at"), "updated_at": row.get::<String,_>("updated_at")
    })).collect::<Vec<_>>() })))
}

async fn disable_rebuild_recommendation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let result = sqlx::query("UPDATE rebuild_recommendations SET state = 'disabled', updated_at = ? WHERE package_base = ? AND state = 'suggested'")
        .bind(Utc::now()).bind(&package_base).execute(&state.database).await.map_err(ApiError::internal)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "REBUILD_NOT_SUGGESTED",
            "该软件包没有可关闭的重建建议",
        ));
    }
    append_event(
        &state.database,
        "package_base",
        &package_base,
        "official_dependency_rebuild_disabled",
        json!({}),
        &actor,
    )
    .await?;
    Ok(Json(
        json!({"package_base": package_base, "state": "disabled"}),
    ))
}

async fn schedule_rebuild_recommendation(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rebuild_recommendations WHERE package_base = ? AND state = 'suggested'")
        .bind(&package_base).fetch_one(&state.database).await.map_err(ApiError::internal)?;
    if exists == 0 {
        return Err(ApiError::conflict(
            "REBUILD_NOT_SUGGESTED",
            "该软件包没有可调度的重建建议",
        ));
    }
    let batch_id = crate::packages::schedule_rebuild_batch(
        &state.database,
        BTreeSet::from([package_base.clone()]),
        &actor,
    )
    .await?
    .ok_or_else(|| ApiError::internal("重建批次未创建"))?;
    sqlx::query("UPDATE rebuild_recommendations SET state = 'scheduled', updated_at = ? WHERE package_base = ?")
        .bind(Utc::now()).bind(&package_base).execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(Json(
        json!({"package_base": package_base, "state": "scheduled", "batch_id": batch_id}),
    ))
}

async fn setup_status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM administrators")
        .fetch_one(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({"initialized": count > 0})))
}

#[derive(Deserialize)]
struct SetupRequest {
    token: String,
    username: String,
    password: String,
}

async fn setup(
    State(state): State<AppState>,
    Json(request): Json<SetupRequest>,
) -> Result<impl IntoResponse, ApiError> {
    if request.username.trim().len() < 3 || request.password.len() < 12 {
        return Err(ApiError::bad_request(
            "INVALID_CREDENTIALS",
            "用户名至少 3 个字符，密码至少 12 个字符",
        ));
    }
    let expected = Sha256::digest(state.config.setup_token.as_bytes());
    let actual = Sha256::digest(request.token.as_bytes());
    if expected.as_slice() != actual.as_slice() {
        return Err(ApiError::unauthorized("初始化令牌无效"));
    }

    let mut transaction = state.database.begin().await.map_err(ApiError::internal)?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM administrators")
        .fetch_one(&mut *transaction)
        .await
        .map_err(ApiError::internal)?;
    if count != 0 {
        return Err(ApiError::conflict(
            "ALREADY_INITIALIZED",
            "系统已经完成初始化",
        ));
    }
    let administrator_id = Uuid::new_v4().to_string();
    let password_hash = auth::hash_password(&request.password)?;
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO administrators(id, username, password_hash, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&administrator_id)
    .bind(request.username.trim())
    .bind(password_hash)
    .bind(now)
    .execute(&mut *transaction)
    .await
    .map_err(ApiError::internal)?;
    append_event_in_transaction(
        &mut transaction,
        "system",
        "singleton",
        "administrator_initialized",
        json!({"administrator_id": administrator_id}),
        "setup",
    )
    .await?;
    transaction.commit().await.map_err(ApiError::internal)?;
    Ok((StatusCode::CREATED, Json(json!({"initialized": true}))))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let row =
        sqlx::query("SELECT id, username, password_hash FROM administrators WHERE username = ?")
            .bind(request.username.trim())
            .fetch_optional(&state.database)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::unauthorized("用户名或密码错误"))?;
    let password_hash: String = row.get("password_hash");
    if !auth::verify_password(&request.password, &password_hash) {
        return Err(ApiError::unauthorized("用户名或密码错误"));
    }
    let administrator_id: String = row.get("id");
    let token = auth::create_session(&state, &administrator_id).await?;
    let cookie = auth::session_cookie(
        &token,
        state.config.secure_cookies,
        state.config.session_hours * 3600,
    );
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok((
        headers,
        Json(json!({"username": row.get::<String, _>("username")})),
    ))
}

async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(cookie) = headers
        .get(axum::http::header::COOKIE)
        .and_then(|value| value.to_str().ok())
    {
        if let Some(token) = cookie
            .split(';')
            .filter_map(|part| part.trim().split_once('='))
            .find_map(|(key, value)| (key == "aursmith_session").then_some(value))
        {
            sqlx::query("DELETE FROM sessions WHERE token_sha256 = ?")
                .bind(auth::sha256(token))
                .execute(&state.database)
                .await
                .map_err(ApiError::internal)?;
        }
    }
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&auth::expired_session_cookie(state.config.secure_cookies))
            .map_err(ApiError::internal)?,
    );
    Ok((response_headers, StatusCode::NO_CONTENT))
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    let id = auth::require_administrator(&state, &headers).await?;
    let username: String = sqlx::query_scalar("SELECT username FROM administrators WHERE id = ?")
        .bind(&id)
        .fetch_one(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({"id": id, "username": username})))
}

async fn requirements(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    Ok(Json(json!({"items": REQUIREMENTS})))
}

#[derive(Debug, Deserialize)]
struct RegisterWorkerRequest {
    name: String,
    role: WorkerRole,
    endpoint: String,
    ssh_host_key_sha256: String,
    protocol_version: u16,
    #[serde(default)]
    labels: Vec<String>,
}

#[derive(Debug, Serialize)]
struct WorkerResponse {
    id: String,
    name: String,
    role: String,
    state: String,
    endpoint: String,
    protocol_version: i64,
    labels: Vec<String>,
    last_seen_at: Option<String>,
    storage: Option<Value>,
    clock_skew_seconds: Option<i64>,
}

async fn register_worker(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterWorkerRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let administrator_id = auth::require_administrator(&state, &headers).await?;
    if request.name.trim().is_empty() || request.endpoint.trim().is_empty() {
        return Err(ApiError::bad_request(
            "INVALID_WORKER",
            "Worker 名称和端点不能为空",
        ));
    }
    let role = role_name(request.role);
    let remote = transport::status(&state.config, request.endpoint.trim()).await?;
    let id = remote.data["instance_id"]
        .as_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| ApiError::conflict("WORKER_ID_MISSING", "Worker 没有报告有效实例 ID"))?
        .to_string();
    let identity_signing_key_hex = remote.data["identity_signing_key_hex"]
        .as_str()
        .filter(|value| {
            value.len() == 64 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .ok_or_else(|| {
            ApiError::conflict("WORKER_SIGNING_KEY_MISSING", "Worker 没有报告有效身份公钥")
        })?;
    if remote.data["name"].as_str() != Some(request.name.trim())
        || remote.data["role"].as_str() != Some(role)
        || remote.data["protocol_major"].as_u64() != Some(u64::from(request.protocol_version))
        || (role == "publisher" && remote.data["writer_epoch"].as_u64() != Some(0))
    {
        return Err(ApiError::conflict(
            "WORKER_IDENTITY_MISMATCH",
            "Worker 报告的名称、角色或协议与注册请求不一致",
        ));
    }
    let repository_fingerprint = if role == "publisher" {
        Some(
            remote.data["repository_gpg_fingerprint"]
                .as_str()
                .filter(|value| {
                    value.len() == 40
                        && value.chars().all(|character| character.is_ascii_hexdigit())
                })
                .ok_or_else(|| {
                    ApiError::conflict(
                        "GPG_FINGERPRINT_MISSING",
                        "Publisher 没有报告有效仓库 GPG 指纹",
                    )
                })?,
        )
    } else {
        None
    };
    let now = Utc::now();
    let labels_json = serde_json::to_string(&request.labels).map_err(ApiError::internal)?;
    sqlx::query(
        "INSERT INTO workers(id, name, role, state, endpoint, ssh_host_key_sha256, protocol_version, labels_json, identity_signing_key_hex, created_at, updated_at) VALUES (?, ?, ?, 'offline', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(request.name.trim())
    .bind(role)
    .bind(request.endpoint.trim())
    .bind(request.ssh_host_key_sha256.trim())
    .bind(i64::from(request.protocol_version))
    .bind(labels_json)
    .bind(identity_signing_key_hex)
    .bind(now)
    .bind(now)
    .execute(&state.database)
    .await
    .map_err(|error| {
        if error.to_string().contains("UNIQUE constraint failed") {
            ApiError::conflict("WORKER_EXISTS", "Worker 名称已经存在")
        } else {
            ApiError::internal(error)
        }
    })?;
    if let Some(fingerprint) = repository_fingerprint {
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('repository_gpg_fingerprint', ?, ?) ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at")
            .bind(json!(fingerprint).to_string()).bind(Utc::now()).execute(&state.database).await.map_err(ApiError::internal)?;
    }
    append_event(
        &state.database,
        "worker",
        &id,
        "worker_registered",
        json!({"name": request.name, "role": role}),
        &administrator_id,
    )
    .await?;
    Ok((StatusCode::CREATED, Json(json!({"id": id}))))
}

async fn list_workers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query(
        "SELECT id, name, role, state, endpoint, protocol_version, labels_json, status_json, clock_skew_seconds, last_seen_at FROM workers ORDER BY name",
    )
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let workers: Result<Vec<_>, ApiError> = rows
        .into_iter()
        .map(|row| {
            let labels_json: String = row.get("labels_json");
            let status = row
                .get::<Option<String>, _>("status_json")
                .and_then(|value| serde_json::from_str::<Value>(&value).ok());
            Ok(WorkerResponse {
                id: row.get("id"),
                name: row.get("name"),
                role: row.get("role"),
                state: row.get("state"),
                endpoint: row.get("endpoint"),
                protocol_version: row.get("protocol_version"),
                labels: serde_json::from_str(&labels_json).map_err(ApiError::internal)?,
                last_seen_at: row.get("last_seen_at"),
                storage: status
                    .as_ref()
                    .and_then(|value| value.get("storage"))
                    .cloned(),
                clock_skew_seconds: row.get("clock_skew_seconds"),
            })
        })
        .collect();
    Ok(Json(json!({"items": workers?})))
}

async fn drain_worker(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let administrator_id = auth::require_administrator(&state, &headers).await?;
    let result = sqlx::query("UPDATE workers SET state = 'draining', updated_at = ? WHERE id = ? AND state IN ('online', 'degraded')")
        .bind(Utc::now())
        .bind(&id)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::not_found("Worker 不存在或当前状态不能 drain"));
    }
    append_event(
        &state.database,
        "worker",
        &id,
        "worker_draining",
        json!({}),
        &administrator_id,
    )
    .await?;
    Ok(Json(
        json!({"id": id, "state": state_name(WorkerState::Draining)}),
    ))
}

async fn probe_worker(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let worker_state = crate::scheduler::probe_worker(&state, &id).await?;
    Ok(Json(json!({"id": id, "state": worker_state})))
}

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    required_role: WorkerRole,
    revision_sha256: String,
    #[serde(default)]
    kind: JobKind,
    profile_sha256: Option<String>,
    source_manifest_sha256: Option<String>,
    dependency_snapshot_sha256: Option<String>,
    preferred_worker_id: Option<Uuid>,
    source_attempt_id: Option<Uuid>,
    #[serde(default)]
    inputs: Vec<ManifestEntry>,
    #[serde(default)]
    inline_inputs: Vec<InlineInput>,
    #[serde(default)]
    expected_outputs: Vec<String>,
    allow_check: Option<bool>,
    #[serde(default)]
    required_labels: Vec<String>,
    limits: Option<ResourceLimits>,
    #[serde(default)]
    priority: i32,
}

async fn create_job(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateJobRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let administrator_id = auth::require_administrator(&state, &headers).await?;
    if request.revision_sha256.len() != 64
        || !request
            .revision_sha256
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Err(ApiError::bad_request(
            "INVALID_REVISION_DIGEST",
            "revision_sha256 必须是 64 位十六进制 SHA-256",
        ));
    }
    for (name, digest) in [
        ("profile_sha256", request.profile_sha256.as_deref()),
        (
            "source_manifest_sha256",
            request.source_manifest_sha256.as_deref(),
        ),
        (
            "dependency_snapshot_sha256",
            request.dependency_snapshot_sha256.as_deref(),
        ),
    ] {
        if let Some(digest) = digest
            && (digest.len() != 64 || !digest.chars().all(|value| value.is_ascii_hexdigit()))
        {
            return Err(ApiError::bad_request(
                "INVALID_JOB_DIGEST",
                format!("{name} 必须是 SHA-256"),
            ));
        }
    }
    if request.inputs.len() > 4096 {
        return Err(ApiError::bad_request(
            "TOO_MANY_INPUTS",
            "Job 输入文件不能超过 4096 个",
        ));
    }
    if request.inline_inputs.len() > 256 {
        return Err(ApiError::bad_request(
            "TOO_MANY_INLINE_INPUTS",
            "Job 内联输入文件不能超过 256 个",
        ));
    }
    let declared_inline_size = request
        .inline_inputs
        .iter()
        .try_fold(0_u64, |total, input| total.checked_add(input.entry.size))
        .ok_or_else(|| ApiError::bad_request("INLINE_INPUT_TOO_LARGE", "Job 内联输入大小溢出"))?;
    if declared_inline_size > 4 * 1024 * 1024 {
        return Err(ApiError::bad_request(
            "INLINE_INPUT_TOO_LARGE",
            "Job 内联输入总大小不能超过 4 MiB",
        ));
    }
    for input in &request.inputs {
        aursmith_protocol::validate_relative_path(&input.path)
            .map_err(|error| ApiError::bad_request("INVALID_INPUT_PATH", error.to_string()))?;
    }
    for input in &request.inline_inputs {
        aursmith_protocol::validate_relative_path(&input.entry.path)
            .map_err(|error| ApiError::bad_request("INVALID_INPUT_PATH", error.to_string()))?;
        if input.entry.path == ".aursmith" || input.entry.path.starts_with(".aursmith/") {
            return Err(ApiError::bad_request(
                "RESERVED_INPUT_PATH",
                ".aursmith 是 Worker 控制目录，不能作为 Job 输入",
            ));
        }
        let content = STANDARD.decode(&input.content_base64).map_err(|_| {
            ApiError::bad_request("INVALID_INLINE_INPUT", "内联输入不是合法 Base64")
        })?;
        if content.len() as u64 != input.entry.size
            || hex::encode(Sha256::digest(&content)) != input.entry.sha256
        {
            return Err(ApiError::bad_request(
                "INVALID_INLINE_INPUT",
                "内联输入内容与 Manifest 摘要不一致",
            ));
        }
    }
    if matches!(request.kind, JobKind::Build | JobKind::ProfileFixture)
        && request.profile_sha256.is_none()
    {
        return Err(ApiError::bad_request(
            "PROFILE_REQUIRED",
            "Build Job 必须固定 Profile",
        ));
    }
    let limits = request.limits.unwrap_or(ResourceLimits {
        cpu_count: 1,
        memory_mib: 1024,
        disk_mib: 4096,
        timeout_seconds: 600,
    });
    if limits.cpu_count == 0
        || limits.memory_mib < 256
        || limits.disk_mib < 512
        || limits.timeout_seconds == 0
    {
        return Err(ApiError::bad_request(
            "INVALID_RESOURCE_LIMITS",
            "任务资源限制超出允许范围",
        ));
    }
    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    sqlx::query(
        "INSERT INTO jobs(id, required_role, status, priority, revision_sha256, kind, profile_sha256, source_manifest_sha256, dependency_snapshot_sha256, preferred_worker_id, source_attempt_id, inputs_json, inline_inputs_json, expected_outputs_json, allow_check, required_labels_json, limits_json, created_at, updated_at) VALUES (?, ?, 'queued', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(role_name(request.required_role))
    .bind(request.priority)
    .bind(request.revision_sha256.to_ascii_lowercase())
    .bind(job_kind_name(request.kind))
    .bind(request.profile_sha256)
    .bind(request.source_manifest_sha256)
    .bind(request.dependency_snapshot_sha256)
    .bind(request.preferred_worker_id.map(|value| value.to_string()))
    .bind(request.source_attempt_id.map(|value| value.to_string()))
    .bind(serde_json::to_string(&request.inputs).map_err(ApiError::internal)?)
    .bind(serde_json::to_string(&request.inline_inputs).map_err(ApiError::internal)?)
    .bind(serde_json::to_string(&request.expected_outputs).map_err(ApiError::internal)?)
    .bind(i64::from(request.allow_check.unwrap_or(true)))
    .bind(serde_json::to_string(&request.required_labels).map_err(ApiError::internal)?)
    .bind(serde_json::to_string(&limits).map_err(ApiError::internal)?)
    .bind(now)
    .bind(now)
    .execute(&state.database)
    .await
    .map_err(ApiError::internal)?;
    append_event(
        &state.database,
        "job",
        &id,
        "job_created",
        json!({"required_role": role_name(request.required_role)}),
        &administrator_id,
    )
    .await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "status": "queued"})),
    ))
}

async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query(
        "SELECT jobs.id, jobs.required_role, jobs.status, jobs.priority, jobs.failure_code, jobs.revision_sha256, jobs.created_at, jobs.updated_at, workers.name AS worker_name FROM jobs LEFT JOIN workers ON workers.id = jobs.worker_id ORDER BY jobs.created_at DESC LIMIT 200",
    )
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "required_role": row.get::<String, _>("required_role"),
                "status": row.get::<String, _>("status"),
                "priority": row.get::<i64, _>("priority"),
                "failure_code": row.get::<Option<String>, _>("failure_code"),
                "revision_sha256": row.get::<Option<String>, _>("revision_sha256"),
                "worker_name": row.get::<Option<String>, _>("worker_name"),
                "created_at": row.get::<String, _>("created_at"),
                "updated_at": row.get::<String, _>("updated_at"),
            })
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

fn role_name(role: WorkerRole) -> &'static str {
    match role {
        WorkerRole::Builder => "builder",
        WorkerRole::Publisher => "publisher",
        WorkerRole::Archiver => "archiver",
    }
}

fn job_kind_name(kind: JobKind) -> &'static str {
    match kind {
        JobKind::Fetch => "fetch",
        JobKind::Build => "build",
        JobKind::ProfileFixture => "profile_fixture",
    }
}

fn state_name(state: WorkerState) -> &'static str {
    match state {
        WorkerState::Online => "online",
        WorkerState::Draining => "draining",
        WorkerState::Offline => "offline",
        WorkerState::Degraded => "degraded",
        WorkerState::Incompatible => "incompatible",
    }
}

pub(crate) async fn append_event(
    database: &SqlitePool,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    payload: Value,
    actor: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO events(event_id, aggregate_type, aggregate_id, event_type, payload_json, actor, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(event_type)
    .bind(payload.to_string())
    .bind(actor)
    .bind(Utc::now())
    .execute(database)
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

pub(crate) async fn append_event_in_transaction(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    aggregate_type: &str,
    aggregate_id: &str,
    event_type: &str,
    payload: Value,
    actor: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO events(event_id, aggregate_type, aggregate_id, event_type, payload_json, actor, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(Uuid::new_v4().to_string())
    .bind(aggregate_type)
    .bind(aggregate_id)
    .bind(event_type)
    .bind(payload.to_string())
    .bind(actor)
    .bind(Utc::now())
    .execute(&mut **transaction)
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    async fn test_router() -> Router {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let config = Config {
            bind_address: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            setup_token: "测试初始化令牌-至少二十个字符".into(),
            signing_key_file: "/不存在".into(),
            ssh_identity_source_file: "/不存在".into(),
            ssh_identity_file: "/不存在".into(),
            ssh_known_hosts_file: "/不存在".into(),
            secure_cookies: false,
            session_hours: 1,
            low_agent_endpoints: vec![],
            high_agent_endpoint: String::new(),
            agent_daily_call_limit: 300,
            agent_monthly_call_limit: 3000,
            agent_monthly_cost_limit_microusd: 5_000_000,
            repository_name: "aursmith".into(),
            source_git_commit: "test".into(),
            repository_base_url: "https://repo.test".into(),
            webhook_url: None,
            webhook_hmac_secret_file: "/不存在".into(),
            ntfy_url: None,
            backup_dir: "/不存在".into(),
            backup_export_dir: "/不存在".into(),
            backup_export_socket: "/不存在".into(),
        };
        router(AppState::new(
            database,
            config,
            SigningKey::from_bytes(&[9_u8; 32]),
        ))
    }

    #[tokio::test]
    async fn setup_login_and_authenticated_requirements_flow() {
        let app = test_router().await;
        let setup = Request::builder()
            .method("POST")
            .uri("/api/v1/setup")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "token": "测试初始化令牌-至少二十个字符",
                    "username": "admin",
                    "password": "足够长的测试密码-123456"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(setup).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);

        let login = Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({"username": "admin", "password": "足够长的测试密码-123456"}).to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(login).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();

        let requirements = Request::builder()
            .uri("/api/v1/requirements")
            .header("cookie", &cookie)
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(requirements).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let update_settings = Request::builder()
            .method("PUT")
            .uri("/api/v1/settings")
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "agent_daily_call_limit": 12,
                    "agent_monthly_call_limit": 120,
                    "agent_monthly_cost_limit_microusd": 3400
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(update_settings).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap())
                .unwrap();
        assert_eq!(body["budget"]["agent_daily_call_limit"], 12);
        assert_eq!(body["agents"]["api_keys_exposed"], false);

        let profile = Request::builder()
            .method("POST")
            .uri("/api/v1/profiles")
            .header("cookie", &cookie)
            .header("content-type", "application/json")
            .body(Body::from(json!({
                "name": "base",
                "spec": {
                    "profile_sha256": "untrusted-candidate-value",
                    "root_image": {"path": "root.qcow2", "sha256": "a".repeat(64), "size": 10},
                    "kernel": {"path": "vmlinuz-linux", "sha256": "b".repeat(64), "size": 10},
                    "initramfs": {"path": "initramfs-linux.img", "sha256": "c".repeat(64), "size": 10},
                    "installed_packages": ["base 3-3"],
                    "created_at": Utc::now()
                }
            }).to_string()))
            .unwrap();
        let response = app.clone().oneshot(profile).await.unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        assert_eq!(body["profile_sha256"].as_str().unwrap().len(), 64);
        assert_ne!(body["profile_sha256"], "untrusted-candidate-value");

        let activate = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/v1/profiles/{}/activate",
                body["id"].as_str().unwrap()
            ))
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(activate).await.unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
