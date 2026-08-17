use anyhow::{Context, bail};
use std::{env, fs, fs::OpenOptions, io::Write, os::unix::fs::OpenOptionsExt, path::Path};

const MAXIMUM_SESSION_IDLE_MINUTES: i64 = 7 * 24 * 60;
const MAXIMUM_SESSION_ABSOLUTE_HOURS: i64 = 365 * 24;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub public_origin: String,
    pub signing_key_file: String,
    pub ssh_identity_source_file: String,
    pub ssh_identity_file: String,
    pub ssh_known_hosts_file: String,
    pub session_idle_minutes: i64,
    pub session_absolute_hours: i64,
    pub low_agent_endpoints: Vec<String>,
    pub high_agent_endpoint: String,
    pub agent_daily_call_limit: i64,
    pub agent_monthly_call_limit: i64,
    pub agent_monthly_cost_limit_microusd: i64,
    pub agent_random_high_cost_review_basis_points: i64,
    pub repository_name: String,
    pub source_git_commit: String,
    pub repository_base_url: String,
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
        Ok(Self {
            bind_address: env::var("AURSMITH_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: env::var("AURSMITH_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://runtime/controller.db".into()),
            public_origin,
            signing_key_file: env::var("AURSMITH_SIGNING_KEY_FILE")
                .unwrap_or_else(|_| "/run/secrets/controller_signing_key".into()),
            ssh_identity_source_file: env::var("AURSMITH_SSH_IDENTITY_SOURCE_FILE")
                .unwrap_or_else(|_| "/run/secrets/worker_ssh_key".into()),
            ssh_identity_file: env::var("AURSMITH_SSH_IDENTITY_FILE")
                .unwrap_or_else(|_| "/run/aursmith-private/worker_ssh_key".into()),
            ssh_known_hosts_file: env::var("AURSMITH_SSH_KNOWN_HOSTS_FILE")
                .unwrap_or_else(|_| "/run/secrets/worker_known_hosts".into()),
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
            agent_daily_call_limit: parse_nonnegative("AURSMITH_AGENT_DAILY_CALL_LIMIT", 300),
            agent_monthly_call_limit: parse_nonnegative("AURSMITH_AGENT_MONTHLY_CALL_LIMIT", 3000),
            agent_monthly_cost_limit_microusd: parse_nonnegative(
                "AURSMITH_AGENT_MONTHLY_COST_LIMIT_MICROUSD",
                5_000_000,
            ),
            agent_random_high_cost_review_basis_points: parse_bounded_nonnegative(
                "AURSMITH_AGENT_RANDOM_HIGH_COST_REVIEW_BASIS_POINTS",
                0,
                10_000,
            ),
            repository_name: env::var("AURSMITH_REPOSITORY_NAME")
                .unwrap_or_else(|_| "aursmith".into()),
            source_git_commit: env::var("AURSMITH_SOURCE_GIT_COMMIT")
                .unwrap_or_else(|_| "development".into()),
            repository_base_url: env::var("AURSMITH_REPOSITORY_BASE_URL")
                .unwrap_or_else(|_| "https://repo.aursmith.lan".into()),
        })
    }

    pub fn load_signing_key(&self) -> anyhow::Result<ed25519_dalek::SigningKey> {
        let value = fs::read_to_string(&self.signing_key_file)
            .with_context(|| format!("无法读取 Controller 签名密钥 {}", self.signing_key_file))?;
        let bytes = hex::decode(value.trim()).context("Controller 签名密钥不是十六进制")?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("Controller 签名密钥必须是 32 字节"))?;
        Ok(ed25519_dalek::SigningKey::from_bytes(&bytes))
    }

    pub fn materialize_ssh_identity(&self) -> anyhow::Result<()> {
        let source = Path::new(&self.ssh_identity_source_file);
        let target = Path::new(&self.ssh_identity_file);
        let metadata = fs::symlink_metadata(source)
            .with_context(|| format!("无法检查 Worker SSH 私钥 {}", source.display()))?;
        if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > 64 * 1024 {
            bail!("Worker SSH 私钥类型或大小不合法");
        }
        let bytes = fs::read(source)
            .with_context(|| format!("无法读取 Worker SSH 私钥 {}", source.display()))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)
            .with_context(|| format!("无法创建私有 SSH 密钥 {}", target.display()))?;
        file.write_all(&bytes)
            .with_context(|| format!("无法写入私有 SSH 密钥 {}", target.display()))?;
        file.sync_all()
            .with_context(|| format!("无法同步私有 SSH 密钥 {}", target.display()))?;
        Ok(())
    }
}

fn parse_nonnegative(name: &str, default: i64) -> i64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value >= 0)
        .unwrap_or(default)
}

fn parse_bounded_nonnegative(name: &str, default: i64, maximum: i64) -> i64 {
    parse_nonnegative(name, default).min(maximum)
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
