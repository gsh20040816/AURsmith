CREATE TABLE archive_inventories (
    id TEXT PRIMARY KEY NOT NULL,
    archiver_worker_id TEXT NOT NULL REFERENCES workers(id),
    full_digest INTEGER NOT NULL,
    release_count INTEGER NOT NULL,
    file_count INTEGER NOT NULL,
    byte_count INTEGER NOT NULL,
    failure_count INTEGER NOT NULL,
    envelope_json TEXT NOT NULL,
    checked_at TEXT NOT NULL
);

CREATE INDEX archive_inventories_checked_idx
    ON archive_inventories(checked_at DESC);
