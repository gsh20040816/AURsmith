CREATE TABLE package_sync_state (
    package_base TEXT PRIMARY KEY NOT NULL,
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK(consecutive_failures >= 0),
    last_checked_at TEXT,
    last_success_at TEXT,
    last_official_checked_at TEXT,
    last_error TEXT,
    next_check_at TEXT
);

CREATE INDEX package_sync_due_idx ON package_sync_state(next_check_at);
