//! 线协议：家用 Builder ↔ aursmithd（HTTPS），以及 aursmithd ↔ 签名器（共享目录）。
//!
//! 所有结构都 `deny_unknown_fields`，任何一侧多发或少发字段都会被拒绝。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Cloudflare 免费/Pro 套餐单个请求体上限是 100 MB；上传分片固定为 32 MiB，留足余量。
pub const UPLOAD_CHUNK_BYTES: u64 = 32 * 1024 * 1024;
/// 单个软件包产物上限。
pub const MAXIMUM_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// 回传日志上限。
pub const MAXIMUM_LOG_BYTES: usize = 128 * 1024;
/// 同一构建最多自动重试的瞬态失败次数（不含首次）。
pub const MAXIMUM_TRANSIENT_RETRIES: i64 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseRequest {
    pub builder_id: String,
    /// Builder 申请的租约时长（构建超时 + 上传余量），服务端会裁剪到允许范围。
    pub lease_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseResponse {
    pub build: Option<BuildSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildSpec {
    pub build_id: String,
    pub attempt: i64,
    pub package_base: String,
    pub revision_id: String,
    pub aur_commit: String,
    pub vcs_commit: Option<String>,
    pub tree_sha256: String,
    pub expected_outputs: Vec<String>,
    pub allow_check: bool,
    pub files: Vec<InputFile>,
    pub dependencies: Vec<DependencyArtifact>,
    pub lease_expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub content_base64: String,
}

/// 同一期望状态中、已构建但可能尚未发布的 AUR 依赖。Builder 从 aursmithd 下载并校验。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyArtifact {
    pub build_id: String,
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRecord {
    pub file: String,
    pub sha256: String,
    pub size: u64,
    pub package_name: String,
    pub package_version: String,
    pub architecture: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildOutcome {
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// 网络、Docker 守护进程、租约过期等：自动重试至多两次。
    Transient,
    /// 校验和、PGP、check()、打包失败、构建超时（BUILD_TIMEOUT）等：重试无意义。
    Deterministic,
    /// Builder 或部署配置错误：需要运维处理。
    Config,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Deterministic => "deterministic",
            Self::Config => "config",
        }
    }
}

/// 失败码到失败类别的唯一映射。
pub fn classify_error_code(code: &str) -> ErrorClass {
    match code {
        "DOCKER_DAEMON_UNAVAILABLE"
        | "DOCKER_PULL_FAILED"
        | "DOCKER_NETWORK_TIMEOUT"
        | "BUILD_NETWORK_TRANSIENT"
        | "BUILDER_RESTARTED"
        | "LEASE_EXPIRED"
        | "UPLOAD_FAILED"
        | "DEPENDENCY_DOWNLOAD_FAILED" => ErrorClass::Transient,
        "BUILD_IMAGE_MISSING"
        | "BUILDER_CONFIG"
        | "INPUT_INVALID"
        | "DOCKER_PERMISSION_DENIED"
        | "DOCKER_CLI_UNAVAILABLE"
        | "DOCKER_DISK_FULL" => ErrorClass::Config,
        _ => ErrorClass::Deterministic,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildReport {
    pub attempt: i64,
    pub outcome: BuildOutcome,
    pub artifacts: Vec<ArtifactRecord>,
    pub error_code: Option<String>,
    pub log: String,
    pub log_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadStatus {
    pub file: String,
    pub size: u64,
    pub received: u64,
    pub complete: bool,
}

/// 构建容器内 `aursmith-builder guest` 读取的规格（不含任何凭据）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestSpec {
    pub build_id: String,
    pub package_base: String,
    pub expected_outputs: Vec<String>,
    pub allow_check: bool,
    pub dependency_files: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestResult {
    pub build_id: String,
    pub artifacts: Vec<ArtifactRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuestError {
    pub code: String,
    pub error: String,
}

/// aursmithd 写给签名器的期望仓库状态。没有时间戳，因此同一期望状态得到同一摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishPlan {
    pub format: u32,
    pub repository_name: String,
    /// 按文件名排序的完整期望软件包集合。
    pub artifacts: Vec<ArtifactRecord>,
}

pub const PUBLISH_PLAN_FORMAT: u32 = 1;

impl PublishPlan {
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("PublishPlan 序列化不会失败")
    }

    pub fn sha256(&self) -> String {
        crate::sha256_hex(&self.canonical_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub file: String,
    pub sha256: String,
    pub size: u64,
}

/// 签名器写入每个 release 目录并签名的清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishManifest {
    pub format: u32,
    pub plan_sha256: String,
    pub repository_name: String,
    pub artifacts: Vec<ArtifactRecord>,
    pub repository_keyring: ArtifactRecord,
    pub keyring_fingerprint: String,
    pub database: FileEntry,
    pub files_database: FileEntry,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignerState {
    Published,
    Failed,
}

/// 签名器写回 `outbox/<plan_sha256>.json` 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerResult {
    pub plan_sha256: String,
    pub state: SignerState,
    pub error: Option<String>,
    pub manifest_sha256: Option<String>,
    pub keyring_fingerprint: Option<String>,
    pub finished_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_digest_is_stable_and_content_addressed() {
        let artifact = ArtifactRecord {
            file: "demo-1-1-x86_64.pkg.tar.zst".into(),
            sha256: "a".repeat(64),
            size: 1,
            package_name: "demo".into(),
            package_version: "1-1".into(),
            architecture: "x86_64".into(),
        };
        let plan = PublishPlan {
            format: PUBLISH_PLAN_FORMAT,
            repository_name: "aursmith".into(),
            artifacts: vec![artifact.clone()],
        };
        assert_eq!(plan.sha256(), plan.clone().sha256());
        let mut other = plan.clone();
        other.artifacts[0].sha256 = "b".repeat(64);
        assert_ne!(plan.sha256(), other.sha256());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let text = r#"{"builder_id":"home","lease_seconds":60,"token":"x"}"#;
        assert!(serde_json::from_str::<LeaseRequest>(text).is_err());
    }

    #[test]
    fn error_classes_are_exhaustive_and_conservative() {
        assert_eq!(classify_error_code("LEASE_EXPIRED"), ErrorClass::Transient);
        assert_eq!(
            classify_error_code("BUILD_IMAGE_MISSING"),
            ErrorClass::Config
        );
        assert_eq!(
            classify_error_code("GUEST_CHECKSUM_FAILED"),
            ErrorClass::Deterministic
        );
        assert_eq!(
            classify_error_code("SOMETHING_NEW"),
            ErrorClass::Deterministic
        );
    }
}
