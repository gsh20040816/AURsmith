use aursmith_domain::{ArchiveState, AttemptRef, WorkerRole};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Fetch,
    #[default]
    Build,
    ProfileFixture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencySource {
    Official,
    AurBatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyInput {
    pub name: String,
    pub kind: String,
    pub source: DependencySource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDependency {
    pub name: String,
    pub version: String,
    pub source: DependencySource,
    pub package: ManifestEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_count: u16,
    pub memory_mib: u64,
    pub disk_mib: u64,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceEntryKind {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceManifestEntry {
    pub path: String,
    pub kind: SourceEntryKind,
    pub sha256: Option<String>,
    pub size: u64,
    pub link_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditSourceFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub selection_reason: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InlineInput {
    pub entry: ManifestEntry,
    pub content_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: Uuid,
    pub attempt: AttemptRef,
    pub required_role: WorkerRole,
    #[serde(default)]
    pub kind: JobKind,
    pub revision_sha256: String,
    pub source_manifest_sha256: Option<String>,
    pub dependency_snapshot_sha256: Option<String>,
    pub profile_sha256: Option<String>,
    #[serde(default)]
    pub source_attempt_id: Option<Uuid>,
    #[serde(default)]
    pub dependency_attempt_ids: Vec<Uuid>,
    #[serde(default)]
    pub dependencies: Vec<DependencyInput>,
    pub inputs: Vec<ManifestEntry>,
    #[serde(default)]
    pub inline_inputs: Vec<InlineInput>,
    pub limits: ResourceLimits,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildProfileSpec {
    pub profile_sha256: String,
    pub root_image: ManifestEntry,
    pub kernel: ManifestEntry,
    pub initramfs: ManifestEntry,
    pub installed_packages: Vec<String>,
    pub created_at: DateTime<Utc>,
}

impl BuildProfileSpec {
    pub fn content_sha256(&self) -> Result<String, serde_json::Error> {
        let content = serde_json::json!({
            "root_image": self.root_image,
            "kernel": self.kernel,
            "initramfs": self.initramfs,
            "installed_packages": self.installed_packages,
            "created_at": self.created_at,
        });
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&content)?)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuestJob {
    pub schema_version: u16,
    pub kind: JobKind,
    pub job_id: Uuid,
    pub attempt: AttemptRef,
    pub revision_sha256: String,
    pub source_manifest_sha256: Option<String>,
    pub dependency_snapshot_sha256: Option<String>,
    pub expected_outputs: Vec<String>,
    pub allow_check: bool,
}

impl JobSpec {
    pub fn is_expired_at(&self, now: DateTime<Utc>) -> bool {
        now > self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub package_name: Option<String>,
    pub package_version: Option<String>,
    pub architecture: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildResult {
    pub job_id: Uuid,
    pub attempt: AttemptRef,
    pub revision_sha256: String,
    pub source_manifest_sha256: String,
    pub dependency_snapshot_sha256: String,
    pub profile_sha256: String,
    pub artifacts: Vec<ArtifactRecord>,
    pub provenance: BTreeMap<String, String>,
    pub log_sha256: String,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchResult {
    pub job_id: Uuid,
    pub attempt: AttemptRef,
    pub revision_sha256: String,
    pub source_manifest_sha256: String,
    pub sources: Vec<SourceManifestEntry>,
    pub audit_files: Vec<AuditSourceFile>,
    pub resolved_dependencies: Vec<ResolvedDependency>,
    pub resolved_pkgver: Option<String>,
    pub dependency_snapshot_sha256: String,
    pub log_sha256: String,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub enum GuestResult {
    Fetch(FetchResult),
    Build(BuildResult),
    ProfileFixture(BuildResult),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferCapability {
    pub id: Uuid,
    pub source_worker: Uuid,
    pub destination_worker: Uuid,
    pub attempt: Option<AttemptRef>,
    pub writer_epoch: u64,
    pub files: Vec<ManifestEntry>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAuthorization {
    pub release_id: Uuid,
    pub batch_id: Uuid,
    pub writer_epoch: u64,
    pub repository_name: String,
    pub source_git_commit: String,
    pub revision_sha256s: Vec<String>,
    pub audit_report_sha256s: Vec<String>,
    pub artifacts: Vec<ArtifactRecord>,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub release_id: Uuid,
    pub batch_id: Uuid,
    pub source_git_commit: String,
    pub repository_name: String,
    pub writer_epoch: u64,
    pub artifacts: Vec<ArtifactRecord>,
    pub repository_database: ManifestEntry,
    pub repository_files: ManifestEntry,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveReceipt {
    pub release_id: Uuid,
    pub archive_worker: Uuid,
    pub release_manifest_sha256: String,
    pub files: Vec<ManifestEntry>,
    pub state: ArchiveState,
    pub verified_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn job_expiration_is_explicit() {
        let now = Utc::now();
        let job_id = Uuid::new_v4();
        let spec = JobSpec {
            job_id,
            attempt: AttemptRef {
                job_id,
                attempt_id: Uuid::new_v4(),
                generation: 0,
            },
            required_role: WorkerRole::Builder,
            kind: JobKind::Build,
            revision_sha256: "a".repeat(64),
            source_manifest_sha256: None,
            dependency_snapshot_sha256: None,
            profile_sha256: None,
            source_attempt_id: None,
            dependency_attempt_ids: Vec::new(),
            dependencies: Vec::new(),
            inputs: Vec::new(),
            inline_inputs: Vec::new(),
            limits: ResourceLimits {
                cpu_count: 1,
                memory_mib: 1024,
                disk_mib: 4096,
                timeout_seconds: 600,
            },
            issued_at: now,
            expires_at: now + Duration::minutes(5),
        };
        assert!(!spec.is_expired_at(now));
        assert!(spec.is_expired_at(now + Duration::minutes(6)));
    }
}
