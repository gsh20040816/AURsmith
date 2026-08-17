CREATE TABLE subscriptions_next (
    id TEXT PRIMARY KEY NOT NULL,
    package_base TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('direct', 'implicit')),
    reference_count INTEGER NOT NULL DEFAULT 0 CHECK(reference_count >= 0),
    followed_outputs_json TEXT NOT NULL,
    selected_providers_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(package_base, kind)
);

WITH RECURSIVE reachable(package_base) AS (
    SELECT package_base
    FROM subscriptions
    WHERE kind = 'direct' AND state IN ('active', 'paused')
    UNION
    SELECT references_table.dependency_package_base
    FROM subscription_references AS references_table
    JOIN reachable ON reachable.package_base = references_table.owner_package_base
)
INSERT INTO subscriptions_next(
    id,
    package_base,
    kind,
    reference_count,
    followed_outputs_json,
    selected_providers_json,
    created_at,
    updated_at
)
SELECT
    id,
    package_base,
    kind,
    reference_count,
    followed_outputs_json,
    selected_providers_json,
    created_at,
    updated_at
FROM subscriptions
WHERE (kind = 'direct' AND state IN ('active', 'paused'))
   OR (kind = 'implicit' AND package_base IN (SELECT package_base FROM reachable));

DROP INDEX subscriptions_package_base_idx;
DROP TABLE subscriptions;
ALTER TABLE subscriptions_next RENAME TO subscriptions;

DELETE FROM subscription_references
WHERE owner_package_base NOT IN (SELECT package_base FROM subscriptions)
   OR dependency_package_base NOT IN (SELECT package_base FROM subscriptions);

UPDATE subscriptions
SET reference_count = (
    SELECT COUNT(*)
    FROM subscription_references
    WHERE dependency_package_base = subscriptions.package_base
),
updated_at = datetime('now')
WHERE kind = 'implicit';

CREATE INDEX subscriptions_package_base_idx
    ON subscriptions(package_base);
