DROP INDEX revisions_identity_idx;
ALTER TABLE revisions ADD COLUMN rebuild_generation INTEGER NOT NULL DEFAULT 0;
ALTER TABLE jobs ADD COLUMN upstream_pkgrel TEXT;
ALTER TABLE jobs ADD COLUMN published_pkgrel TEXT;
CREATE UNIQUE INDEX revisions_identity_idx
    ON revisions(
        package_base,
        aur_commit,
        COALESCE(vcs_commit, ''),
        audit_policy_version,
        provider_selection_sha256,
        rebuild_generation
    );

CREATE TABLE artifact_official_dependencies (
    artifact_sha256 TEXT NOT NULL REFERENCES artifacts(sha256) ON DELETE CASCADE,
    package_name TEXT NOT NULL,
    package_version TEXT NOT NULL,
    package_sha256 TEXT NOT NULL,
    PRIMARY KEY(artifact_sha256, package_name)
);

CREATE TABLE rebuild_recommendations (
    package_base TEXT PRIMARY KEY NOT NULL REFERENCES package_bases(name),
    state TEXT NOT NULL CHECK(state IN ('suggested', 'disabled', 'scheduled', 'resolved')),
    reason TEXT NOT NULL,
    changes_json TEXT NOT NULL,
    detected_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
