use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionKind {
    Direct,
    Implicit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionState {
    Active,
    Paused,
    RetainedWithoutReferences,
    Purged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevisionState {
    Discovered,
    Fetching,
    AuditPending,
    AuditApproved,
    AuditRejected,
    BuildPending,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    NoEligibleWorker,
    Dispatched,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Uncertain,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseState {
    Candidate,
    Staging,
    Committed,
    Superseded,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptRef {
    pub job_id: Uuid,
    pub attempt_id: Uuid,
    pub generation: u32,
}

impl AttemptRef {
    pub fn accepts_result_from(&self, candidate: &Self) -> bool {
        self.job_id == candidate.job_id
            && self.attempt_id == candidate.attempt_id
            && self.generation == candidate.generation
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageBaseRevision {
    pub id: Uuid,
    pub package_base: String,
    pub aur_commit: String,
    pub vcs_commit: Option<String>,
    pub upstream_version: String,
    pub published_version: Option<String>,
    pub split_outputs: BTreeSet<String>,
    pub selected_providers: BTreeMap<String, String>,
    pub input_sha256: String,
    pub audit_policy_version: String,
    pub state: RevisionState,
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempts_reject_stale_generations_and_ids() {
        let current = AttemptRef {
            job_id: Uuid::new_v4(),
            attempt_id: Uuid::new_v4(),
            generation: 2,
        };
        assert!(current.accepts_result_from(&current));
        let mut stale = current.clone();
        stale.generation = 1;
        assert!(!current.accepts_result_from(&stale));
        stale = current.clone();
        stale.attempt_id = Uuid::new_v4();
        assert!(!current.accepts_result_from(&stale));
    }

    #[test]
    fn release_state_is_explicit() {
        let release = ReleaseState::Committed;
        assert_eq!(release, ReleaseState::Committed);
    }
}
