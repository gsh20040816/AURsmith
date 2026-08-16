PRAGMA application_id = 0x41555253;
PRAGMA user_version = 2;

CREATE TABLE administrators (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    username TEXT NOT NULL UNIQUE CHECK (length(username) BETWEEN 3 AND 64),
    password_hash TEXT NOT NULL,
    created_at TEXT NOT NULL
) STRICT;

CREATE TABLE sessions (
    token_sha256 TEXT PRIMARY KEY
        CHECK (length(token_sha256) = 64 AND token_sha256 NOT GLOB '*[^0-9a-f]*'),
    administrator_id INTEGER NOT NULL DEFAULT 1
        CHECK (administrator_id = 1)
        REFERENCES administrators(id) ON DELETE CASCADE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
) STRICT;

CREATE TABLE tracked_packages (
    pkgbase TEXT PRIMARY KEY
        CHECK (
            length(pkgbase) BETWEEN 1 AND 128
            AND pkgbase = lower(pkgbase)
            AND pkgbase NOT GLOB '*[^a-z0-9@._+-]*'
            AND substr(pkgbase, 1, 1) NOT IN ('.', '-')
        ),
    state TEXT NOT NULL CHECK (state IN ('active', 'paused')),
    approved_aur_commit TEXT,
    approved_tree_sha256 TEXT,
    approved_at TEXT,
    last_checked_at TEXT,
    last_error TEXT CHECK (last_error IS NULL OR length(last_error) BETWEEN 1 AND 16384),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (approved_aur_commit IS NULL AND approved_tree_sha256 IS NULL AND approved_at IS NULL)
        OR
        (
            approved_aur_commit IS NOT NULL
            AND approved_tree_sha256 IS NOT NULL
            AND approved_at IS NOT NULL
            AND length(approved_aur_commit) = 40
            AND approved_aur_commit NOT GLOB '*[^0-9a-f]*'
            AND length(approved_tree_sha256) = 64
            AND approved_tree_sha256 NOT GLOB '*[^0-9a-f]*'
            AND length(approved_at) > 0
        )
    )
) STRICT;

CREATE TABLE aur_reviews (
    pkgbase TEXT NOT NULL
        REFERENCES tracked_packages(pkgbase) ON DELETE CASCADE,
    aur_commit TEXT NOT NULL
        CHECK (length(aur_commit) = 40 AND aur_commit NOT GLOB '*[^0-9a-f]*'),
    tree_sha256 TEXT
        CHECK (
            tree_sha256 IS NULL
            OR (length(tree_sha256) = 64 AND tree_sha256 NOT GLOB '*[^0-9a-f]*')
        ),
    comparison_kind TEXT NOT NULL CHECK (comparison_kind IN ('full', 'diff')),
    baseline_aur_commit TEXT,
    baseline_tree_sha256 TEXT,
    full_reason TEXT,
    status TEXT NOT NULL CHECK (status IN ('prepared', 'input_blocked', 'superseded')),
    blocker TEXT CHECK (blocker IS NULL OR length(blocker) BETWEEN 1 AND 16384),
    review_json_sha256 TEXT NOT NULL
        CHECK (length(review_json_sha256) = 64 AND review_json_sha256 NOT GLOB '*[^0-9a-f]*'),
    changes_diff_sha256 TEXT
        CHECK (
            changes_diff_sha256 IS NULL
            OR (
                length(changes_diff_sha256) = 64
                AND changes_diff_sha256 NOT GLOB '*[^0-9a-f]*'
            )
        ),
    findings_json_sha256 TEXT NOT NULL
        CHECK (
            length(findings_json_sha256) = 64
            AND findings_json_sha256 NOT GLOB '*[^0-9a-f]*'
        ),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (pkgbase, aur_commit),
    CHECK (
        (baseline_aur_commit IS NULL AND baseline_tree_sha256 IS NULL)
        OR
        (
            baseline_aur_commit IS NOT NULL
            AND baseline_tree_sha256 IS NOT NULL
            AND length(baseline_aur_commit) = 40
            AND baseline_aur_commit NOT GLOB '*[^0-9a-f]*'
            AND length(baseline_tree_sha256) = 64
            AND baseline_tree_sha256 NOT GLOB '*[^0-9a-f]*'
        )
    ),
    CHECK (
        (comparison_kind = 'full' AND full_reason IS NOT NULL AND changes_diff_sha256 IS NULL)
        OR
        (
            comparison_kind = 'diff'
            AND tree_sha256 IS NOT NULL
            AND baseline_aur_commit IS NOT NULL
            AND full_reason IS NULL
            AND changes_diff_sha256 IS NOT NULL
        )
    ),
    CHECK (
        (status = 'prepared' AND tree_sha256 IS NOT NULL AND blocker IS NULL)
        OR (status = 'input_blocked' AND blocker IS NOT NULL)
        OR
        (
            status = 'superseded'
            AND
            (
                (tree_sha256 IS NOT NULL AND blocker IS NULL)
                OR blocker IS NOT NULL
            )
        )
    )
) STRICT;

CREATE UNIQUE INDEX aur_reviews_one_current_per_package
    ON aur_reviews(pkgbase)
    WHERE status IN ('prepared', 'input_blocked');
