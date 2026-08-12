ALTER TABLE workers ADD COLUMN connection_mode TEXT NOT NULL DEFAULT 'direct'
    CHECK(connection_mode IN ('direct', 'reverse'));

CREATE TABLE reverse_worker_nonces (
    worker_id TEXT NOT NULL REFERENCES workers(id),
    nonce TEXT NOT NULL,
    seen_at TEXT NOT NULL,
    PRIMARY KEY(worker_id, nonce)
);

CREATE TABLE reverse_worker_reports (
    worker_id TEXT NOT NULL REFERENCES workers(id),
    job_id TEXT NOT NULL REFERENCES jobs(id),
    response_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(worker_id, job_id)
);
