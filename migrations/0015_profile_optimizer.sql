CREATE TABLE profile_evaluation_runs (
    id TEXT PRIMARY KEY NOT NULL,
    evaluated_at TEXT NOT NULL UNIQUE
);

CREATE TABLE profile_dependency_evaluations (
    package_name TEXT PRIMARY KEY NOT NULL,
    consecutive_hot_periods INTEGER NOT NULL DEFAULT 0,
    consecutive_low_periods INTEGER NOT NULL DEFAULT 0,
    action TEXT NOT NULL,
    stats_json TEXT NOT NULL,
    evaluated_at TEXT NOT NULL
);
