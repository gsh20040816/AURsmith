CREATE TABLE control_plane_backups (
    id TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('creating', 'verified', 'failed')),
    database_sha256 TEXT,
    database_size INTEGER,
    directory TEXT NOT NULL,
    envelope_json TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    verified_at TEXT
);
