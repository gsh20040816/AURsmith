-- AURsmith 单库 schema（v2）。旧库通过 `aursmithd legacy-export` → `aursmithd import` 迁移。
-- 时间统一为 RFC 3339 UTC 文本。

CREATE TABLE admin (
    id INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
    username TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE sessions (
    token_sha256 TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

-- 只记录直接订阅；依赖闭包由已批准 revision 的依赖实时计算。
CREATE TABLE subscriptions (
    package_base TEXT PRIMARY KEY NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE provider_choices (
    package_base TEXT NOT NULL,
    dependency TEXT NOT NULL,
    provider_base TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (package_base, dependency)
);

-- AUR 元数据缓存 + 同步状态 + 构建策略。
CREATE TABLE packages (
    package_base TEXT PRIMARY KEY NOT NULL,
    version TEXT,
    description TEXT,
    maintainer TEXT,
    out_of_date INTEGER,
    last_modified INTEGER,
    outputs_json TEXT NOT NULL DEFAULT '[]',
    allow_check INTEGER NOT NULL DEFAULT 1 CHECK (allow_check IN (0, 1)),
    next_check_at TEXT NOT NULL,
    last_checked_at TEXT,
    sync_failures INTEGER NOT NULL DEFAULT 0 CHECK (sync_failures >= 0),
    sync_error TEXT
);

CREATE TABLE revisions (
    id TEXT PRIMARY KEY NOT NULL,
    package_base TEXT NOT NULL REFERENCES packages(package_base),
    aur_commit TEXT NOT NULL,
    vcs_commit TEXT,
    version TEXT NOT NULL,
    tree_sha256 TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    dependencies_json TEXT NOT NULL,
    baseline_revision_id TEXT REFERENCES revisions(id),
    state TEXT NOT NULL CHECK (state IN ('pending_review', 'manual_review', 'approved', 'rejected', 'superseded')),
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE UNIQUE INDEX revisions_identity ON revisions(package_base, aur_commit, COALESCE(vcs_commit, ''));
CREATE INDEX revisions_by_state ON revisions(state, package_base);

CREATE TABLE reviews (
    id TEXT PRIMARY KEY NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revisions(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('scan', 'reuse', 'agent', 'human')),
    role TEXT CHECK (role IN ('low', 'high')),
    model TEXT,
    verdict TEXT NOT NULL CHECK (verdict IN ('approve', 'reject', 'error')),
    summary TEXT NOT NULL,
    findings_json TEXT NOT NULL DEFAULT '[]',
    created_at TEXT NOT NULL
);
CREATE INDEX reviews_by_revision ON reviews(revision_id);

-- 一行一个构建；瞬态失败在同一行上递增 attempt 重试，租约过期替代旧的“不确定”状态。
CREATE TABLE builds (
    id TEXT PRIMARY KEY NOT NULL,
    revision_id TEXT NOT NULL REFERENCES revisions(id),
    package_base TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'uploading', 'succeeded', 'failed')),
    attempt INTEGER NOT NULL DEFAULT 1 CHECK (attempt >= 1),
    reason TEXT NOT NULL CHECK (reason IN ('approved', 'manual')),
    allow_check INTEGER NOT NULL CHECK (allow_check IN (0, 1)),
    builder_id TEXT,
    lease_expires_at TEXT,
    dep_build_ids_json TEXT NOT NULL DEFAULT '[]',
    artifacts_json TEXT NOT NULL DEFAULT '[]',
    error_code TEXT,
    error_class TEXT CHECK (error_class IN ('transient', 'deterministic', 'config')),
    log TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT
);
CREATE INDEX builds_by_state ON builds(state);
CREATE INDEX builds_by_package ON builds(package_base, created_at);

-- 期望状态发布：每个不同的期望软件包集合一行。
CREATE TABLE publications (
    id TEXT PRIMARY KEY NOT NULL,
    plan_sha256 TEXT NOT NULL,
    plan_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'published', 'failed')),
    error TEXT,
    manifest_sha256 TEXT,
    keyring_fingerprint TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX publications_by_created ON publications(created_at);
