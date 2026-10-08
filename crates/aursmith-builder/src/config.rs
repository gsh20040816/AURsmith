//! Builder 配置：全部来自环境变量，令牌从文件读取。

use anyhow::{Context, bail};
use std::path::PathBuf;
use url::Url;

#[derive(Debug, Clone)]
pub struct BuilderConfig {
    pub server_url: Url,
    pub token: String,
    pub builder_id: String,
    pub jobs_dir: PathBuf,
    pub concurrency: usize,
    pub cpus: u16,
    pub memory_mib: u64,
    pub timeout_seconds: u64,
    pub image: String,
    /// docker CLI 路径；测试中替换为假脚本。
    pub docker: PathBuf,
    /// 没有可领取构建时的轮询间隔。
    pub idle_seconds: u64,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn parse<T: std::str::FromStr>(name: &str, default: T) -> anyhow::Result<T> {
    match var(name) {
        Some(value) => value
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{name} 不是有效数值")),
        None => Ok(default),
    }
}

impl BuilderConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let server_url = var("AURSMITH_SERVER_URL").context("必须设置 AURSMITH_SERVER_URL")?;
        let token_file = var("AURSMITH_BUILDER_TOKEN_FILE")
            .unwrap_or_else(|| "/run/secrets/builder_token".into());
        let token = std::fs::read_to_string(&token_file)
            .with_context(|| format!("无法读取 Builder 令牌文件 {token_file}"))?
            .trim()
            .to_owned();
        let config = Self {
            server_url: parse_server_url(&server_url)?,
            token,
            builder_id: var("AURSMITH_BUILDER_ID").unwrap_or_else(|| "home".into()),
            jobs_dir: var("AURSMITH_JOBS_DIR")
                .unwrap_or_else(|| "/var/lib/aursmith-builder/jobs".into())
                .into(),
            concurrency: parse("AURSMITH_BUILD_CONCURRENCY", 1)?,
            cpus: parse("AURSMITH_BUILD_CPUS", 4)?,
            memory_mib: parse("AURSMITH_BUILD_MEMORY_MIB", 8192)?,
            timeout_seconds: parse("AURSMITH_BUILD_TIMEOUT_SECONDS", 4 * 3600)?,
            image: var("AURSMITH_BUILD_IMAGE").unwrap_or_else(|| "aursmith-build:latest".into()),
            docker: "/usr/bin/docker".into(),
            idle_seconds: 15,
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.token.len() < 32 {
            bail!("Builder 令牌至少 32 个字符");
        }
        if self.builder_id.is_empty()
            || self.builder_id.len() > 64
            || !self
                .builder_id
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
        {
            bail!("AURSMITH_BUILDER_ID 只能包含字母数字和 -_.");
        }
        if !self.jobs_dir.is_absolute() {
            bail!("AURSMITH_JOBS_DIR 必须是绝对路径（宿主机与 Builder 容器内路径必须一致）");
        }
        if !(1..=16).contains(&self.concurrency)
            || !(1..=64).contains(&self.cpus)
            || !(256..=262_144).contains(&self.memory_mib)
            || !(60..=86_400).contains(&self.timeout_seconds)
        {
            bail!("构建并发/CPU/内存/超时配置超出允许范围");
        }
        if self.image.is_empty() || self.image.starts_with('-') {
            bail!("AURSMITH_BUILD_IMAGE 无效");
        }
        Ok(())
    }

    /// 申请的租约：构建超时 + 上传余量。服务端会裁剪到 600..172800 秒。
    pub fn lease_seconds(&self) -> u64 {
        self.timeout_seconds + 2 * 3600
    }
}

/// 只接受 HTTPS；明文 HTTP 仅允许回环地址（本机调试与测试）。
pub fn parse_server_url(value: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value.trim()).context("AURSMITH_SERVER_URL 不是有效 URL")?;
    let loopback = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        bail!("AURSMITH_SERVER_URL 必须使用 HTTPS");
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() {
        bail!("AURSMITH_SERVER_URL 不能包含凭据或查询参数");
    }
    Ok(url)
}

#[cfg(test)]
pub fn test_config(server_url: &str, jobs_dir: PathBuf, docker: PathBuf) -> BuilderConfig {
    BuilderConfig {
        server_url: parse_server_url(server_url).unwrap(),
        token: "builder-token-builder-token-builder-token".into(),
        builder_id: "test".into(),
        jobs_dir,
        concurrency: 1,
        cpus: 1,
        memory_mib: 512,
        timeout_seconds: 60,
        image: "aursmith-build:latest".into(),
        docker,
        idle_seconds: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_url_must_be_https_except_loopback() {
        assert!(parse_server_url("https://aursmith.example").is_ok());
        assert!(parse_server_url("http://127.0.0.1:8080").is_ok());
        assert!(parse_server_url("http://aursmith.example").is_err());
        assert!(parse_server_url("https://user:pass@aursmith.example").is_err());
    }

    #[test]
    fn limits_are_validated() {
        let mut config = test_config("https://a.example", "/tmp/jobs".into(), "/bin/true".into());
        assert!(config.validate().is_ok());
        config.concurrency = 0;
        assert!(config.validate().is_err());
        config.concurrency = 1;
        config.jobs_dir = "relative".into();
        assert!(config.validate().is_err());
        config.jobs_dir = "/tmp/jobs".into();
        config.token = "short".into();
        assert!(config.validate().is_err());
    }
}
