use anyhow::Context;
use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{str::FromStr, time::Duration};

static MIGRATOR: Migrator = sqlx::migrate!("../../migrations");

pub async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .context("SQLite URL 无效")?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await
        .context("无法连接 Controller SQLite")?;
    MIGRATOR.run(&pool).await.context("数据库迁移失败")?;
    Ok(pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn migrations_apply_to_empty_database() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'releases'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        let obsolete_vcs_review_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'vcs_rewrite_reviews'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(obsolete_vcs_review_table, 0);
        for table in ["workers", "job_evidence", "job_evidence_files"] {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?",
            )
            .bind(table)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, 0, "旧表仍存在：{table}");
        }
        let obsolete_job_columns: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('jobs') WHERE name IN ('preferred_worker_id', 'worker_id', 'required_role', 'required_labels_json', 'profile_sha256', 'limits_json')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(obsolete_job_columns, 0);
        let job_logs: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'job_logs'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(job_logs, 1);
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(busy_timeout, 10_000);
    }

    #[tokio::test]
    async fn stale_pipeline_cleanup_migration_is_idempotent_and_terminal() {
        let pool = connect("sqlite::memory:").await.unwrap();
        let now = Utc::now();
        sqlx::query("INSERT INTO revisions(id, package_base, aur_commit, upstream_version, input_sha256, audit_policy_version, state, metadata_json, created_at) VALUES ('old', 'demo', ?, '1-1', ?, 'v1', 'audit_pending', '{}', ?), ('new', 'demo', ?, '2-1', ?, 'v1', 'published', '{}', ?)")
            .bind("a".repeat(40))
            .bind("b".repeat(64))
            .bind(now - chrono::Duration::seconds(1))
            .bind("c".repeat(40))
            .bind("d".repeat(64))
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO audit_bundles(sha256, revision_id, policy_version, payload_json, coverage_json, deterministic_findings_json, state, created_at) VALUES (?, 'old', 'v1', '{}', '{}', '[]', 'agent_pending', ?)")
            .bind("e".repeat(64))
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO agent_runs(id, audit_bundle_sha256, tier, slot, attempt, adapter, model, adapter_version, prompt_version, status) VALUES ('run', ?, 'low', 1, 0, 'test', 'test', 'v1', 'v1', 'pending')")
            .bind("e".repeat(64))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO release_batches(id, state, graph_json, created_at, updated_at) VALUES ('batch', 'awaiting_audit', '{}', ?, ?)")
            .bind(now)
            .bind(now)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO release_batch_revisions(batch_id, revision_id, build_order) VALUES ('batch', 'old', 0)")
            .execute(&pool)
            .await
            .unwrap();

        sqlx::raw_sql(include_str!(
            "../../../migrations/0035_supersede_stale_release_pipelines.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0035_supersede_stale_release_pipelines.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();

        let old_revision: String =
            sqlx::query_scalar("SELECT state FROM revisions WHERE id = 'old'")
                .fetch_one(&pool)
                .await
                .unwrap();
        let audit: String = sqlx::query_scalar("SELECT state FROM audit_bundles")
            .fetch_one(&pool)
            .await
            .unwrap();
        let agent: String = sqlx::query_scalar("SELECT status FROM agent_runs")
            .fetch_one(&pool)
            .await
            .unwrap();
        let batch: String = sqlx::query_scalar("SELECT state FROM release_batches")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(old_revision, "superseded");
        assert_eq!(audit, "rejected");
        assert_eq!(agent, "failed");
        assert_eq!(batch, "superseded");
    }
}
