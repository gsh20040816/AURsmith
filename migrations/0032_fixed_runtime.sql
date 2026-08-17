CREATE TABLE builder_runtime (
    id INTEGER PRIMARY KEY NOT NULL CHECK(id = 1),
    status_json TEXT NOT NULL DEFAULT '{}',
    last_seen_at TEXT,
    updated_at TEXT NOT NULL
);

INSERT INTO builder_runtime(id, status_json, last_seen_at, updated_at)
SELECT 1, COALESCE(status_json, '{}'), last_seen_at, updated_at
FROM workers
WHERE role = 'builder' AND connection_mode = 'reverse'
ORDER BY created_at
LIMIT 1;

INSERT OR IGNORE INTO builder_runtime(id, updated_at)
VALUES (1, CURRENT_TIMESTAMP);

CREATE TABLE builder_reports (
    job_id TEXT PRIMARY KEY NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    response_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT OR REPLACE INTO builder_reports(job_id, response_json, updated_at)
SELECT job_id, response_json, updated_at
FROM reverse_worker_reports
WHERE job_id IN (
    SELECT id FROM jobs WHERE status IN ('dispatched', 'running', 'uncertain')
);

CREATE TABLE uploads (
    id TEXT PRIMARY KEY NOT NULL,
    batch_id TEXT NOT NULL REFERENCES release_batches(id) ON DELETE CASCADE,
    source_job_id TEXT NOT NULL UNIQUE REFERENCES jobs(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('issued', 'export_ready', 'verified', 'expired', 'failed')),
    request_json TEXT NOT NULL,
    last_error TEXT,
    expires_at TEXT NOT NULL,
    export_cleaned_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO uploads(id, batch_id, source_job_id, state, request_json, last_error, expires_at, export_cleaned_at, created_at, updated_at)
SELECT id, batch_id, source_job_id, state, envelope_json, last_error, expires_at, export_cleaned_at, created_at, updated_at
FROM transfer_capabilities
WHERE state IN ('issued', 'export_ready', 'verified')
  AND batch_id IN (
      SELECT id FROM release_batches
      WHERE state IN ('ready_to_publish', 'artifacts_ready', 'publishing')
  );

CREATE INDEX uploads_state_idx ON uploads(state, updated_at);

CREATE TABLE release_jobs (
    release_id TEXT PRIMARY KEY NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('issued', 'signing', 'published', 'failed')),
    plan_json TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO release_jobs(release_id, state, plan_json, attempt_count, last_error, expires_at, created_at, updated_at)
SELECT release_id,
       CASE state WHEN 'awaiting_signer' THEN 'signing' ELSE state END,
       envelope_json,
       attempt_count,
       last_error,
       expires_at,
       created_at,
       updated_at
FROM release_authorizations
WHERE state IN ('issued', 'awaiting_signer');

CREATE INDEX release_jobs_state_idx ON release_jobs(state, updated_at);

-- The copied rows above are the only migration inputs retained by the fixed
-- Builder/Publisher runtime. These legacy subsystems are no longer queried.
DROP TABLE alert_notifications;
DROP TABLE alerts;
DROP TABLE control_plane_backup_archives;
DROP TABLE control_plane_backups;
DROP TABLE archive_transfers;
DROP TABLE archive_inventories;
DROP TABLE archive_copies;
DROP TABLE profile_dependency_evaluations;
DROP TABLE profile_evaluation_runs;
DROP TABLE dependency_observations;
DROP TABLE build_profiles;
DROP TABLE reverse_worker_reports;
DROP TABLE reverse_worker_nonces;
DROP TABLE transfer_capabilities;
DROP TABLE release_authorizations;
DROP TABLE audit_pre_scans;
DROP TABLE events;
ALTER TABLE releases DROP COLUMN writer_epoch;
