use anyhow::{Context, bail};
use std::{env, fs};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub setup_token: String,
    pub secure_cookies: bool,
    pub session_hours: i64,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let setup_token = match env::var("AURSMITH_SETUP_TOKEN") {
            Ok(value) => value,
            Err(_) => {
                let path = env::var("AURSMITH_SETUP_TOKEN_FILE").context(
                    "必须通过 secret 设置 AURSMITH_SETUP_TOKEN 或 AURSMITH_SETUP_TOKEN_FILE",
                )?;
                fs::read_to_string(&path)
                    .with_context(|| format!("无法读取初始化令牌文件 {path}"))?
                    .trim()
                    .to_owned()
            }
        };
        if setup_token.len() < 20 {
            bail!("AURSMITH_SETUP_TOKEN 至少需要 20 个字符");
        }
        Ok(Self {
            bind_address: env::var("AURSMITH_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into()),
            database_url: env::var("AURSMITH_DATABASE_URL")
                .unwrap_or_else(|_| "sqlite://runtime/controller.db".into()),
            setup_token,
            secure_cookies: env::var("AURSMITH_SECURE_COOKIES")
                .map(|value| value != "false")
                .unwrap_or(true),
            session_hours: env::var("AURSMITH_SESSION_HOURS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(168),
        })
    }
}
