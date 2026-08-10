ALTER TABLE jobs ADD COLUMN revision_sha256 TEXT;
ALTER TABLE jobs ADD COLUMN required_labels_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE jobs ADD COLUMN limits_json TEXT;
ALTER TABLE jobs ADD COLUMN signed_spec_json TEXT;

CREATE INDEX jobs_schedulable_idx ON jobs(status, priority DESC, created_at);
