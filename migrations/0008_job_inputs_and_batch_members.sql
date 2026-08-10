ALTER TABLE jobs ADD COLUMN kind TEXT NOT NULL DEFAULT 'build';
ALTER TABLE jobs ADD COLUMN profile_sha256 TEXT;
ALTER TABLE jobs ADD COLUMN source_manifest_sha256 TEXT;
ALTER TABLE jobs ADD COLUMN dependency_snapshot_sha256 TEXT;
ALTER TABLE jobs ADD COLUMN inputs_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE jobs ADD COLUMN inline_inputs_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE workers ADD COLUMN profiles_json TEXT NOT NULL DEFAULT '[]';

CREATE TABLE release_batch_revisions (
    batch_id TEXT NOT NULL REFERENCES release_batches(id) ON DELETE CASCADE,
    revision_id TEXT NOT NULL REFERENCES revisions(id),
    build_order INTEGER NOT NULL,
    PRIMARY KEY(batch_id, revision_id),
    UNIQUE(batch_id, build_order)
);
