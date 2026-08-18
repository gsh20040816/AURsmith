use crate::{config::Config, error::ApiError};
use aursmith_protocol::{BuilderUpload, ReleasePlan, ReleaseRollbackRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
    time::timeout,
};

#[derive(Debug, Deserialize)]
pub struct WorkerReply {
    pub ok: bool,
    pub code: String,
    pub message: String,
    pub data: Value,
}

pub async fn prepare_push_import(
    config: &Config,
    upload: &BuilderUpload,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(upload).map_err(ApiError::internal)?;
    invoke(config, "prepare-push-import", Some(body)).await
}

pub async fn authorize_release(
    config: &Config,
    plan: &ReleasePlan,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(plan).map_err(ApiError::internal)?;
    invoke(config, "authorize-release", Some(body)).await
}

pub async fn authorize_rollback(
    config: &Config,
    request: &ReleaseRollbackRequest,
) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(request).map_err(ApiError::internal)?;
    invoke_with_timeout(
        config,
        "authorize-rollback",
        Some(body),
        Duration::from_secs(90),
    )
    .await
}

pub async fn query_release(config: &Config, release_id: &str) -> Result<WorkerReply, ApiError> {
    invoke(
        config,
        "query-release",
        Some(release_id.as_bytes().to_vec()),
    )
    .await
}

pub async fn aur_search(config: &Config, query: &str) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"query": query})).map_err(ApiError::internal)?;
    invoke(config, "aur-search", Some(body)).await
}

pub async fn aur_info(config: &Config, names: &[String]) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"names": names})).map_err(ApiError::internal)?;
    invoke(config, "aur-info", Some(body)).await
}

pub async fn aur_snapshot(config: &Config, package_base: &str) -> Result<WorkerReply, ApiError> {
    let body = serde_json::to_vec(&serde_json::json!({"package_base": package_base}))
        .map_err(ApiError::internal)?;
    invoke(config, "aur-snapshot", Some(body)).await
}

pub async fn aur_providers(config: &Config, names: &[String]) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"names": names})).map_err(ApiError::internal)?;
    invoke(config, "aur-providers", Some(body)).await
}

pub async fn official_info(config: &Config, names: &[String]) -> Result<WorkerReply, ApiError> {
    let body =
        serde_json::to_vec(&serde_json::json!({"names": names})).map_err(ApiError::internal)?;
    invoke(config, "official-info", Some(body)).await
}

pub async fn publisher_doctor(config: &Config) -> Result<WorkerReply, ApiError> {
    invoke(config, "publisher-doctor", None).await
}

pub async fn publisher_status(config: &Config) -> Result<WorkerReply, ApiError> {
    invoke(config, "status", None).await
}

async fn invoke(
    config: &Config,
    remote_command: &str,
    stdin: Option<Vec<u8>>,
) -> Result<WorkerReply, ApiError> {
    invoke_with_timeout(config, remote_command, stdin, Duration::from_secs(20)).await
}

async fn invoke_with_timeout(
    config: &Config,
    remote_command: &str,
    stdin: Option<Vec<u8>>,
    command_timeout: Duration,
) -> Result<WorkerReply, ApiError> {
    let request = publisher_request(remote_command, stdin)?;
    let exchange = async {
        let mut stream = UnixStream::connect(&config.publisher_socket).await?;
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        stream.write_all(&bytes).await?;
        let mut response = Vec::new();
        stream
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut response)
            .await?;
        if response.len() > 4 * 1024 * 1024 {
            anyhow::bail!("Publisher 响应超过 4 MiB");
        }
        Ok::<_, anyhow::Error>(String::from_utf8(response)?)
    };
    let output = timeout(command_timeout, exchange)
        .await
        .map_err(|_| ApiError::internal("Publisher 本地调用超时"))?
        .map_err(ApiError::internal)?;
    let reply: WorkerReply = serde_json::from_str(&output).map_err(ApiError::internal)?;
    if !reply.ok {
        return Err(ApiError::conflict(
            "PUBLISHER_REJECTED",
            format!("{}: {}", reply.code, reply.message),
        ));
    }
    Ok(reply)
}

fn publisher_request(command: &str, body: Option<Vec<u8>>) -> Result<Value, ApiError> {
    let command_name = command.replace('-', "_");
    match command {
        "prepare-push-import" | "authorize-release" | "authorize-rollback" => Ok(json!({
            "command": command_name,
            "envelope": serde_json::from_slice::<Value>(&body.unwrap_or_default()).map_err(ApiError::internal)?,
        })),
        "query-release" => Ok(json!({
            "command": command_name,
            "release_id": String::from_utf8(body.unwrap_or_default()).map_err(ApiError::internal)?,
        })),
        "publisher-doctor" | "status" => Ok(json!({"command": command_name})),
        _ => {
            let mut value: Value =
                serde_json::from_slice(&body.unwrap_or_default()).map_err(ApiError::internal)?;
            value
                .as_object_mut()
                .ok_or_else(|| ApiError::internal("Publisher 请求必须是 JSON 对象"))?
                .insert("command".into(), Value::String(command_name));
            Ok(value)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publisher_envelope_is_wrapped_for_the_local_protocol() {
        let request =
            publisher_request("authorize-release", Some(br#"{"release_id":"x"}"#.to_vec()))
                .unwrap();
        assert_eq!(request["command"], "authorize_release");
        assert_eq!(request["envelope"]["release_id"], "x");
    }

    #[test]
    fn publisher_object_commands_cannot_override_the_command() {
        let request = publisher_request(
            "aur-search",
            Some(br#"{"command":"authorize_release","query":"demo"}"#.to_vec()),
        )
        .unwrap();
        assert_eq!(request["command"], "aur_search");
    }
}
