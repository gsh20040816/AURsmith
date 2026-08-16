CREATE TABLE vcs_rewrite_reviews (
    package_base TEXT PRIMARY KEY NOT NULL,
    previous_commit TEXT NOT NULL,
    current_commit TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending', 'approved', 'rejected')),
    rationale TEXT,
    requested_at TEXT NOT NULL,
    decided_at TEXT,
    decided_by TEXT
);
