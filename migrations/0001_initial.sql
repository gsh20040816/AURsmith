PRAGMA foreign_keys = ON;

CREATE TABLE system_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE administrators (
    id TEXT PRIMARY KEY NOT NULL,
    username TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE sessions (
    token_sha256 TEXT PRIMARY KEY NOT NULL,
    administrator_id TEXT NOT NULL REFERENCES administrators(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE TABLE events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    actor TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX events_aggregate_idx ON events(aggregate_type, aggregate_id, sequence);

CREATE TABLE workers (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    role TEXT NOT NULL CHECK(role IN ('builder', 'publisher', 'archiver')),
    state TEXT NOT NULL CHECK(state IN ('online', 'draining', 'offline', 'degraded', 'incompatible')),
    endpoint TEXT NOT NULL,
    ssh_host_key_sha256 TEXT NOT NULL,
    protocol_version INTEGER NOT NULL,
    labels_json TEXT NOT NULL,
    writer_epoch INTEGER NOT NULL DEFAULT 0,
    last_seen_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX one_active_publisher ON workers(role) WHERE role = 'publisher' AND state = 'online';

CREATE TABLE subscriptions (
    id TEXT PRIMARY KEY NOT NULL,
    package_base TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('direct', 'implicit')),
    state TEXT NOT NULL CHECK(state IN ('active', 'paused', 'retained_without_references', 'purged')),
    reference_count INTEGER NOT NULL DEFAULT 0 CHECK(reference_count >= 0),
    followed_outputs_json TEXT NOT NULL,
    selected_providers_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(package_base, kind)
);

CREATE TABLE revisions (
    id TEXT PRIMARY KEY NOT NULL,
    package_base TEXT NOT NULL,
    aur_commit TEXT NOT NULL,
    vcs_commit TEXT,
    upstream_version TEXT NOT NULL,
    published_version TEXT,
    input_sha256 TEXT NOT NULL,
    source_manifest_sha256 TEXT,
    audit_policy_version TEXT NOT NULL,
    state TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX revisions_identity_idx
    ON revisions(package_base, aur_commit, COALESCE(vcs_commit, ''), audit_policy_version);

CREATE TABLE release_batches (
    id TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL,
    graph_json TEXT NOT NULL,
    current_release_id TEXT,
    failure_reason TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE jobs (
    id TEXT PRIMARY KEY NOT NULL,
    batch_id TEXT REFERENCES release_batches(id),
    revision_id TEXT REFERENCES revisions(id),
    required_role TEXT NOT NULL,
    worker_id TEXT REFERENCES workers(id),
    status TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 0,
    failure_code TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE attempts (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL,
    token_sha256 TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    result_sha256 TEXT,
    UNIQUE(job_id, generation)
);

CREATE TABLE artifacts (
    sha256 TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES jobs(id),
    path TEXT NOT NULL,
    size INTEGER NOT NULL CHECK(size >= 0),
    package_name TEXT,
    package_version TEXT,
    architecture TEXT,
    provenance_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE releases (
    id TEXT PRIMARY KEY NOT NULL,
    batch_id TEXT NOT NULL REFERENCES release_batches(id),
    state TEXT NOT NULL,
    manifest_sha256 TEXT NOT NULL UNIQUE,
    source_git_commit TEXT NOT NULL,
    writer_epoch INTEGER NOT NULL,
    committed_at TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE archive_copies (
    id TEXT PRIMARY KEY NOT NULL,
    release_id TEXT NOT NULL REFERENCES releases(id),
    archiver_worker_id TEXT REFERENCES workers(id),
    state TEXT NOT NULL,
    receipt_sha256 TEXT,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE build_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    architecture TEXT NOT NULL,
    runner TEXT NOT NULL,
    manifest_sha256 TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    package_manifest_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    activated_at TEXT
);

CREATE TABLE alerts (
    id TEXT PRIMARY KEY NOT NULL,
    fingerprint TEXT NOT NULL UNIQUE,
    severity TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('open', 'acknowledged', 'resolved')),
    title TEXT NOT NULL,
    details_json TEXT NOT NULL,
    opened_at TEXT NOT NULL,
    acknowledged_at TEXT,
    resolved_at TEXT
);
