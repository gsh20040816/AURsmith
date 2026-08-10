use crate::{error::ApiError, routes::AppState};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::http::HeaderMap;
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(ApiError::internal)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(ApiError::internal)
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

pub fn sha256(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

pub async fn create_session(state: &AppState, administrator_id: &str) -> Result<String, ApiError> {
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let now = Utc::now();
    let expires = now + Duration::hours(state.config.session_hours);
    sqlx::query(
        "INSERT INTO sessions(token_sha256, administrator_id, created_at, expires_at, last_seen_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(sha256(&token))
    .bind(administrator_id)
    .bind(now)
    .bind(expires)
    .bind(now)
    .execute(&state.database)
    .await
    .map_err(ApiError::internal)?;
    Ok(token)
}

pub async fn require_administrator(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, ApiError> {
    let token = cookie_value(headers, "aursmith_session")
        .ok_or_else(|| ApiError::unauthorized("缺少登录会话"))?;
    let now = Utc::now();
    let administrator_id: Option<String> = sqlx::query_scalar(
        "SELECT administrator_id FROM sessions WHERE token_sha256 = ? AND expires_at > ?",
    )
    .bind(sha256(token))
    .bind(now)
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    let administrator_id =
        administrator_id.ok_or_else(|| ApiError::unauthorized("会话无效或已过期"))?;
    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_sha256 = ?")
        .bind(now)
        .bind(sha256(token))
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(administrator_id)
}

pub fn session_cookie(token: &str, secure: bool, max_age_seconds: i64) -> String {
    let secure_attribute = if secure { "; Secure" } else { "" };
    format!(
        "aursmith_session={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}{secure_attribute}"
    )
}

pub fn expired_session_cookie(secure: bool) -> String {
    session_cookie("", secure, 0)
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(axum::http::header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == name).then_some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_verifies_only_the_original_password() {
        let hash = hash_password("一段足够长的密码").unwrap();
        assert!(verify_password("一段足够长的密码", &hash));
        assert!(!verify_password("错误密码", &hash));
    }

    #[test]
    fn secure_cookie_is_http_only_and_strict() {
        let cookie = session_cookie("secret", true, 3600);
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));
        assert!(cookie.contains("Secure"));
    }
}
