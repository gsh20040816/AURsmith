CREATE TABLE job_evidence_files (
    job_id TEXT NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size > 0),
    created_at TEXT NOT NULL,
    PRIMARY KEY (job_id, path)
);

CREATE INDEX job_evidence_files_job_idx
    ON job_evidence_files(job_id, created_at);
