ALTER TABLE archive_inventories ADD COLUMN backup_count INTEGER NOT NULL DEFAULT 0;

CREATE TABLE control_plane_backup_archives (
    id TEXT PRIMARY KEY NOT NULL,
    backup_id TEXT NOT NULL UNIQUE REFERENCES control_plane_backups(id),
    archiver_worker_id TEXT NOT NULL REFERENCES workers(id),
    state TEXT NOT NULL CHECK(state IN ('issued', 'verified', 'failed')),
    envelope_json TEXT NOT NULL,
    export_directory TEXT NOT NULL,
    receipt_sha256 TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
