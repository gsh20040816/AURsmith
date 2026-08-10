CREATE TABLE package_bases (
    name TEXT PRIMARY KEY NOT NULL,
    version TEXT NOT NULL,
    description TEXT,
    maintainer TEXT,
    out_of_date_at INTEGER,
    orphaned INTEGER NOT NULL DEFAULT 0 CHECK(orphaned IN (0, 1)),
    vcs_kind TEXT,
    outputs_json TEXT NOT NULL,
    dependencies_json TEXT NOT NULL,
    optional_dependencies_json TEXT NOT NULL,
    provides_json TEXT NOT NULL,
    architectures_json TEXT NOT NULL,
    aur_last_modified INTEGER,
    last_synced_at TEXT NOT NULL
);

CREATE TABLE subscription_references (
    owner_package_base TEXT NOT NULL,
    dependency_package_base TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY(owner_package_base, dependency_package_base)
);

CREATE TABLE revision_dependencies (
    revision_id TEXT NOT NULL REFERENCES revisions(id) ON DELETE CASCADE,
    dependency_name TEXT NOT NULL,
    dependency_kind TEXT NOT NULL CHECK(dependency_kind IN ('runtime', 'build', 'check')),
    target_package_base TEXT,
    provider_state TEXT NOT NULL CHECK(provider_state IN ('official_or_unknown', 'resolved', 'needs_selection', 'cycle')),
    candidates_json TEXT NOT NULL,
    PRIMARY KEY(revision_id, dependency_name, dependency_kind)
);
CREATE INDEX revision_dependencies_target_idx
    ON revision_dependencies(target_package_base, revision_id);

CREATE INDEX subscriptions_package_base_idx
    ON subscriptions(package_base, state);
