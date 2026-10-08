//! 管理员会话、CSRF/Origin 校验与 Builder Bearer 校验。

use crate::{app::AppState, error::ApiError};
use axum::{
    extract::{Request, State},
    http::{
        HeaderMap, Method,
        header::{AUTHORIZATION, ORIGIN},
    },
    middleware::Next,
    response::Response,
};
use chrono::{Duration, Utc};
use uuid::Uuid;

pub const SESSION_COOKIE_NAME: &str = "__Host-aursmith_session";
pub const CSRF_HEADER_NAME: &str = "x-aursmith-csrf";
pub const CSRF_HEADER_VALUE: &str = "1";

fn sha256(value: &str) -> String {
    aursmith_core::sha256_hex(value.as_bytes())
}

/// 常量时间比较 Bearer token 的 SHA-256。
pub fn require_builder(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::unauthorized("缺少 Builder Bearer 凭据"))?;
    let actual = sha256(token);
    let expected = state.config.builder_token_sha256.as_bytes();
    let matches = actual.len() == expected.len()
        && actual
            .as_bytes()
            .iter()
            .zip(expected)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            })
            == 0;
    if !matches {
        return Err(ApiError::unauthorized("Builder Bearer 凭据无效"));
    }
    Ok(())
}

pub async fn create_session(state: &AppState) -> Result<String, ApiError> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let now = Utc::now();
    let expires = now + Duration::hours(state.config.session_absolute_hours);
    sqlx::query(
        "INSERT INTO sessions(token_sha256, created_at, last_seen_at, expires_at) VALUES (?, ?, ?, ?)",
    )
    .bind(sha256(&token))
    .bind(now)
    .bind(now)
    .bind(expires)
    .execute(&state.db)
    .await
    .map_err(ApiError::internal)?;
    Ok(token)
}

pub async fn require_administrator(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = session_token(headers).ok_or_else(|| ApiError::unauthorized("缺少登录会话"))?;
    let now = Utc::now();
    let idle_cutoff = now - Duration::minutes(state.config.session_idle_minutes);
    let found: Option<String> = sqlx::query_scalar(
        "SELECT token_sha256 FROM sessions WHERE token_sha256 = ? AND expires_at > ? AND last_seen_at > ?",
    )
    .bind(sha256(token))
    .bind(now)
    .bind(idle_cutoff)
    .fetch_optional(&state.db)
    .await
    .map_err(ApiError::internal)?;
    found
        .map(|_| ())
        .ok_or_else(|| ApiError::unauthorized("会话无效或已过期"))
}

/// 管理 API 统一鉴权：除登录、健康检查和 Builder 接口外都需要会话；
/// 写操作额外要求固定 Origin 与 CSRF 头。
pub async fn management_guard(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let path = request.uri().path();
    if !path.starts_with("/api/")
        || path == "/api/v1/auth/login"
        || path.starts_with("/api/v1/builder/")
    {
        return Ok(next.run(request).await);
    }
    require_administrator(&state, request.headers()).await?;
    if is_state_changing(request.method()) {
        require_origin(&state, request.headers())?;
        require_csrf(request.headers())?;
        touch_session(&state, request.headers()).await?;
    }
    Ok(next.run(request).await)
}

pub fn require_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let origin = headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::forbidden("缺少请求 Origin"))?;
    if origin != state.config.public_origin {
        return Err(ApiError::forbidden("请求 Origin 不受信任"));
    }
    Ok(())
}

fn require_csrf(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers
        .get(CSRF_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        != Some(CSRF_HEADER_VALUE)
    {
        return Err(ApiError::forbidden("缺少有效的 CSRF 请求头"));
    }
    Ok(())
}

async fn touch_session(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = session_token(headers).ok_or_else(|| ApiError::unauthorized("缺少登录会话"))?;
    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_sha256 = ?")
        .bind(Utc::now())
        .bind(sha256(token))
        .execute(&state.db)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

pub async fn delete_session(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let token = session_token(headers).ok_or_else(|| ApiError::unauthorized("缺少登录会话"))?;
    sqlx::query("DELETE FROM sessions WHERE token_sha256 = ?")
        .bind(sha256(token))
        .execute(&state.db)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

pub fn session_cookie(token: &str, max_age_seconds: i64) -> String {
    format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}"
    )
}

pub fn expired_session_cookie() -> String {
    session_cookie("", 0)
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == SESSION_COOKIE_NAME).then_some(value))
}

fn is_state_changing(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_has_the_fixed_host_only_security_contract() {
        let cookie = session_cookie("secret", 3600);
        assert_eq!(
            cookie,
            "__Host-aursmith_session=secret; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=3600"
        );
        assert!(!cookie.contains("Domain="));
    }

    #[test]
    fn only_get_and_head_are_read_only_methods() {
        assert!(!is_state_changing(&Method::GET));
        assert!(!is_state_changing(&Method::HEAD));
        assert!(is_state_changing(&Method::POST));
        assert!(is_state_changing(&Method::PUT));
        assert!(is_state_changing(&Method::DELETE));
    }
}
