use aursmith_domain::AttemptRef;
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
pub struct ReverseAttemptReport {
    pub job_id: Uuid,
    pub response: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderPoll {
    pub status: serde_json::Value,
    #[serde(default)]
    pub attempts: Vec<ReverseAttemptReport>,
    #[serde(default, alias = "completed_transfers")]
    pub completed_uploads: Vec<Uuid>,
    pub sent_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderUpload {
    #[serde(alias = "id")]
    pub upload_id: Uuid,
    pub attempt: AttemptRef,
    pub files: Vec<ManifestEntry>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuilderLease {
    #[serde(default)]
    pub acknowledged_attempts: Vec<Uuid>,
    #[serde(default)]
    pub releasable_attempts: Vec<Uuid>,
    #[serde(default)]
    pub job: Option<JobSpec>,
    #[serde(default, alias = "transfer")]
    pub upload: Option<BuilderUpload>,
    pub issued_at: DateTime<Utc>,
    pub next_poll_seconds: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePlan {
    pub release_id: Uuid,
    pub batch_id: Uuid,
    pub repository_name: String,
    pub source_git_commit: String,
    pub artifacts: Vec<ArtifactRecord>,
    #[serde(default)]
    pub removed_package_names: Vec<String>,
    #[serde(default)]
    pub include_repository_keyring: bool,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseManifest {
    pub release_id: Uuid,
    pub batch_id: Uuid,
    pub source_git_commit: String,
    pub repository_name: String,
    pub artifacts: Vec<ArtifactRecord>,
    #[serde(default)]
    pub removed_package_names: Vec<String>,
    #[serde(default)]
    pub repository_keyring: Option<ArtifactRecord>,
    #[serde(default)]
    pub keyring_generation: Option<u64>,
    #[serde(default)]
    pub keyring_fingerprint: Option<String>,
    #[serde(default)]
    pub keyring_published_at: Option<DateTime<Utc>>,
    pub repository_database: ManifestEntry,
    pub repository_files: ManifestEntry,
    pub committed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRollbackRequest {
    pub release_id: Uuid,
    pub issued_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
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
    fn builder_poll_and_lease_are_plain_bearer_protocol_messages() {
        let poll = BuilderPoll {
            status: serde_json::json!({"role": "builder"}),
            attempts: Vec::new(),
            completed_uploads: Vec::new(),
            sent_at: Utc::now(),
        };
        let lease = BuilderLease {
            acknowledged_attempts: Vec::new(),
            releasable_attempts: Vec::new(),
            job: None,
            upload: None,
            issued_at: Utc::now(),
            next_poll_seconds: 15,
        };
        let encoded = serde_json::to_vec(&lease).unwrap();
        let decoded: BuilderLease = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, lease);
        assert_eq!(
            serde_json::to_value(poll).unwrap()["status"]["role"],
            "builder"
        );
    }

    #[test]
    fn builder_messages_accept_legacy_transfer_field_names() {
        let upload_id = Uuid::new_v4();
        let attempt_id = Uuid::new_v4();
        let job_id = Uuid::new_v4();
        let now = Utc::now();
        let poll: BuilderPoll = serde_json::from_value(serde_json::json!({
            "status": {"role": "builder"},
            "attempts": [],
            "completed_transfers": [upload_id],
            "sent_at": now,
        }))
        .unwrap();
        assert_eq!(poll.completed_uploads, vec![upload_id]);

        let lease: BuilderLease = serde_json::from_value(serde_json::json!({
            "acknowledged_attempts": [],
            "releasable_attempts": [],
            "job": null,
            "transfer": {
                "id": upload_id,
                "attempt": {
                    "job_id": job_id,
                    "attempt_id": attempt_id,
                    "generation": 0
                },
                "files": [],
                "expires_at": now,
            },
            "issued_at": now,
            "next_poll_seconds": 15,
        }))
        .unwrap();
        assert_eq!(lease.upload.unwrap().upload_id, upload_id);
    }
}
