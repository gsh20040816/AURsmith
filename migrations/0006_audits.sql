CREATE TABLE audit_bundles (
    sha256 TEXT PRIMARY KEY NOT NULL,
    revision_id TEXT NOT NULL UNIQUE REFERENCES revisions(id) ON DELETE CASCADE,
    policy_version TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    coverage_json TEXT NOT NULL,
    deterministic_findings_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('blocked', 'agent_pending', 'agent_running', 'manual_review', 'approved', 'rejected')),
    created_at TEXT NOT NULL
);

CREATE TABLE agent_runs (
    id TEXT PRIMARY KEY NOT NULL,
    audit_bundle_sha256 TEXT NOT NULL REFERENCES audit_bundles(sha256),
    tier TEXT NOT NULL CHECK(tier IN ('low', 'high')),
    slot INTEGER NOT NULL,
    attempt INTEGER NOT NULL CHECK(attempt IN (0, 1)),
    adapter TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT 'unconfigured',
    model TEXT NOT NULL,
    adapter_version TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'succeeded', 'failed')),
    verdict TEXT CHECK(verdict IN ('approve', 'reject', 'error')),
    report_json TEXT,
    raw_output_json TEXT,
    report_sha256 TEXT,
    cost_microusd INTEGER,
    started_at TEXT,
    finished_at TEXT,
    UNIQUE(audit_bundle_sha256, tier, slot, attempt)
);

CREATE TABLE audit_decisions (
    id TEXT PRIMARY KEY NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revisions(id),
    audit_bundle_sha256 TEXT NOT NULL REFERENCES audit_bundles(sha256),
    policy_version TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('approved_by_low_cost', 'approved_by_high_cost', 'manual_review', 'blocked_deterministically', 'manually_approved', 'manually_rejected')),
    decided_by TEXT NOT NULL,
    rationale TEXT,
    report_sha256 TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX audit_decisions_revision_idx ON audit_decisions(revision_id, created_at);

CREATE TABLE manual_actions (
    id TEXT PRIMARY KEY NOT NULL,
    action_type TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    requested_by TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending', 'completed', 'rejected')),
    details_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE INDEX agent_runs_pending_idx ON agent_runs(status, tier, started_at);
