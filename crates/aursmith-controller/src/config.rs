use anyhow::{Context, bail};
use std::{env, fs};

#[derive(Debug, Clone)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub setup_token: String,
    pub signing_key_file: String,
    pub ssh_identity_file: String,
    pub ssh_known_hosts_file: String,
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
            signing_key_file: env::var("AURSMITH_SIGNING_KEY_FILE")
                .unwrap_or_else(|_| "/run/secrets/controller_signing_key".into()),
            ssh_identity_file: env::var("AURSMITH_SSH_IDENTITY_FILE")
                .unwrap_or_else(|_| "/run/secrets/worker_ssh_key".into()),
            ssh_known_hosts_file: env::var("AURSMITH_SSH_KNOWN_HOSTS_FILE")
                .unwrap_or_else(|_| "/run/secrets/worker_known_hosts".into()),
            secure_cookies: env::var("AURSMITH_SECURE_COOKIES")
                .map(|value| value != "false")
                .unwrap_or(true),
            session_hours: env::var("AURSMITH_SESSION_HOURS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(168),
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
}
