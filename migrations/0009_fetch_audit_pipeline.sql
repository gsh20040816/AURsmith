CREATE TABLE audit_pre_scans (
    revision_id TEXT PRIMARY KEY NOT NULL REFERENCES revisions(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL UNIQUE,
    payload_json TEXT NOT NULL,
    coverage_json TEXT NOT NULL,
    deterministic_findings_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('ready_for_fetch', 'blocked', 'consumed')),
    created_at TEXT NOT NULL,
    consumed_at TEXT
);

CREATE INDEX jobs_batch_revision_kind_idx
    ON jobs(batch_id, revision_id, kind, status);
