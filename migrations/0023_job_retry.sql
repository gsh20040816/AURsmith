ALTER TABLE jobs ADD COLUMN next_attempt_at TEXT;
CREATE INDEX jobs_retry_idx ON jobs(status, next_attempt_at, priority DESC, created_at);
