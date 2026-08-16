use crate::{credentials, error::ApiError, web::AppState};
use axum::{
    Extension, Json,
    extract::{Request, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode,
        header::{COOKIE, ORIGIN, SET_COOKIE},
    },
    middleware::Next,
    response::{IntoResponse, Redirect, Response},
};
use chrono::{Duration, Utc};
use rand_core::{OsRng, RngCore};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{
    collections::{BTreeMap, VecDeque},
    net::IpAddr,
    time::{Duration as StdDuration, Instant},
};

pub const SESSION_COOKIE_NAME: &str = "__Host-aursmith_session";
pub const CSRF_HEADER_NAME: &str = "x-aursmith-csrf";
pub const CSRF_HEADER_VALUE: &str = "1";
pub const TRUSTED_CLIENT_IP_HEADER_NAME: &str = "x-aursmith-client-ip";

#[derive(Clone)]
pub struct AuthenticatedSession {
    pub username: String,
    token_sha256: String,
}

#[derive(Default)]
pub(crate) struct LoginThrottle {
    global_attempts: VecDeque<Instant>,
    attempts_by_source: BTreeMap<String, VecDeque<Instant>>,
}

impl LoginThrottle {
    pub(crate) const SOURCE_LIMIT: usize = 5;
    pub(crate) const GLOBAL_LIMIT: usize = 100;
    pub(crate) const MAX_TRACKED_SOURCES: usize = 64;
    const WINDOW: StdDuration = StdDuration::from_secs(60);
    const OVERFLOW_SOURCE: &'static str = "overflow";

    fn accept(&mut self, now: Instant, source: String) -> Result<LoginReservation, ApiError> {
        retain_recent(&mut self.global_attempts, now, Self::WINDOW);
        self.attempts_by_source.retain(|_, attempts| {
            retain_recent(attempts, now, Self::WINDOW);
            !attempts.is_empty()
        });
        if self.global_attempts.len() >= Self::GLOBAL_LIMIT {
            return Err(ApiError::too_many_requests(
                "全局登录尝试过于频繁，请稍后重试",
            ));
        }
        let source = if self.attempts_by_source.contains_key(&source)
            || self.attempts_by_source.len() < Self::MAX_TRACKED_SOURCES - 1
        {
            source
        } else {
            Self::OVERFLOW_SOURCE.to_owned()
        };
        let attempts = self.attempts_by_source.entry(source.clone()).or_default();
        if attempts.len() >= Self::SOURCE_LIMIT {
            return Err(ApiError::too_many_requests("登录尝试过于频繁，请稍后重试"));
        }
        attempts.push_back(now);
        self.global_attempts.push_back(now);
        Ok(LoginReservation {
            source,
            recorded_at: now,
        })
    }

    fn release(&mut self, reservation: LoginReservation) {
        if let Some(attempts) = self.attempts_by_source.get_mut(&reservation.source) {
            if let Some(index) = attempts
                .iter()
                .position(|attempt| *attempt == reservation.recorded_at)
            {
                attempts.remove(index);
            }
            if attempts.is_empty() {
                self.attempts_by_source.remove(&reservation.source);
            }
        }
        if let Some(index) = self
            .global_attempts
            .iter()
            .position(|attempt| *attempt == reservation.recorded_at)
        {
            self.global_attempts.remove(index);
        }
    }
}

struct LoginReservation {
    source: String,
    recorded_at: Instant,
}

fn retain_recent(attempts: &mut VecDeque<Instant>, now: Instant, window: StdDuration) {
    while attempts
        .front()
        .is_some_and(|attempt| now.duration_since(*attempt) >= window)
    {
        attempts.pop_front();
    }
}

fn login_source(headers: &HeaderMap) -> String {
    headers
        .get(TRUSTED_CLIENT_IP_HEADER_NAME)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<IpAddr>().ok())
        .map(|address| address.to_string())
        .unwrap_or_else(|| "direct".into())
}

pub async fn authorize_management_request(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let session = match require_session(&state, request.headers()).await {
        Ok(session) => session,
        Err(error)
            if error.status == StatusCode::UNAUTHORIZED
                && matches!(method, Method::GET | Method::HEAD)
                && is_management_html_path(&path) =>
        {
            return Ok(Redirect::to("/login").into_response());
        }
        Err(error) => return Err(error),
    };
    if is_state_changing(&method) {
        require_origin(&state, request.headers())?;
        require_csrf(request.headers())?;
        touch_session(&state, &session.token_sha256).await?;
    }
    request.extensions_mut().insert(session);
    Ok(next.run(request).await)
}

async fn require_session(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<AuthenticatedSession, ApiError> {
    let token = session_token(headers).ok_or_else(|| ApiError::unauthorized("缺少登录会话"))?;
    let token_sha256 = sha256(token);
    let now = Utc::now();
    let idle_cutoff = now
        .checked_sub_signed(Duration::minutes(state.config.session_idle_minutes))
        .ok_or_else(|| ApiError::internal("会话空闲期限计算溢出"))?;
    let row = sqlx::query("SELECT administrators.username FROM sessions JOIN administrators ON administrators.id = sessions.administrator_id WHERE sessions.token_sha256 = ? AND sessions.expires_at > ? AND sessions.last_seen_at > ?")
        .bind(&token_sha256)
        .bind(now)
        .bind(idle_cutoff)
        .fetch_optional(&state.database)
        .await
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::unauthorized("会话无效或已过期"))?;
    Ok(AuthenticatedSession {
        username: row.get("username"),
        token_sha256,
    })
}

fn require_origin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
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

async fn touch_session(state: &AppState, token_sha256: &str) -> Result<(), ApiError> {
    sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_sha256 = ?")
        .bind(Utc::now())
        .bind(token_sha256)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(())
}

#[derive(Deserialize)]
pub struct LoginRequest {
    username: String,
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    require_origin(&state, &headers)?;
    let reservation = state
        .login_throttle
        .lock()
        .await
        .accept(Instant::now(), login_source(&headers))?;
    let verification_permit = state
        .password_verification_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|error| ApiError::internal(format!("密码校验许可已关闭：{error}")))?;
    let row = sqlx::query(
        "SELECT username, password_hash FROM administrators WHERE id = 1 AND username = ?",
    )
    .bind(request.username.trim())
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?
    .ok_or_else(|| ApiError::unauthorized("用户名或密码错误"))?;
    let password = request.password;
    let password_hash: String = row.get("password_hash");
    let password_matches = tokio::task::spawn_blocking(move || {
        let _verification_permit = verification_permit;
        credentials::verify_password(&password, &password_hash)
    })
    .await
    .map_err(|error| ApiError::internal(format!("密码校验任务异常结束：{error}")))?;
    if !password_matches {
        return Err(ApiError::unauthorized("用户名或密码错误"));
    }
    state.login_throttle.lock().await.release(reservation);
    let token = create_session(&state).await?;
    let cookie = session_cookie(
        &token,
        state
            .config
            .session_absolute_seconds()
            .map_err(ApiError::internal)?,
    );
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(ApiError::internal)?,
    );
    Ok((response_headers, StatusCode::NO_CONTENT))
}

async fn create_session(state: &AppState) -> Result<String, ApiError> {
    let mut token_bytes = [0_u8; 32];
    OsRng.fill_bytes(&mut token_bytes);
    let token = hex::encode(token_bytes);
    let now = Utc::now();
    let lifetime = Duration::try_hours(state.config.session_absolute_hours)
        .ok_or_else(|| ApiError::internal("绝对会话期限无法转换"))?;
    let expires_at = now
        .checked_add_signed(lifetime)
        .ok_or_else(|| ApiError::internal("绝对会话过期时间溢出"))?;
    sqlx::query("INSERT INTO sessions(token_sha256, administrator_id, created_at, expires_at, last_seen_at) VALUES (?, 1, ?, ?, ?)")
        .bind(sha256(&token))
        .bind(now)
        .bind(expires_at)
        .bind(now)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    Ok(token)
}

pub async fn logout(
    State(state): State<AppState>,
    Extension(session): Extension<AuthenticatedSession>,
) -> Result<impl IntoResponse, ApiError> {
    sqlx::query("DELETE FROM sessions WHERE token_sha256 = ?")
        .bind(session.token_sha256)
        .execute(&state.database)
        .await
        .map_err(ApiError::internal)?;
    let mut headers = HeaderMap::new();
    headers.insert(
        SET_COOKIE,
        HeaderValue::from_str(&expired_session_cookie()).map_err(ApiError::internal)?,
    );
    Ok((headers, StatusCode::NO_CONTENT))
}

fn sha256(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

fn session_cookie(token: &str, max_age_seconds: i64) -> String {
    format!(
        "{SESSION_COOKIE_NAME}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age_seconds}"
    )
}

fn expired_session_cookie() -> String {
    session_cookie("", 0)
}

fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find_map(|(key, value)| (key == SESSION_COOKIE_NAME).then_some(value))
}

fn is_state_changing(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD)
}

fn is_management_html_path(path: &str) -> bool {
    if path == "/manage" {
        return true;
    }
    let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
    matches!(segments.as_slice(), ["manage", "packages", _, "reviews", _])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_has_the_fixed_host_only_contract() {
        assert_eq!(
            session_cookie("secret", 3600),
            "__Host-aursmith_session=secret; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age=3600"
        );
        assert!(!session_cookie("secret", 3600).contains("Domain="));
    }

    #[test]
    fn throttle_is_source_isolated_bounded_and_releases_successes() {
        let now = Instant::now();
        let mut throttle = LoginThrottle::default();
        for _ in 0..LoginThrottle::SOURCE_LIMIT + 2 {
            let successful = throttle.accept(now, "successful".into()).unwrap();
            throttle.release(successful);
        }
        assert!(throttle.accept(now, "successful".into()).is_ok());
        for _ in 0..LoginThrottle::SOURCE_LIMIT {
            throttle.accept(now, "source-a".into()).unwrap();
        }
        assert!(throttle.accept(now, "source-a".into()).is_err());
        assert!(throttle.accept(now, "source-b".into()).is_ok());

        let mut bounded = LoginThrottle::default();
        for index in 0..1_000 {
            let _ = bounded.accept(now, format!("source-{index}"));
        }
        assert!(bounded.attempts_by_source.len() <= LoginThrottle::MAX_TRACKED_SOURCES);
        assert!(bounded.global_attempts.len() <= LoginThrottle::GLOBAL_LIMIT);

        let mut globally_limited = LoginThrottle::default();
        for source in 0..20 {
            for _ in 0..LoginThrottle::SOURCE_LIMIT {
                globally_limited
                    .accept(now, format!("global-source-{source}"))
                    .unwrap();
            }
        }
        assert_eq!(
            globally_limited.global_attempts.len(),
            LoginThrottle::GLOBAL_LIMIT
        );
        assert!(
            globally_limited
                .accept(now, "one-more-source".into())
                .is_err()
        );
    }

    #[test]
    fn only_management_html_paths_use_login_redirects() {
        assert!(is_management_html_path("/manage"));
        assert!(is_management_html_path(
            "/manage/packages/paru/reviews/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        ));
        assert!(!is_management_html_path("/manage/packages"));
        assert!(!is_management_html_path("/manage/packages/paru/refresh"));
    }
}
