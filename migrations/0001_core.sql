PRAGMA application_id = 0x41555253;
PRAGMA user_version = 1;

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
