ALTER TABLE jobs ADD COLUMN preferred_worker_id TEXT REFERENCES workers(id);
ALTER TABLE jobs ADD COLUMN source_attempt_id TEXT;
