ALTER TABLE jobs ADD COLUMN expected_outputs_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE jobs ADD COLUMN allow_check INTEGER NOT NULL DEFAULT 1 CHECK(allow_check IN (0, 1));

CREATE TABLE package_build_policies (
    package_base TEXT PRIMARY KEY NOT NULL REFERENCES package_bases(name) ON DELETE CASCADE,
    allow_check INTEGER NOT NULL DEFAULT 1 CHECK(allow_check IN (0, 1)),
    updated_at TEXT NOT NULL
);
