use anyhow::{Context, bail};
use std::{env, path::Path};

const MAXIMUM_SESSION_IDLE_MINUTES: i64 = 7 * 24 * 60;
const MAXIMUM_SESSION_ABSOLUTE_HOURS: i64 = 365 * 24;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub public_origin: String,
    pub session_idle_minutes: i64,
    pub session_absolute_hours: i64,
    pub low_agent_endpoints: Vec<String>,
    pub high_agent_endpoint: String,
    pub repository_name: String,
    pub source_git_commit: String,
    pub repository_base_url: String,
    pub builder_token_sha256: String,
    pub builder_max_concurrent: u16,
    pub update_interval_minutes: u32,
    pub publisher_socket: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let public_origin = validate_public_origin(
            &env::var("AURSMITH_PUBLIC_ORIGIN")
                .context("必须设置管理站点的固定 HTTPS Origin：AURSMITH_PUBLIC_ORIGIN")?,
        )?;
        let session_idle_minutes = parse_bounded_positive(
            "AURSMITH_SESSION_IDLE_MINUTES",
            60,
            MAXIMUM_SESSION_IDLE_MINUTES,
        )?;
        let session_absolute_hours = parse_bounded_positive(
            "AURSMITH_SESSION_ABSOLUTE_HOURS",
            168,
            MAXIMUM_SESSION_ABSOLUTE_HOURS,
        )?;
        validate_session_durations(session_idle_minutes, session_absolute_hours)?;
        let builder_token_sha256 = env::var("AURSMITH_BUILDER_TOKEN_SHA256").context(
            "必须设置固定 Builder Bearer secret 的 SHA-256：AURSMITH_BUILDER_TOKEN_SHA256",
        )?;
        if builder_token_sha256.len() != 64
            || !builder_token_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("AURSMITH_BUILDER_TOKEN_SHA256 必须是 64 位十六进制 SHA-256");
        }
        let builder_max_concurrent = u16::try_from(parse_bounded_positive(
            "AURSMITH_BUILDER_MAX_CONCURRENT",
            1,
            16,
        )?)?;
        let update_interval_minutes = u32::try_from(parse_bounded_positive(
            "AURSMITH_UPDATE_INTERVAL_MINUTES",
            30,
            10_080,
        )?)?;
        Ok(Self {
            bind_address: env::var("AURSMITH_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: env::var("AURSMITH_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://runtime/controller.db".into()),
            public_origin,
            session_idle_minutes,
            session_absolute_hours,
            low_agent_endpoints: env::var("AURSMITH_LOW_AGENT_ENDPOINTS")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect(),
            high_agent_endpoint: env::var("AURSMITH_HIGH_AGENT_ENDPOINT").unwrap_or_default(),
            repository_name: env::var("AURSMITH_REPOSITORY_NAME")
                .unwrap_or_else(|_| "aursmith".into()),
            source_git_commit: env::var("AURSMITH_SOURCE_GIT_COMMIT")
                .unwrap_or_else(|_| "development".into()),
            repository_base_url: env::var("AURSMITH_REPOSITORY_BASE_URL")
                .unwrap_or_else(|_| "https://repo.aursmith.lan".into()),
            builder_token_sha256: builder_token_sha256.to_ascii_lowercase(),
            builder_max_concurrent,
            update_interval_minutes,
            publisher_socket: validate_publisher_socket(
                &env::var("AURSMITH_PUBLISHER_SOCKET")
                    .unwrap_or_else(|_| "/run/aursmith-publisher/worker.sock".into()),
            )?,
        })
    }
}

fn validate_publisher_socket(value: &str) -> anyhow::Result<String> {
    let path = Path::new(value);
    if !path.is_absolute() || value.as_bytes().contains(&0) {
        bail!("AURSMITH_PUBLISHER_SOCKET 必须是绝对路径");
    }
    Ok(value.to_owned())
}

fn parse_bounded_positive(name: &str, default: i64, maximum: i64) -> anyhow::Result<i64> {
    match env::var(name) {
        Ok(value) => parse_bounded_positive_value(name, Some(&value), default, maximum),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} 必须是 UTF-8 正整数"),
    }
}

fn parse_bounded_positive_value(
    name: &str,
    value: Option<&str>,
    default: i64,
    maximum: i64,
) -> anyhow::Result<i64> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed: i64 = value
        .parse()
        .with_context(|| format!("{name} 必须是正整数"))?;
    if !(1..=maximum).contains(&parsed) {
        bail!("{name} 必须在 1 至 {maximum} 之间");
    }
    Ok(parsed)
}

fn validate_session_durations(idle_minutes: i64, absolute_hours: i64) -> anyhow::Result<()> {
    let absolute_minutes = absolute_hours
        .checked_mul(60)
        .context("绝对会话期限换算溢出")?;
    if idle_minutes > absolute_minutes {
        bail!("AURSMITH_SESSION_IDLE_MINUTES 不能超过绝对会话期限");
    }
    Ok(())
}

fn validate_public_origin(value: &str) -> anyhow::Result<String> {
    let origin = url::Url::parse(value.trim()).context("AURSMITH_PUBLIC_ORIGIN 不是有效 URL")?;
    if origin.scheme() != "https"
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.path() != "/"
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        bail!("AURSMITH_PUBLIC_ORIGIN 必须是无凭据、路径、查询参数和片段的 HTTPS Origin");
    }
    Ok(origin.origin().ascii_serialization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_origin_is_https_and_contains_no_url_tail() {
        assert_eq!(
            validate_public_origin("https://aursmith.example:8443").unwrap(),
            "https://aursmith.example:8443"
        );
        for invalid in [
            "http://aursmith.example",
            "https://user@aursmith.example",
            "https://aursmith.example/admin",
            "https://aursmith.example/?debug=1",
        ] {
            assert!(validate_public_origin(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn session_durations_reject_invalid_or_excessive_values() {
        assert_eq!(
            parse_bounded_positive_value("SESSION", None, 60, 120).unwrap(),
            60
        );
        assert_eq!(
            parse_bounded_positive_value("SESSION", Some("120"), 60, 120).unwrap(),
            120
        );
        for invalid in ["0", "-1", "121", "not-a-number"] {
            assert!(
                parse_bounded_positive_value("SESSION", Some(invalid), 60, 120).is_err(),
                "{invalid}"
            );
        }
        assert!(validate_session_durations(60, 1).is_ok());
        assert!(validate_session_durations(61, 1).is_err());
    }
}
