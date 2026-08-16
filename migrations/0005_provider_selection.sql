DROP INDEX revisions_identity_idx;
ALTER TABLE revisions ADD COLUMN provider_selection_sha256 TEXT NOT NULL DEFAULT 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855';
CREATE UNIQUE INDEX revisions_identity_idx
    ON revisions(
        package_base,
        aur_commit,
        COALESCE(vcs_commit, ''),
        audit_policy_version,
        provider_selection_sha256
    );
