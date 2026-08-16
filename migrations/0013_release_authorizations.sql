CREATE TABLE release_authorizations (
    release_id TEXT PRIMARY KEY NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    publisher_worker_id TEXT NOT NULL REFERENCES workers(id),
    state TEXT NOT NULL CHECK(state IN ('issued', 'awaiting_signer', 'published', 'failed')),
    envelope_json TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    expires_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX release_authorizations_state_idx
    ON release_authorizations(state, updated_at);

CREATE TABLE release_artifacts (
    release_id TEXT NOT NULL REFERENCES releases(id) ON DELETE CASCADE,
    artifact_sha256 TEXT NOT NULL REFERENCES artifacts(sha256),
    PRIMARY KEY(release_id, artifact_sha256)
);
