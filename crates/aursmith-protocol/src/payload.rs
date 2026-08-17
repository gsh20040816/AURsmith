use crate::SignedEnvelope;
use aursmith_domain::{ArchiveState, AttemptRef, WorkerRole};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    #[default]
    Build,
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
    pub artifacts: Vec<ArtifactRecord>,
    pub provenance: BTreeMap<String, String>,
    pub log_sha256: String,
    pub finished_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "result", rename_all = "snake_case")]
pub enum GuestResult {
    Build(BuildResult),
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
pub struct ReverseAttemptReport {
    pub job_id: Uuid,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseWorkerPoll {
    pub worker_id: Uuid,
    pub nonce: Uuid,
    pub status: serde_json::Value,
    #[serde(default)]
    pub attempts: Vec<ReverseAttemptReport>,
    #[serde(default)]
    pub completed_transfers: Vec<Uuid>,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReverseWorkerLease {
    pub worker_id: Uuid,
    #[serde(default)]
    pub acknowledged_attempts: Vec<Uuid>,
    #[serde(default)]
    pub releasable_attempts: Vec<Uuid>,
    #[serde(default)]
    pub job: Option<SignedEnvelope>,
    #[serde(default)]
    pub transfer: Option<SignedEnvelope>,
    pub issued_at: DateTime<Utc>,
    pub next_poll_seconds: u16,
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
    fn reverse_worker_poll_and_lease_keep_signed_identity_and_job() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[23; 32]);
        let worker_id = Uuid::new_v4();
        let poll = ReverseWorkerPoll {
            worker_id,
            nonce: Uuid::new_v4(),
            status: serde_json::json!({"role": "builder"}),
            attempts: Vec::new(),
            completed_transfers: Vec::new(),
            sent_at: Utc::now(),
        };
        let envelope = SignedEnvelope::sign("aursmith.reverse_worker_poll", &poll, &key).unwrap();
        assert_eq!(
            envelope
                .verify::<ReverseWorkerPoll>("aursmith.reverse_worker_poll")
                .unwrap(),
            poll
        );
        let lease = ReverseWorkerLease {
            worker_id,
            acknowledged_attempts: Vec::new(),
            releasable_attempts: Vec::new(),
            job: Some(envelope.clone()),
            transfer: None,
            issued_at: Utc::now(),
            next_poll_seconds: 15,
        };
        let encoded = serde_json::to_vec(&lease).unwrap();
        let decoded: ReverseWorkerLease = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded.worker_id, worker_id);
        assert_eq!(decoded.job.unwrap().payload_sha256, envelope.payload_sha256);
    }
}
