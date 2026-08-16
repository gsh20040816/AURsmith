ALTER TABLE workers ADD COLUMN identity_signing_key_hex TEXT;
ALTER TABLE transfer_capabilities ADD COLUMN export_cleaned_at TEXT;

CREATE TABLE archive_transfers (
    id TEXT PRIMARY KEY NOT NULL,
    archive_copy_id TEXT NOT NULL UNIQUE REFERENCES archive_copies(id) ON DELETE CASCADE,
    publisher_worker_id TEXT NOT NULL REFERENCES workers(id),
    archiver_worker_id TEXT NOT NULL REFERENCES workers(id),
    state TEXT NOT NULL CHECK(state IN ('issued', 'export_ready', 'verified', 'expired', 'failed')),
    envelope_json TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    export_cleaned_at TEXT
);

CREATE INDEX archive_transfers_state_idx
    ON archive_transfers(state, updated_at);
