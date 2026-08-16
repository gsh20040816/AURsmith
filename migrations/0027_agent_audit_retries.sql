CREATE TABLE agent_runs_next (
    id TEXT PRIMARY KEY NOT NULL,
    audit_bundle_sha256 TEXT NOT NULL REFERENCES audit_bundles(sha256),
    tier TEXT NOT NULL CHECK(tier IN ('low', 'high')),
    slot INTEGER NOT NULL,
    attempt INTEGER NOT NULL CHECK(attempt >= 0),
    adapter TEXT NOT NULL,
    provider TEXT NOT NULL DEFAULT 'unconfigured',
    model TEXT NOT NULL,
    adapter_version TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('pending', 'running', 'succeeded', 'failed')),
    verdict TEXT CHECK(verdict IN ('approve', 'reject', 'error')),
    report_json TEXT,
    raw_output_json TEXT,
    report_sha256 TEXT,
    cost_microusd INTEGER,
    started_at TEXT,
    finished_at TEXT,
    UNIQUE(audit_bundle_sha256, tier, slot, attempt)
);

INSERT INTO agent_runs_next(
    id, audit_bundle_sha256, tier, slot, attempt, adapter, provider, model,
    adapter_version, prompt_version, status, verdict, report_json,
    raw_output_json, report_sha256, cost_microusd, started_at, finished_at
)
SELECT
    id, audit_bundle_sha256, tier, slot, attempt, adapter, provider, model,
    adapter_version, prompt_version, status, verdict, report_json,
    raw_output_json, report_sha256, cost_microusd, started_at, finished_at
FROM agent_runs;

DROP TABLE agent_runs;
ALTER TABLE agent_runs_next RENAME TO agent_runs;

CREATE INDEX agent_runs_pending_idx ON agent_runs(status, tier, started_at);
