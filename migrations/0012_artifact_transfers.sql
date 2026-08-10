CREATE TABLE transfer_capabilities (
    id TEXT PRIMARY KEY NOT NULL,
    batch_id TEXT NOT NULL REFERENCES release_batches(id) ON DELETE CASCADE,
    source_job_id TEXT NOT NULL REFERENCES jobs(id),
    source_worker_id TEXT NOT NULL REFERENCES workers(id),
    destination_worker_id TEXT NOT NULL REFERENCES workers(id),
    state TEXT NOT NULL CHECK(state IN ('issued', 'export_ready', 'verified', 'expired', 'failed')),
    envelope_json TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX transfer_capabilities_state_idx
    ON transfer_capabilities(state, updated_at);
