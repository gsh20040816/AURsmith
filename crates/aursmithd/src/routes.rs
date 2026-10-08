//! HTTP 路由：管理 API、Builder API、健康检查与 SPA 静态文件。

use crate::{app::AppState, auth, builds, error::ApiError, packages, publish, review};
use aursmith_core::{credentials, protocol::UPLOAD_CHUNK_BYTES};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header::SET_COOKIE},
    middleware,
    response::IntoResponse,
    routing::{any, delete, get, post},
};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;
use tower_http::{
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    services::{ServeDir, ServeFile},
    trace::TraceLayer,
};

pub fn router(state: AppState) -> Router {
    let static_root = state.config.web_root.clone();
    let authentication_state = state.clone();
    let upload_limit = usize::try_from(UPLOAD_CHUNK_BYTES).unwrap_or(usize::MAX) + 1024 * 1024;
    Router::new()
        .route("/healthz", get(health))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", get(me))
        .route("/api/v1/status", get(status))
        .route("/api/v1/client-bootstrap", get(client_bootstrap))
        .route("/api/v1/aur/search", get(packages::search))
        .route(
            "/api/v1/subscriptions",
            get(packages::list_subscriptions).post(packages::subscribe),
        )
        .route(
            "/api/v1/subscriptions/{package_base}",
            delete(packages::unsubscribe),
        )
        .route("/api/v1/packages/{package_base}", get(packages::detail))
        .route(
            "/api/v1/packages/{package_base}/refresh",
            post(packages::refresh),
        )
        .route(
            "/api/v1/packages/{package_base}/rebuild",
            post(builds::rebuild),
        )
        .route(
            "/api/v1/packages/{package_base}/build-policy",
            post(packages::set_build_policy),
        )
        .route(
            "/api/v1/packages/{package_base}/providers/{dependency}",
            post(packages::select_provider),
        )
        .route("/api/v1/reviews", get(review::list))
        .route(
            "/api/v1/revisions/{id}/decision",
            post(review::decide_manually),
        )
        .route("/api/v1/revisions/{id}/retry-review", post(review::retry))
        .route("/api/v1/builds", get(builds::list))
        .route("/api/v1/builds/{id}/log", get(builds::log))
        .route("/api/v1/publications", get(publish::list))
        .route("/api/v1/builder/lease", post(builds::lease))
        .route("/api/v1/builder/builds/{id}/report", post(builds::report))
        .route(
            "/api/v1/builder/builds/{id}/files/{file}",
            get(builds::upload_get)
                .put(builds::upload_put)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/api/v1/builder/builds/{id}/complete",
            post(builds::complete),
        )
        .route(
            "/api/v1/builder/artifacts/{id}/{file}",
            get(builds::download_artifact),
        )
        .route("/api/{*path}", any(api_not_found))
        .fallback_service(
            ServeDir::new(&static_root).fallback(ServeFile::new(static_root.join("index.html"))),
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
            auth::management_guard,
        ))
}

async fn api_not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn health(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    sqlx::query("SELECT 1")
        .execute(&state.db)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(
        json!({"status": "ok", "service": "aursmithd", "version": env!("CARGO_PKG_VERSION")}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
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
        sqlx::query("SELECT username, password_hash FROM admin WHERE id = 1 AND username = ?")
            .bind(request.username.trim())
            .fetch_optional(&state.db)
            .await
            .map_err(ApiError::internal)?;
    let valid = row.as_ref().is_some_and(|row| {
        credentials::verify_password(&request.password, row.get("password_hash"))
    });
    let Some(row) = row.filter(|_| valid) else {
        return Err(ApiError::unauthorized("用户名或密码错误"));
    };
    let token = auth::create_session(&state).await?;
    let cookie = auth::session_cookie(&token, state.config.session_absolute_hours * 3600);
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok((
        response_headers,
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

async fn me(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let username: String = sqlx::query_scalar("SELECT username FROM admin WHERE id = 1")
        .fetch_one(&state.db)
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(json!({"username": username})))
}

async fn count(state: &AppState, sql: &str) -> Result<i64, ApiError> {
    sqlx::query_scalar(sql)
        .fetch_one(&state.db)
        .await
        .map_err(ApiError::internal)
}

/// 合并后的状态页（替代旧 Doctor）：只给出运维真正需要的几个信号。
async fn status(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let builder = state.builder_last_seen();
    let builder_ok = builder
        .as_ref()
        .is_some_and(|(_, seen)| *seen > Utc::now() - Duration::minutes(5));
    let latest = sqlx::query("SELECT state, error, created_at, finished_at FROM publications ORDER BY created_at DESC LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::internal)?;
    let publication_ok = latest
        .as_ref()
        .is_none_or(|row| row.get::<String, _>("state") != "failed");
    let checks = vec![
        json!({"id": "database", "ok": true, "message": "SQLite 可用"}),
        json!({
            "id": "review",
            "ok": state.reviewer.is_some(),
            "message": if state.reviewer.is_some() { "2+1 Agent 审查已配置" } else { "未配置 Agent 审查：所有 revision 进入人工审查" },
        }),
        json!({
            "id": "builder",
            "ok": builder_ok,
            "message": match &builder {
                Some((id, seen)) => format!("Builder {id} 最近一次租约请求：{}", seen.to_rfc3339()),
                None => "自服务启动以来没有 Builder 请求租约".to_owned(),
            },
        }),
        json!({
            "id": "signer",
            "ok": publication_ok,
            "message": match &latest {
                Some(row) => format!("最近一次发布：{}{}", row.get::<String, _>("state"), row.get::<Option<String>, _>("error").map(|error| format!("（{error}）")).unwrap_or_default()),
                None => "尚未发布".to_owned(),
            },
        }),
    ];
    let ready = checks.iter().all(|check| check["ok"] == true);
    Ok(Json(json!({
        "ready": ready,
        "checked_at": Utc::now(),
        "checks": checks,
        "counts": {
            "subscriptions": count(&state, "SELECT COUNT(*) FROM subscriptions").await?,
            "pending_review": count(&state, "SELECT COUNT(*) FROM revisions WHERE state = 'pending_review'").await?,
            "manual_review": count(&state, "SELECT COUNT(*) FROM revisions WHERE state = 'manual_review'").await?,
            "queued": count(&state, "SELECT COUNT(*) FROM builds WHERE state = 'queued'").await?,
            "running": count(&state, "SELECT COUNT(*) FROM builds WHERE state IN ('running', 'uploading')").await?,
            "failed": count(&state, "SELECT COUNT(*) FROM builds WHERE state = 'failed' AND finished_at > datetime('now', '-1 day')").await?,
        },
    })))
}

async fn client_bootstrap(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let fingerprint: String = sqlx::query_scalar("SELECT keyring_fingerprint FROM publications WHERE state = 'published' AND keyring_fingerprint IS NOT NULL ORDER BY created_at DESC LIMIT 1")
        .fetch_optional(&state.db)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::conflict("REPOSITORY_KEY_NOT_READY", "签名器尚未完成首次发布，仓库公钥指纹未知"))?;
    let base = state.config.repository_base_url.trim_end_matches('/');
    let name = &state.config.repository_name;
    Ok(Json(json!({
        "repository_config": format!("[{name}]\nSigLevel = Required DatabaseRequired\nServer = {base}/$arch"),
        "gpg_fingerprint": fingerprint,
        "gpg_key_url": format!("{base}/x86_64/aursmith-repository-key.asc"),
        "commands": [
            format!("curl --fail --output /tmp/aursmith-repository-key.asc '{base}/x86_64/aursmith-repository-key.asc'"),
            "sudo pacman-key --add /tmp/aursmith-repository-key.asc".to_owned(),
            format!("sudo pacman-key --lsign-key {fingerprint}"),
            "sudo pacman -Syu aursmith-keyring".to_owned(),
        ],
        "warnings": [
            "执行导入前必须人工核对页面显示的完整 GPG 指纹。",
            "AURsmith 仓库必须放在官方仓库之后。",
            "aursmith-keyring 只在签名密钥更换时更新。",
        ],
    })))
}
