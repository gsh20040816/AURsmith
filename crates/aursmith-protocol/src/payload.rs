use aursmith_domain::{ArchiveState, AttemptRef, WorkerRole};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;

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
pub struct JobSpec {
    pub job_id: Uuid,
    pub attempt: AttemptRef,
    pub required_role: WorkerRole,
    pub revision_sha256: String,
    pub source_manifest_sha256: Option<String>,
    pub dependency_snapshot_sha256: Option<String>,
    pub profile_sha256: Option<String>,
    pub inputs: Vec<ManifestEntry>,
    pub limits: ResourceLimits,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
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
            revision_sha256: "a".repeat(64),
            source_manifest_sha256: None,
            dependency_snapshot_sha256: None,
            profile_sha256: None,
            inputs: Vec::new(),
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
