ALTER TABLE jobs DROP COLUMN preferred_worker_id;
ALTER TABLE jobs DROP COLUMN worker_id;
ALTER TABLE jobs DROP COLUMN required_role;
ALTER TABLE jobs DROP COLUMN required_labels_json;
ALTER TABLE jobs DROP COLUMN profile_sha256;
ALTER TABLE jobs DROP COLUMN source_attempt_id;
ALTER TABLE jobs DROP COLUMN upstream_pkgrel;
ALTER TABLE jobs DROP COLUMN published_pkgrel;
ALTER TABLE jobs DROP COLUMN signed_spec_json;
ALTER TABLE jobs DROP COLUMN limits_json;

DROP INDEX one_active_publisher;
DROP TABLE workers;
