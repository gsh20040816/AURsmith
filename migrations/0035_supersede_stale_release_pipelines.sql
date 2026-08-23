UPDATE agent_runs
SET status = 'failed',
    verdict = 'error',
    raw_output_json = '{"error":"SUPERSEDED_REVISION"}',
    finished_at = CURRENT_TIMESTAMP
WHERE status IN ('pending', 'running')
  AND audit_bundle_sha256 IN (
      SELECT audit_bundles.sha256
      FROM audit_bundles
      JOIN revisions AS current ON current.id = audit_bundles.revision_id
      WHERE EXISTS (
          SELECT 1
          FROM revisions AS newer
          WHERE newer.package_base = current.package_base
            AND (
                newer.created_at > current.created_at
                OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
            )
      )
  );

UPDATE audit_bundles
SET state = 'rejected'
WHERE state IN ('agent_pending', 'agent_running', 'manual_review')
  AND revision_id IN (
      SELECT current.id
      FROM revisions AS current
      WHERE EXISTS (
          SELECT 1
          FROM revisions AS newer
          WHERE newer.package_base = current.package_base
            AND (
                newer.created_at > current.created_at
                OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
            )
      )
  );

UPDATE manual_actions
SET state = 'rejected',
    completed_at = COALESCE(completed_at, CURRENT_TIMESTAMP)
WHERE state = 'pending'
  AND aggregate_type = 'revision'
  AND aggregate_id IN (
      SELECT current.id
      FROM revisions AS current
      WHERE EXISTS (
          SELECT 1
          FROM revisions AS newer
          WHERE newer.package_base = current.package_base
            AND (
                newer.created_at > current.created_at
                OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
            )
      )
  );

UPDATE attempts
SET status = 'cancelled',
    finished_at = COALESCE(finished_at, CURRENT_TIMESTAMP)
WHERE status NOT IN ('succeeded', 'failed', 'cancelled')
  AND job_id IN (
      SELECT jobs.id
      FROM jobs
      JOIN release_batches ON release_batches.id = jobs.batch_id
      JOIN release_batch_revisions ON release_batch_revisions.batch_id = release_batches.id
      JOIN revisions AS current ON current.id = release_batch_revisions.revision_id
      WHERE release_batches.state IN ('awaiting_audit', 'building', 'build_failed', 'ready_to_publish', 'artifacts_ready')
        AND EXISTS (
            SELECT 1
            FROM revisions AS newer
            WHERE newer.package_base = current.package_base
              AND (
                  newer.created_at > current.created_at
                  OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
              )
        )
  );

UPDATE jobs
SET status = 'cancelled',
    failure_code = 'STALE_RELEASE_BATCH',
    updated_at = CURRENT_TIMESTAMP
WHERE status IN ('queued', 'no_eligible_worker', 'dispatched', 'running', 'uncertain')
  AND batch_id IN (
      SELECT release_batches.id
      FROM release_batches
      JOIN release_batch_revisions ON release_batch_revisions.batch_id = release_batches.id
      JOIN revisions AS current ON current.id = release_batch_revisions.revision_id
      WHERE release_batches.state IN ('awaiting_audit', 'building', 'build_failed', 'ready_to_publish', 'artifacts_ready')
        AND EXISTS (
            SELECT 1
            FROM revisions AS newer
            WHERE newer.package_base = current.package_base
              AND (
                  newer.created_at > current.created_at
                  OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
              )
        )
  );

UPDATE uploads
SET state = 'expired',
    last_error = 'STALE_RELEASE_BATCH',
    updated_at = CURRENT_TIMESTAMP
WHERE state IN ('issued', 'export_ready', 'verified')
  AND batch_id IN (
      SELECT release_batches.id
      FROM release_batches
      JOIN release_batch_revisions ON release_batch_revisions.batch_id = release_batches.id
      JOIN revisions AS current ON current.id = release_batch_revisions.revision_id
      WHERE release_batches.state IN ('awaiting_audit', 'building', 'build_failed', 'ready_to_publish', 'artifacts_ready')
        AND EXISTS (
            SELECT 1
            FROM revisions AS newer
            WHERE newer.package_base = current.package_base
              AND (
                  newer.created_at > current.created_at
                  OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
              )
        )
  );

UPDATE release_batches
SET state = 'superseded',
    failure_reason = 'STALE_RELEASE_BATCH',
    updated_at = CURRENT_TIMESTAMP
WHERE state IN ('awaiting_audit', 'building', 'build_failed', 'ready_to_publish', 'artifacts_ready')
  AND id IN (
      SELECT release_batch_revisions.batch_id
      FROM release_batch_revisions
      JOIN revisions AS current ON current.id = release_batch_revisions.revision_id
      WHERE EXISTS (
          SELECT 1
          FROM revisions AS newer
          WHERE newer.package_base = current.package_base
            AND (
                newer.created_at > current.created_at
                OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
            )
      )
  );

UPDATE revisions AS current
SET state = 'superseded'
WHERE state IN ('discovered', 'audit_pending', 'audit_approved', 'build_pending', 'built')
  AND EXISTS (
      SELECT 1
      FROM revisions AS newer
      WHERE newer.package_base = current.package_base
        AND (
            newer.created_at > current.created_at
            OR (newer.created_at = current.created_at AND newer.rowid > current.rowid)
        )
  );
