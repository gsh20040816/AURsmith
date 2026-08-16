use anyhow::{Context, bail};
use std::{net::SocketAddr, path::PathBuf};

const MAXIMUM_SESSION_IDLE_MINUTES: i64 = 7 * 24 * 60;
const MAXIMUM_SESSION_ABSOLUTE_HOURS: i64 = 365 * 24;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub database_path: PathBuf,
    pub public_origin: String,
    pub session_idle_minutes: i64,
    pub session_absolute_hours: i64,
}

impl Config {
    pub fn new(
        bind: SocketAddr,
        database_path: PathBuf,
        public_origin: &str,
        session_idle_minutes: i64,
        session_absolute_hours: i64,
    ) -> anyhow::Result<Self> {
        if !(1..=MAXIMUM_SESSION_IDLE_MINUTES).contains(&session_idle_minutes) {
            bail!("AURSMITH_SESSION_IDLE_MINUTES 必须在 1 至 {MAXIMUM_SESSION_IDLE_MINUTES} 之间");
        }
        if !(1..=MAXIMUM_SESSION_ABSOLUTE_HOURS).contains(&session_absolute_hours) {
            bail!(
                "AURSMITH_SESSION_ABSOLUTE_HOURS 必须在 1 至 {MAXIMUM_SESSION_ABSOLUTE_HOURS} 之间"
            );
        }
        let absolute_minutes = session_absolute_hours
            .checked_mul(60)
            .context("绝对会话期限换算溢出")?;
        if session_idle_minutes > absolute_minutes {
            bail!("AURSMITH_SESSION_IDLE_MINUTES 不能超过绝对会话期限");
        }
        Ok(Self {
            bind,
            database_path,
            public_origin: validate_public_origin(public_origin)?,
            session_idle_minutes,
            session_absolute_hours,
        })
    }

    pub fn session_absolute_seconds(&self) -> anyhow::Result<i64> {
        self.session_absolute_hours
            .checked_mul(3600)
            .context("绝对会话期限换算溢出")
    }

    pub fn aur_state_directory(&self) -> PathBuf {
        self.database_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .join("aur")
    }
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

    fn config(origin: &str, idle: i64, absolute: i64) -> anyhow::Result<Config> {
        Config::new(
            "127.0.0.1:8080".parse().unwrap(),
            "/tmp/aursmith.db".into(),
            origin,
            idle,
            absolute,
        )
    }

    #[test]
    fn public_origin_is_an_exact_https_origin() {
        assert_eq!(
            config("https://aursmith.example:8443", 60, 1)
                .unwrap()
                .public_origin,
            "https://aursmith.example:8443"
        );
        for invalid in [
            "http://aursmith.example",
            "https://user@aursmith.example",
            "https://aursmith.example/admin",
            "https://aursmith.example/?debug=1",
        ] {
            assert!(config(invalid, 60, 1).is_err(), "{invalid}");
        }
    }

    #[test]
    fn session_durations_are_positive_bounded_and_ordered() {
        assert!(config("https://aursmith.example", 1, 1).is_ok());
        assert!(
            config(
                "https://aursmith.example",
                MAXIMUM_SESSION_IDLE_MINUTES,
                MAXIMUM_SESSION_ABSOLUTE_HOURS
            )
            .is_ok()
        );
        for (idle, absolute) in [(0, 1), (-1, 1), (1, 0), (61, 1)] {
            assert!(
                config("https://aursmith.example", idle, absolute).is_err(),
                "idle={idle}, absolute={absolute}"
            );
        }
        assert!(
            config(
                "https://aursmith.example",
                MAXIMUM_SESSION_IDLE_MINUTES + 1,
                MAXIMUM_SESSION_ABSOLUTE_HOURS
            )
            .is_err()
        );
        assert!(
            config(
                "https://aursmith.example",
                1,
                MAXIMUM_SESSION_ABSOLUTE_HOURS + 1
            )
            .is_err()
        );
    }

    #[test]
    fn aur_state_is_derived_from_the_database_parent() {
        assert_eq!(
            config("https://aursmith.example", 60, 1)
                .unwrap()
                .aur_state_directory(),
            std::path::Path::new("/tmp/aur")
        );
    }
}
