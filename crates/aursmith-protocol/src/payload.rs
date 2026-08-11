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
    pub upstream_pkgrel: Option<String>,
    #[serde(default)]
    pub published_pkgrel: Option<String>,
    #[serde(default)]
    pub source_attempt_id: Option<Uuid>,
    #[serde(default)]
    pub dependency_attempt_ids: Vec<Uuid>,
    #[serde(default)]
    pub dependencies: Vec<DependencyInput>,
    pub inputs: Vec<ManifestEntry>,
    #[serde(default)]
    pub inline_inputs: Vec<InlineInput>,
    #[serde(default)]
    pub expected_outputs: Vec<String>,
    #[serde(default = "default_allow_check")]
    pub allow_check: bool,
    pub limits: ResourceLimits,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

fn default_allow_check() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildProfileSpec {
    pub profile_sha256: String,
    pub root_image: ManifestEntry,
    pub kernel: ManifestEntry,
    pub initramfs: ManifestEntry,
    pub installed_packages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_mirror: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl BuildProfileSpec {
    pub fn content_sha256(&self) -> Result<String, serde_json::Error> {
        let mut content = serde_json::json!({
            "root_image": self.root_image,
            "kernel": self.kernel,
            "initramfs": self.initramfs,
            "installed_packages": self.installed_packages,
            "created_at": self.created_at,
        });
        if let Some(repository_mirror) = &self.repository_mirror {
            content["repository_mirror"] = serde_json::json!(repository_mirror);
        }
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(&content)?)))
    }
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
    pub dependency_download_milliseconds: u64,
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
    #[serde(default)]
    pub release_id: Option<Uuid>,
    #[serde(default)]
    pub backup_id: Option<Uuid>,
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
    #[serde(default)]
    pub evidence_files: Vec<ManifestEntry>,
    #[serde(default)]
    pub removed_package_names: Vec<String>,
    #[serde(default)]
    pub include_repository_keyring: bool,
    #[serde(default)]
    pub evidence: ReleaseEvidence,
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
    #[serde(default)]
    pub evidence_files: Vec<ManifestEntry>,
    #[serde(default)]
    pub removed_package_names: Vec<String>,
    #[serde(default)]
    pub repository_keyring: Option<ArtifactRecord>,
    pub repository_database: ManifestEntry,
    pub repository_files: ManifestEntry,
    #[serde(default)]
    pub artifact_inspections: Option<ManifestEntry>,
    #[serde(default)]
    pub release_authorization: Option<ManifestEntry>,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReleaseEvidence {
    pub schema_version: u16,
    pub records: Vec<ReleaseEvidenceRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseEvidenceRecord {
    pub kind: String,
    pub identity: String,
    pub sha256: String,
    pub document: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRollbackAuthorization {
    pub release_id: Uuid,
    pub writer_epoch: u64,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlPlaneBackup {
    pub backup_id: Uuid,
    pub database: ManifestEntry,
    pub source_git_commit: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveInventory {
    pub archive_worker: Uuid,
    pub full_digest: bool,
    pub release_count: u64,
    #[serde(default)]
    pub backup_count: u64,
    pub file_count: u64,
    pub byte_count: u64,
    pub failures: Vec<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupArchiveReceipt {
    pub backup_id: Uuid,
    pub archive_worker: Uuid,
    pub files: Vec<ManifestEntry>,
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
            upstream_pkgrel: None,
            published_pkgrel: None,
            source_attempt_id: None,
            dependency_attempt_ids: Vec::new(),
            dependencies: Vec::new(),
            inputs: Vec::new(),
            inline_inputs: Vec::new(),
            expected_outputs: vec!["demo".into()],
            allow_check: true,
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
        let mut legacy = serde_json::to_value(&spec).unwrap();
        legacy.as_object_mut().unwrap().remove("expected_outputs");
        legacy.as_object_mut().unwrap().remove("allow_check");
        let decoded: JobSpec = serde_json::from_value(legacy).unwrap();
        assert!(decoded.allow_check, "旧 JobSpec 必须保持默认执行 check()");
        assert!(decoded.expected_outputs.is_empty());
    }

    #[test]
    fn profile_mirror_is_part_of_new_digest_without_breaking_legacy_payloads() {
        let created_at = Utc::now();
        let entry = |path: &str| ManifestEntry {
            path: path.into(),
            sha256: "a".repeat(64),
            size: 1,
        };
        let mut profile = BuildProfileSpec {
            profile_sha256: String::new(),
            root_image: entry("root.qcow2"),
            kernel: entry("vmlinuz-linux"),
            initramfs: entry("initramfs-linux.img"),
            installed_packages: vec!["base 3-3".into()],
            repository_mirror: None,
            created_at,
        };
        let legacy_digest = profile.content_sha256().unwrap();
        profile.repository_mirror = Some("https://geo.mirror.pkgbuild.com".into());
        assert_ne!(legacy_digest, profile.content_sha256().unwrap());
        let mut value = serde_json::to_value(&profile).unwrap();
        value.as_object_mut().unwrap().remove("repository_mirror");
        assert_eq!(
            serde_json::from_value::<BuildProfileSpec>(value)
                .unwrap()
                .repository_mirror,
            None
        );
    }
}
