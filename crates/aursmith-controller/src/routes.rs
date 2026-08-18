use crate::{auth, config::Config, error::ApiError};
use aursmith_domain::credentials;
use aursmith_protocol::{BuilderLease, BuilderPoll};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    middleware,
    response::IntoResponse,
    routing::{any, delete, get, post},
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
#[cfg(test)]
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{collections::BTreeSet, sync::Arc};
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};
use uuid::Uuid;

#[derive(Clone)]
pub struct AppState {
    pub database: SqlitePool,
    pub config: Arc<Config>,
}

impl AppState {
    pub fn new(database: SqlitePool, config: Config) -> Self {
        Self {
            database,
            config: Arc::new(config),
        }
    }
}

pub fn router(state: AppState) -> Router {
    let authentication_state = state.clone();
    Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/client-bootstrap", get(client_bootstrap))
        .route("/api/v1/doctor", get(doctor_status))
        .route("/api/v1/builder/poll", post(builder_poll))
        .route("/api/v1/jobs", get(list_jobs))
        .route("/api/v1/jobs/{id}/logs", get(job_logs))
        .route("/api/v1/audits", get(crate::audits::list))
        .route("/api/v1/audits/{bundle}/retry", post(crate::audits::retry))
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
            "/api/v1/subscriptions/{package_base}",
            delete(crate::packages::delete_subscription),
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
            "/api/v1/packages/{package_base}/rebuild",
            post(manual_rebuild_package),
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
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(
            ServeDir::new("/srv").not_found_service(ServeFile::new("/srv/index.html")),
        )
        .with_state(state)
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::new(
            axum::http::HeaderName::from_static("x-request-id"),
            MakeRequestUuid,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(middleware::from_fn_with_state(
            authentication_state,
            auth::authorize_management_request,
        ))
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "service": "controller", "version": env!("CARGO_PKG_VERSION")}))
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
    let keyring: Value = sqlx::query_scalar::<_, String>(
        "SELECT value_json FROM system_settings WHERE key = 'keyring_state'",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .and_then(|value| serde_json::from_str(&value).ok())
    .unwrap_or(Value::Null);
    let base = state.config.repository_base_url.trim_end_matches('/');
    let repository_config =
        format!("[aursmith]\nSigLevel = Required DatabaseRequired\nServer = {base}/$arch");
    let warnings = vec![
        "执行导入前必须人工核对页面显示的完整 GPG 指纹。",
        "AURsmith 仓库必须放在官方仓库之后。",
        "控制面和仓库证书由现有系统信任链验证。",
    ];
    Ok(Json(json!({
        "repository_config": repository_config,
        "gpg_fingerprint": fingerprint,
        "gpg_key_url": format!("{base}/x86_64/aursmith-repository-key.asc"),
        "keyring_generation": keyring["generation"],
        "keyring_published_at": keyring["published_at"],
        "keyring_next_due_at": keyring["next_due_at"],
        "client_ca_url": null,
        "commands": [
            format!("curl --fail --output /tmp/aursmith-repository-key.asc '{base}/x86_64/aursmith-repository-key.asc'"),
            "sudo pacman-key --add /tmp/aursmith-repository-key.asc".to_owned(),
            format!("sudo pacman-key --lsign-key {fingerprint}"),
            "sudo pacman -Syu aursmith-keyring".to_owned(),
        ],
        "warnings": warnings
    })))
}

async fn doctor_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let mut checks = Vec::new();
    let builder = sqlx::query("SELECT last_seen_at, status_json FROM builder_runtime WHERE id = 1")
        .fetch_optional(&state.database)
        .await
        .map_err(ApiError::internal)?;
    let builder_recent = builder.as_ref().is_some_and(|row| {
        row.get::<Option<chrono::DateTime<Utc>>, _>("last_seen_at")
            .is_some_and(|last_poll| last_poll >= Utc::now() - chrono::Duration::minutes(2))
    });
    let builder_status = builder
        .as_ref()
        .and_then(|row| row.get::<Option<String>, _>("status_json"))
        .and_then(|value| serde_json::from_str::<Value>(&value).ok())
        .unwrap_or(Value::Null);
    let available = builder_status["storage"]["available_percent"].as_u64();
    checks.push(json!({
        "id": "builder-poll",
        "ok": builder_recent && available.is_none_or(|value| value >= 10),
        "message": format!("Builder 最近轮询：{}，可用空间：{}%", if builder_recent { "正常" } else { "超过 2 分钟" }, available.map(|value| value.to_string()).unwrap_or_else(|| "未知".into())),
    }));
    let fingerprint_ready: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM system_settings WHERE key = 'repository_gpg_fingerprint'",
    )
    .fetch_one(&state.database)
    .await
    .map_err(ApiError::internal)?;
    checks.push(json!({"id": "repository-gpg", "ok": fingerprint_ready == 1, "message": "仓库 GPG 指纹已由 Publisher 固定"}));
    checks.push(json!({"id": "controller-tls", "ok": true, "message": "TLS 由宿主反向代理和系统信任链负责"}));
    let agent_client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(2))
        .timeout(std::time::Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(ApiError::internal)?;
    for (index, endpoint) in state.config.low_agent_endpoints.iter().enumerate() {
        checks.push(
            agent_doctor_check(&agent_client, &format!("agent-low-{}", index + 1), endpoint).await,
        );
    }
    if state.config.low_agent_endpoints.len() != 3 {
        checks.push(json!({"id": "agent-low-count", "ok": false, "message": format!("需要 3 个低成本 Agent Runner，当前配置 {} 个", state.config.low_agent_endpoints.len())}));
    }
    if state.config.high_agent_endpoint.is_empty() {
        checks.push(
            json!({"id": "agent-high", "ok": false, "message": "高成本 Agent Runner 未配置"}),
        );
    } else {
        checks.push(
            agent_doctor_check(
                &agent_client,
                "agent-high",
                &state.config.high_agent_endpoint,
            )
            .await,
        );
    }
    match crate::transport::publisher_doctor(&state.config).await {
        Ok(reply) if reply.ok => {
            let check = &reply.data["checks"]["aur"];
            checks.push(json!({
                "id": "publisher-aur",
                "ok": check["ok"].as_bool().unwrap_or(false),
                "message": check["message"].as_str().unwrap_or("Publisher Doctor 返回字段无效")
            }));
        }
        Ok(reply) => {
            checks.push(json!({"id": "publisher-upstream", "ok": false, "message": reply.message}))
        }
        Err(error) => checks
            .push(json!({"id": "publisher-upstream", "ok": false, "message": error.to_string()})),
    }
    let ready = checks.iter().all(|check| check["ok"] == true);
    Ok(Json(
        json!({"ready": ready, "checked_at": Utc::now(), "checks": checks}),
    ))
}

async fn agent_doctor_check(client: &reqwest::Client, id: &str, endpoint: &str) -> Value {
    let result = async {
        let mut url = url::Url::parse(endpoint)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            anyhow::bail!("Runner endpoint 不是 HTTP(S) URL");
        }
        url.set_path("/healthz");
        url.set_query(None);
        url.set_fragment(None);
        let response = client.get(url).send().await?.error_for_status()?;
        let payload: Value = response.json().await?;
        if payload["ok"] != true || payload["credential_gateway_reachable"] != true {
            anyhow::bail!("Runner 健康响应不完整");
        }
        Ok::<Value, anyhow::Error>(payload)
    }
    .await;
    match result {
        Ok(payload) => json!({
            "id": id,
            "ok": true,
            "message": format!("{} / {}：CLI 与凭据网关可用", payload["adapter"].as_str().unwrap_or("unknown"), payload["model"].as_str().unwrap_or("unknown"))
        }),
        Err(error) => {
            json!({"id": id, "ok": false, "message": format!("Agent Runner 探测失败：{error}")})
        }
    }
}

async fn manual_rebuild_package(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(package_base): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let actor = auth::require_administrator(&state, &headers).await?;
    let batch_id = crate::packages::schedule_rebuild_batch(
        &state.database,
        BTreeSet::from([package_base.clone()]),
        &actor,
        "manual_rebuild",
    )
    .await?
    .ok_or_else(|| ApiError::internal("手工重建批次未创建"))?;
    Ok(Json(json!({
        "package_base": package_base,
        "state": "scheduled",
        "batch_id": batch_id,
    })))
}

#[derive(Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    auth::require_origin(&state, &headers)?;
    let row =
        sqlx::query("SELECT id, username, password_hash FROM administrators WHERE username = ?")
            .bind(request.username.trim())
            .fetch_optional(&state.database)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::unauthorized("用户名或密码错误"))?;
    let password_hash: String = row.get("password_hash");
    if !credentials::verify_password(&request.password, &password_hash) {
        return Err(ApiError::unauthorized("用户名或密码错误"));
    }
    let administrator_id: String = row.get("id");
    let token = auth::create_session(&state, &administrator_id).await?;
    let maximum_age_seconds = state
        .config
        .session_absolute_hours
        .checked_mul(3600)
        .ok_or_else(|| ApiError::internal("绝对会话期限换算溢出"))?;
    let cookie = auth::session_cookie(&token, maximum_age_seconds);
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
    auth::delete_session(&state, &headers).await?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&auth::expired_session_cookie()).map_err(ApiError::internal)?,
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

async fn builder_poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(poll): Json<BuilderPoll>,
) -> Result<Json<BuilderLease>, ApiError> {
    auth::require_builder(&state, &headers)?;
    if (Utc::now() - poll.sent_at).num_seconds().unsigned_abs() > 120 {
        return Err(ApiError::conflict(
            "STALE_WORKER_POLL",
            "Builder 轮询时间戳已过期",
        ));
    }
    let mut acknowledged_attempts = Vec::new();
    for report in poll.attempts {
        let result = sqlx::query("INSERT INTO builder_reports(job_id, response_json, updated_at) SELECT id, ?, ? FROM jobs WHERE id = ? ON CONFLICT(job_id) DO UPDATE SET response_json = excluded.response_json, updated_at = excluded.updated_at")
            .bind(report.response.to_string())
            .bind(Utc::now()).bind(report.job_id.to_string())
            .execute(&state.database).await.map_err(ApiError::internal)?;
        if result.rows_affected() > 0 {
            acknowledged_attempts.push(report.job_id);
        }
    }
    for upload_id in poll.completed_uploads {
        sqlx::query("UPDATE uploads SET state = 'verified', last_error = NULL, export_cleaned_at = COALESCE(export_cleaned_at, ?), updated_at = ? WHERE id = ? AND state IN ('export_ready', 'verified')")
            .bind(Utc::now())
            .bind(Utc::now()).bind(upload_id.to_string())
            .execute(&state.database).await.map_err(ApiError::internal)?;
    }
    let status = &poll.status;
    if status["role"].as_str() != Some("builder") {
        return Err(ApiError::conflict(
            "INVALID_BUILDER_STATUS",
            "Builder 状态角色无效",
        ));
    }
    sqlx::query(
        "UPDATE builder_runtime SET status_json = ?, last_seen_at = ?, updated_at = ? WHERE id = 1",
    )
    .bind(status.to_string())
    .bind(Utc::now())
    .bind(Utc::now())
    .execute(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let releasable_attempts = releasable_reverse_attempts(&state.database).await?;
    let job = crate::scheduler::lease_reverse_job(&state).await?;
    let upload = crate::scheduler::lease_reverse_upload(&state).await?;
    Ok(Json(BuilderLease {
        acknowledged_attempts,
        releasable_attempts,
        job,
        upload,
        issued_at: Utc::now(),
        next_poll_seconds: 15,
    }))
}

async fn releasable_reverse_attempts(database: &SqlitePool) -> Result<Vec<Uuid>, ApiError> {
    Ok(sqlx::query_scalar::<_, String>(
        "SELECT attempts.id FROM attempts JOIN jobs ON jobs.id = attempts.job_id LEFT JOIN release_batches ON release_batches.id = jobs.batch_id WHERE attempts.status IN ('succeeded', 'failed', 'cancelled') AND (jobs.status IN ('failed', 'cancelled') OR release_batches.state IN ('published', 'build_failed', 'publish_failed', 'transfer_failed', 'superseded')) ORDER BY attempts.finished_at, attempts.id LIMIT 256",
    )
    .fetch_all(database)
    .await
    .map_err(ApiError::internal)?
    .into_iter()
    .filter_map(|value| value.parse().ok())
    .collect())
}

async fn list_jobs(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    let rows = sqlx::query(
        "SELECT jobs.id, jobs.kind, jobs.status, jobs.priority, jobs.failure_code, jobs.revision_sha256, jobs.next_attempt_at, jobs.created_at, jobs.updated_at, (SELECT COUNT(*) FROM attempts WHERE attempts.job_id = jobs.id) AS attempt_count, EXISTS(SELECT 1 FROM job_logs WHERE job_logs.job_id = jobs.id) AS has_logs FROM jobs ORDER BY jobs.created_at DESC LIMIT 200",
    )
    .fetch_all(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let items: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<String, _>("id"),
                "kind": row.get::<String, _>("kind"),
                "status": row.get::<String, _>("status"),
                "priority": row.get::<i64, _>("priority"),
                "failure_code": row.get::<Option<String>, _>("failure_code"),
                "revision_sha256": row.get::<Option<String>, _>("revision_sha256"),
                "attempt_count": row.get::<i64, _>("attempt_count"),
                "has_logs": row.get::<bool, _>("has_logs"),
                "next_attempt_at": row.get::<Option<String>, _>("next_attempt_at"),
                "created_at": row.get::<String, _>("created_at"),
                "updated_at": row.get::<String, _>("updated_at"),
            })
        })
        .collect();
    Ok(Json(json!({"items": items})))
}

async fn job_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    auth::require_administrator(&state, &headers).await?;
    Uuid::parse_str(&id).map_err(|_| ApiError::bad_request("INVALID_JOB_ID", "Job ID 无效"))?;
    let row = sqlx::query(
        "SELECT kind, document_json, sha256, created_at FROM job_logs WHERE job_id = ?",
    )
    .bind(&id)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::not_found("Job 尚无可用证据"))?;
    Ok(Json(json!({
        "job_id": id,
        "kind": row.get::<String, _>("kind"),
        "sha256": row.get::<String, _>("sha256"),
        "document": serde_json::from_str::<Value>(row.get("document_json")).map_err(ApiError::internal)?,
        "created_at": row.get::<String, _>("created_at")
    })))
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
        insert_test_administrator(&database).await;
        router(AppState::new(database, test_config()))
    }

    fn test_config() -> Config {
        Config {
            bind_address: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            public_origin: "https://aursmith.test".into(),
            session_idle_minutes: 30,
            session_absolute_hours: 1,
            low_agent_endpoints: vec![],
            high_agent_endpoint: String::new(),
            repository_name: "aursmith".into(),
            source_git_commit: "test".into(),
            repository_base_url: "https://repo.test".into(),
            builder_token_sha256: auth::sha256("test-builder-token"),
            builder_max_concurrent: 1,
            update_interval_minutes: 30,
            publisher_socket: "/run/aursmith-publisher/worker.sock".into(),
        }
    }

    async fn insert_test_administrator(database: &SqlitePool) {
        sqlx::query(
            "INSERT INTO administrators(id, username, password_hash, created_at) VALUES ('admin-id', 'admin', ?, ?)",
        )
        .bind(credentials::hash_password("足够长的测试密码-123456").unwrap())
        .bind(Utc::now())
        .execute(database)
        .await
        .unwrap();
    }

    async fn login_cookie(app: &Router) -> String {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({"username": "admin", "password": "足够长的测试密码-123456"})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        response
            .headers()
            .get(SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn removed_management_platform_routes_are_not_reachable() {
        let app = test_router().await;
        let cookie = login_cookie(&app).await;
        for path in [
            "/api/v1/requirements",
            "/api/v1/settings",
            "/api/v1/events",
            "/api/v1/client-ca.crt",
            "/api/v1/metrics",
            "/api/v1/alerts",
            "/api/v1/backups",
            "/api/v1/archive-inventories",
            "/api/v1/workers",
            "/api/v1/archives",
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    }

    #[tokio::test]
    async fn removed_setup_routes_are_not_reachable_after_authentication() {
        let app = test_router().await;
        let cookie = login_cookie(&app).await;
        let status = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/setup/status")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::NOT_FOUND);

        let setup = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/setup")
                    .header("cookie", cookie)
                    .header("origin", "https://aursmith.test")
                    .header(auth::CSRF_HEADER_NAME, auth::CSRF_HEADER_VALUE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(setup.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn login_requires_fixed_origin() {
        let app = test_router().await;
        let body = || {
            Body::from(
                json!({"username": "admin", "password": "错误但足够长的密码-123456"}).to_string(),
            )
        };
        for origin in [None, Some("https://other.test")] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/auth/login")
                .header("content-type", "application/json");
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            let response = app
                .clone()
                .oneshot(request.body(body()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let valid_origin = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/login")
                    .header("origin", "https://aursmith.test")
                    .header("content-type", "application/json")
                    .body(body())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(valid_origin.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn management_writes_require_origin_and_csrf_but_get_does_not_touch_session() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        insert_test_administrator(&database).await;
        let app = router(AppState::new(database.clone(), test_config()));
        let cookie = login_cookie(&app).await;
        let before: String = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
            .fetch_one(&database)
            .await
            .unwrap();
        let read = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(read.status(), StatusCode::OK);
        let after: String = sqlx::query_scalar("SELECT last_seen_at FROM sessions")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(after, before, "GET 不得刷新会话活动时间");

        for (origin, csrf) in [
            (None, None),
            (Some("https://other.test"), Some(auth::CSRF_HEADER_VALUE)),
            (Some("https://aursmith.test"), None),
        ] {
            let mut request = Request::builder()
                .method("POST")
                .uri("/api/v1/auth/logout")
                .header("cookie", &cookie);
            if let Some(origin) = origin {
                request = request.header("origin", origin);
            }
            if let Some(csrf) = csrf {
                request = request.header(auth::CSRF_HEADER_NAME, csrf);
            }
            let response = app
                .clone()
                .oneshot(request.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let logout = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/auth/logout")
                    .header("cookie", cookie)
                    .header("origin", "https://aursmith.test")
                    .header(auth::CSRF_HEADER_NAME, auth::CSRF_HEADER_VALUE)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn idle_and_absolute_session_expiry_are_both_enforced() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        insert_test_administrator(&database).await;
        let app = router(AppState::new(database.clone(), test_config()));
        let idle_cookie = login_cookie(&app).await;
        sqlx::query("UPDATE sessions SET last_seen_at = ?")
            .bind(Utc::now() - chrono::Duration::minutes(31))
            .execute(&database)
            .await
            .unwrap();
        let idle = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header("cookie", idle_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(idle.status(), StatusCode::UNAUTHORIZED);

        sqlx::query("DELETE FROM sessions")
            .execute(&database)
            .await
            .unwrap();
        let absolute_cookie = login_cookie(&app).await;
        sqlx::query("UPDATE sessions SET expires_at = ?")
            .bind(Utc::now() - chrono::Duration::seconds(1))
            .execute(&database)
            .await
            .unwrap();
        let absolute = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/auth/me")
                    .header("cookie", absolute_cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(absolute.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn browser_session_does_not_authorize_the_builder_api() {
        let app = test_router().await;
        let cookie = login_cookie(&app).await;
        let poll = BuilderPoll {
            status: json!({"role": "builder"}),
            attempts: Vec::new(),
            completed_uploads: Vec::new(),
            sent_at: Utc::now(),
        };
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/builder/poll")
                    .header("cookie", cookie)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&poll).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn builder_poll_ignores_reports_for_unknown_jobs() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let config = Config {
            bind_address: "127.0.0.1:0".into(),
            database_url: "sqlite::memory:".into(),
            public_origin: "https://aursmith.test".into(),
            session_idle_minutes: 30,
            session_absolute_hours: 1,
            low_agent_endpoints: vec![],
            high_agent_endpoint: String::new(),
            repository_name: "aursmith".into(),
            source_git_commit: "test".into(),
            repository_base_url: "https://repo.test".into(),
            builder_token_sha256: auth::sha256("test-builder-token"),
            builder_max_concurrent: 1,
            update_interval_minutes: 30,
            publisher_socket: "/run/aursmith-publisher/worker.sock".into(),
        };
        let now = Utc::now();
        let app = router(AppState::new(database, config));
        let poll = BuilderPoll {
            status: json!({"role": "builder"}),
            attempts: vec![aursmith_protocol::ReverseAttemptReport {
                job_id: Uuid::new_v4(),
                response: json!({"ok": true}),
            }],
            completed_uploads: Vec::new(),
            sent_at: now,
        };
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/builder/poll")
                    .header("authorization", "Bearer test-builder-token")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&poll).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let lease: BuilderLease = serde_json::from_slice(&body).unwrap();
        assert!(lease.acknowledged_attempts.is_empty());
    }

    #[tokio::test]
    async fn reverse_worker_releases_only_terminal_batch_workspaces() {
        let database = crate::db::connect("sqlite::memory:").await.unwrap();
        let now = Utc::now();
        let active_batch = Uuid::new_v4();
        let published_batch = Uuid::new_v4();
        for (id, state) in [(active_batch, "building"), (published_batch, "published")] {
            sqlx::query("INSERT INTO release_batches(id, state, graph_json, created_at, updated_at) VALUES (?, ?, '{}', ?, ?)")
                .bind(id.to_string()).bind(state).bind(now).bind(now).execute(&database).await.unwrap();
        }
        let active_attempt = Uuid::new_v4();
        let published_attempt = Uuid::new_v4();
        let failed_attempt = Uuid::new_v4();
        for (batch, job_status, kind, attempt) in [
            (Some(active_batch), "succeeded", "build", active_attempt),
            (
                Some(published_batch),
                "succeeded",
                "build",
                published_attempt,
            ),
            (Some(active_batch), "failed", "build", failed_attempt),
        ] {
            let job_id = Uuid::new_v4();
            sqlx::query("INSERT INTO jobs(id, batch_id, status, priority, kind, inputs_json, inline_inputs_json, created_at, updated_at) VALUES (?, ?, ?, 1, ?, '[]', '[]', ?, ?)")
                .bind(job_id.to_string()).bind(batch.map(|value| value.to_string()))
                .bind(job_status).bind(kind).bind(now).bind(now).execute(&database).await.unwrap();
            sqlx::query("INSERT INTO attempts(id, job_id, generation, token_sha256, status, finished_at) VALUES (?, ?, 0, ?, ?, ?)")
                .bind(attempt.to_string()).bind(job_id.to_string()).bind(hex::encode(Sha256::digest(attempt.as_bytes())))
                .bind(job_status).bind(now).execute(&database).await.unwrap();
        }

        let releasable = releasable_reverse_attempts(&database).await.unwrap();
        assert!(!releasable.contains(&active_attempt));
        assert!(releasable.contains(&published_attempt));
        assert!(releasable.contains(&failed_attempt));
    }

    #[tokio::test]
    async fn agent_doctor_probes_cli_and_credential_gateway_status() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(
                    "/healthz",
                    get(|| async {
                        Json(json!({
                            "ok": true,
                            "adapter": "codex",
                            "model": "fixture",
                            "credential_gateway_reachable": true
                        }))
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let client = reqwest::Client::new();
        let check = agent_doctor_check(&client, "agent-low-1", &format!("http://{address}")).await;
        assert_eq!(check["ok"], true);
        assert!(check["message"].as_str().unwrap().contains("codex"));
        server.abort();
    }
}
