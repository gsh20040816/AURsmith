//! aursmithd Builder API 客户端（HTTPS + Bearer）。

use anyhow::{Context, bail};
use aursmith_core::protocol::{BuildReport, BuildSpec, LeaseRequest, LeaseResponse, UploadStatus};
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use std::{path::Path, time::Duration};
use tokio::io::AsyncWriteExt;
use url::Url;

/// 服务端返回的错误。`status` 为 409 表示构建已不属于本 Builder（租约失效或状态已变），
/// 此时应当丢弃本地工作目录而不是重试。
#[derive(Debug)]
pub struct ServerError {
    pub status: StatusCode,
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "HTTP {} {}: {}",
            self.status.as_u16(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for ServerError {}

impl ServerError {
    pub fn is_conflict(&self) -> bool {
        self.status == StatusCode::CONFLICT
    }
}

/// 判断错误是否意味着构建已不再属于本 Builder。
pub fn is_gone(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<ServerError>()
        .is_some_and(|error| error.is_conflict() || error.status == StatusCode::NOT_FOUND)
}

pub fn server_code(error: &anyhow::Error) -> Option<&str> {
    error
        .downcast_ref::<ServerError>()
        .map(|error| error.code.as_str())
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: Url,
    token: String,
}

impl Client {
    pub fn new(base: Url, token: String) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .timeout(Duration::from_secs(600))
            .user_agent(concat!("aursmith-builder/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, base, token })
    }

    fn url(&self, path: &str) -> anyhow::Result<Url> {
        self.base
            .join(path)
            .with_context(|| format!("无法拼接 URL {path}"))
    }

    async fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<reqwest::Body>,
        json: Option<&serde_json::Value>,
    ) -> anyhow::Result<T> {
        let mut request = self
            .http
            .request(method, self.url(path)?)
            .bearer_auth(&self.token);
        if let Some(json) = json {
            request = request.json(json);
        }
        if let Some(body) = body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(body);
        }
        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
            return Err(ServerError {
                status,
                code: value["code"].as_str().unwrap_or("HTTP_ERROR").to_owned(),
                message: value["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).into_owned()
                    }),
            }
            .into());
        }
        serde_json::from_slice(&bytes).with_context(|| format!("{path} 返回了无法解析的 JSON"))
    }

    pub async fn lease(
        &self,
        builder_id: &str,
        lease_seconds: u64,
    ) -> anyhow::Result<Option<BuildSpec>> {
        let request = serde_json::to_value(LeaseRequest {
            builder_id: builder_id.to_owned(),
            lease_seconds,
        })?;
        let response: LeaseResponse = self
            .send(Method::POST, "/api/v1/builder/lease", None, Some(&request))
            .await?;
        Ok(response.build)
    }

    pub async fn report(
        &self,
        build_id: &str,
        report: &BuildReport,
    ) -> anyhow::Result<serde_json::Value> {
        let body = serde_json::to_value(report)?;
        self.send(
            Method::POST,
            &format!("/api/v1/builder/builds/{build_id}/report"),
            None,
            Some(&body),
        )
        .await
    }

    pub async fn upload_status(&self, build_id: &str, file: &str) -> anyhow::Result<UploadStatus> {
        self.send(
            Method::GET,
            &format!("/api/v1/builder/builds/{build_id}/files/{file}"),
            None,
            None,
        )
        .await
    }

    pub async fn upload_chunk(
        &self,
        build_id: &str,
        file: &str,
        offset: u64,
        chunk: Vec<u8>,
    ) -> anyhow::Result<UploadStatus> {
        self.send(
            Method::PUT,
            &format!("/api/v1/builder/builds/{build_id}/files/{file}?offset={offset}"),
            Some(chunk.into()),
            None,
        )
        .await
    }

    pub async fn complete(&self, build_id: &str) -> anyhow::Result<serde_json::Value> {
        self.send(
            Method::POST,
            &format!("/api/v1/builder/builds/{build_id}/complete"),
            None,
            Some(&serde_json::json!({})),
        )
        .await
    }

    /// 下载依赖产物到 `destination`（先写 `.part`，大小与摘要都正确后再改名）。
    pub async fn download(
        &self,
        build_id: &str,
        file: &str,
        expected_size: u64,
        expected_sha256: &str,
        destination: &Path,
    ) -> anyhow::Result<()> {
        use futures_util::StreamExt as _;
        use sha2::{Digest, Sha256};
        let response = self
            .http
            .get(self.url(&format!("/api/v1/builder/artifacts/{build_id}/{file}"))?)
            .bearer_auth(&self.token)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "依赖产物 {file} 下载失败：HTTP {}",
                response.status().as_u16()
            );
        }
        let partial = std::path::PathBuf::from(format!("{}.part", destination.display()));
        let mut output = tokio::fs::File::create(&partial).await?;
        let mut hasher = Sha256::new();
        let mut received = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            received += chunk.len() as u64;
            if received > expected_size {
                bail!("依赖产物 {file} 超过声明大小");
            }
            hasher.update(&chunk);
            output.write_all(&chunk).await?;
        }
        output.sync_all().await?;
        if received != expected_size || hex::encode(hasher.finalize()) != expected_sha256 {
            let _ = tokio::fs::remove_file(&partial).await;
            bail!("依赖产物 {file} 大小或 SHA-256 不一致");
        }
        tokio::fs::rename(&partial, destination).await?;
        Ok(())
    }
}
