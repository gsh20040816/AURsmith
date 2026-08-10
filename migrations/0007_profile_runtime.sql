ALTER TABLE build_profiles ADD COLUMN envelope_json TEXT;
ALTER TABLE build_profiles ADD COLUMN last_verified_at TEXT;
ALTER TABLE build_profiles ADD COLUMN failure_reason TEXT;

CREATE TABLE dependency_observations (
    id TEXT PRIMARY KEY NOT NULL,
    job_id TEXT NOT NULL REFERENCES jobs(id),
    package_name TEXT NOT NULL,
    official_repository INTEGER NOT NULL CHECK(official_repository IN (0, 1)),
    download_bytes INTEGER NOT NULL CHECK(download_bytes >= 0),
    download_milliseconds INTEGER NOT NULL CHECK(download_milliseconds >= 0),
    install_milliseconds INTEGER NOT NULL CHECK(install_milliseconds >= 0),
    cache_hit INTEGER NOT NULL CHECK(cache_hit IN (0, 1)),
    observed_at TEXT NOT NULL
);
CREATE INDEX dependency_observations_package_time_idx
    ON dependency_observations(package_name, observed_at);
