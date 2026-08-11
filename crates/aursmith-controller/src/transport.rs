use crate::{config::Config, error::ApiError};
use aursmith_protocol::SignedEnvelope;
use serde::Deserialize;
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command, time::timeout};
use url::Url;

#[derive(Debug, Deserialize)]
pub struct WorkerReply {
    pub ok: bool,
    pub code: String,
    pub message: String,
    pub data: Value,
}

pub async fn status(config: &Config, endpoint: &str) -> Result<WorkerReply, ApiError> {
    invoke(config, endpoint, "status", None).await
}

pub async fn query(config: &Config, endpoint: &str, job_id: &str) -> Result<WorkerReply, ApiError> {
    invoke(config, endpoint, &format!("query {job_id}"), None).await
}

pub async fn submit(
    config: &Config,
    endpoint: &str,
    envelope: &SignedEnvelope,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(envelope).map_err(ApiError::internal)?;
    invoke(config, endpoint, "submit", Some(body)).await
}

pub async fn authorize_export(
    config: &Config,
    endpoint: &str,
    envelope: &SignedEnvelope,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(envelope).map_err(ApiError::internal)?;
    invoke(config, endpoint, "authorize-export", Some(body)).await
}

pub async fn authorize_import(
    config: &Config,
    endpoint: &str,
    envelope: &SignedEnvelope,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(envelope).map_err(ApiError::internal)?;
    invoke(config, endpoint, "authorize-import", Some(body)).await
}

pub async fn complete_export(
    config: &Config,
    endpoint: &str,
    envelope: &SignedEnvelope,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(envelope).map_err(ApiError::internal)?;
    invoke(config, endpoint, "complete-export", Some(body)).await
}

pub async fn authorize_release(
    config: &Config,
    endpoint: &str,
    envelope: &SignedEnvelope,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(envelope).map_err(ApiError::internal)?;
    invoke(config, endpoint, "authorize-release", Some(body)).await
}

pub async fn authorize_rollback(
    config: &Config,
    endpoint: &str,
    envelope: &SignedEnvelope,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(envelope).map_err(ApiError::internal)?;
    invoke(config, endpoint, "authorize-rollback", Some(body)).await
}

pub async fn query_release(
    config: &Config,
    endpoint: &str,
    release_id: &str,
) -> Result<WorkerReply, ApiError> {
    invoke(
        config,
        endpoint,
        "query-release",
        Some(release_id.as_bytes().to_vec()),
    )
    .await
}

pub async fn release_files(
    config: &Config,
    endpoint: &str,
    release_id: &str,
) -> Result<WorkerReply, ApiError> {
    invoke(
        config,
        endpoint,
        "release-files",
        Some(release_id.as_bytes().to_vec()),
    )
    .await
}

pub async fn archive_inventory(
    config: &Config,
    endpoint: &str,
    full_digest: bool,
) -> Result<WorkerReply, ApiError> {
    invoke(
        config,
        endpoint,
        if full_digest {
            "inventory --full-digest"
        } else {
            "inventory"
        },
        None,
    )
    .await
}

pub async fn aur_search(
    config: &Config,
    endpoint: &str,
    query: &str,
) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"query": query})).map_err(ApiError::internal)?;
    invoke(config, endpoint, "aur-search", Some(body)).await
}

pub async fn aur_info(
    config: &Config,
    endpoint: &str,
    names: &[String],
) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"names": names})).map_err(ApiError::internal)?;
    invoke(config, endpoint, "aur-info", Some(body)).await
}

pub async fn aur_snapshot(
    config: &Config,
    endpoint: &str,
    package_base: &str,
    previous_vcs_commit: Option<&str>,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(&serde_json::json!({
        "package_base": package_base,
        "previous_vcs_commit": previous_vcs_commit
    }))
    .map_err(ApiError::internal)?;
    invoke(config, endpoint, "aur-snapshot", Some(body)).await
}

pub async fn aur_providers(
    config: &Config,
    endpoint: &str,
    names: &[String],
) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"names": names})).map_err(ApiError::internal)?;
    invoke(config, endpoint, "aur-providers", Some(body)).await
}

pub async fn official_info(
    config: &Config,
    endpoint: &str,
    names: &[String],
) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"names": names})).map_err(ApiError::internal)?;
    invoke(config, endpoint, "official-info", Some(body)).await
}

pub async fn publisher_doctor(config: &Config, endpoint: &str) -> Result<WorkerReply, ApiError> {
    invoke(config, endpoint, "publisher-doctor", None).await
}

async fn invoke(
    config: &Config,
    endpoint: &str,
    remote_command: &str,
    stdin: Option<Vec<u8>>,
) -> Result<WorkerReply, ApiError> {
    let endpoint = ParsedEndpoint::parse(endpoint)?;
    let mut command = Command::new("ssh");
    command
        .kill_on_drop(true)
        .arg("-T")
        .arg("-p")
        .arg(endpoint.port.to_string())
        .arg("-i")
        .arg(&config.ssh_identity_file)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg(format!(
            "UserKnownHostsFile={}",
            config.ssh_known_hosts_file
        ))
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(format!("{}@{}", endpoint.user, endpoint.host))
        .arg(remote_command)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(ApiError::internal)?;
    if let Some(body) = stdin {
        child
            .stdin
            .take()
            .ok_or_else(|| ApiError::internal("无法打开 ssh stdin"))?
            .write_all(&body)
            .await
            .map_err(ApiError::internal)?;
    }
    let output = timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .map_err(|_| ApiError::internal("Worker SSH 调用超时"))?
        .map_err(ApiError::internal)?;
    let reply = serde_json::from_slice::<WorkerReply>(&output.stdout);
    if !output.status.success() {
        if let Ok(reply) = &reply
            && !reply.ok
        {
            return Err(ApiError::conflict(
                "WORKER_REJECTED",
                format!("{}: {}", reply.code, reply.message),
            ));
        }
        return Err(ApiError::internal(format!(
            "Worker SSH 失败: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let reply = reply.map_err(ApiError::internal)?;
    if !reply.ok {
        return Err(ApiError::conflict(
            "WORKER_REJECTED",
            format!("{}: {}", reply.code, reply.message),
        ));
    }
    Ok(reply)
}

struct ParsedEndpoint {
    host: String,
    port: u16,
    user: String,
}

impl ParsedEndpoint {
    fn parse(value: &str) -> Result<Self, ApiError> {
        let url = Url::parse(value).map_err(|_| {
            ApiError::bad_request("INVALID_ENDPOINT", "Worker 端点必须是 ssh:// URL")
        })?;
        if url.scheme() != "ssh"
            || url.password().is_some()
            || (!url.path().is_empty() && url.path() != "/")
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ApiError::bad_request(
                "INVALID_ENDPOINT",
                "Worker 端点只允许 ssh://user@host:port",
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| ApiError::bad_request("INVALID_ENDPOINT", "Worker 端点缺少主机"))?
            .to_owned();
        let user = if url.username().is_empty() {
            "aursmith".to_owned()
        } else {
            url.username().to_owned()
        };
        Ok(Self {
            host,
            port: url.port().unwrap_or(22),
            user,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_rejects_passwords_and_paths() {
        assert!(ParsedEndpoint::parse("ssh://user:password@host:2222").is_err());
        assert!(ParsedEndpoint::parse("ssh://host:2222/arbitrary").is_err());
    }

    #[test]
    fn endpoint_uses_safe_defaults() {
        let endpoint = ParsedEndpoint::parse("ssh://worker.example").unwrap();
        assert_eq!(endpoint.user, "aursmith");
        assert_eq!(endpoint.port, 22);
    }
}
