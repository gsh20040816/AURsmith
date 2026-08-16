use anyhow::{Context, bail};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    fs::OpenOptions,
    future::Future,
    io::ErrorKind,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

const APPLICATION_ID: i64 = 0x4155_5253;
const SCHEMA_VERSION: i64 = 2;
const CORE_MIGRATION: &str = include_str!("../../../migrations/0001_core.sql");

pub async fn open_or_create(path: &Path) -> anyhow::Result<SqlitePool> {
    open_or_create_with_migration(path, CORE_MIGRATION).await
}

async fn open_or_create_with_migration(path: &Path, migration: &str) -> anyhow::Result<SqlitePool> {
    open_or_create_with_connector(
        path,
        migration,
        |path| async move { connect(&path, 5).await },
    )
    .await
}

async fn open_or_create_with_connector<F, Fut>(
    path: &Path,
    migration: &str,
    connector: F,
) -> anyhow::Result<SqlitePool>
where
    F: FnOnce(PathBuf) -> Fut,
    Fut: Future<Output = anyhow::Result<SqlitePool>>,
{
    let fresh = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(_) => true,
        Err(error) if error.kind() == ErrorKind::AlreadyExists => false,
        Err(error) => {
            return Err(error).with_context(|| format!("无法创建 fresh SQLite {}", path.display()));
        }
    };
    let database = match connector(path.to_path_buf()).await {
        Ok(database) => database,
        Err(error) => {
            if fresh {
                remove_failed_fresh_database(path, &error)?;
            }
            return Err(error);
        }
    };
    let prepared = async {
        if fresh {
            sqlx::raw_sql(migration)
                .execute(&database)
                .await
                .context("无法初始化 AURsmith core Schema")?;
        }
        validate_schema(&database).await
    }
    .await;
    if let Err(error) = prepared {
        database.close().await;
        if fresh {
            remove_failed_fresh_database(path, &error)?;
        }
        return Err(error);
    }
    Ok(database)
}

fn remove_failed_fresh_database(path: &Path, original_error: &anyhow::Error) -> anyhow::Result<()> {
    std::fs::remove_file(path).with_context(|| {
        format!(
            "fresh 数据库准备失败，且无法清理半成品 {}；原始错误：{original_error:#}",
            path.display()
        )
    })
}

pub async fn open_existing(path: &Path, maximum_connections: u32) -> anyhow::Result<SqlitePool> {
    if !path.is_file() {
        bail!("本地命令只连接既有 AURsmith SQLite：{}", path.display());
    }
    let database = connect(path, maximum_connections).await?;
    validate_schema(&database).await?;
    Ok(database)
}

async fn connect(path: &Path, maximum_connections: u32) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    SqlitePoolOptions::new()
        .max_connections(maximum_connections)
        .connect_with(options)
        .await
        .with_context(|| format!("无法连接 SQLite {}", path.display()))
}

async fn validate_schema(database: &SqlitePool) -> anyhow::Result<()> {
    let application_id: i64 = sqlx::query_scalar("PRAGMA application_id")
        .fetch_one(database)
        .await?;
    let user_version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(database)
        .await?;
    if application_id != APPLICATION_ID || user_version != SCHEMA_VERSION {
        bail!("数据库不是 AURsmith core Schema；禁止打开或迁移旧数据库");
    }
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .fetch_all(database)
    .await?;
    if tables
        != [
            "administrators",
            "aur_reviews",
            "sessions",
            "tracked_packages",
        ]
    {
        bail!("AURsmith core Schema 表集合不匹配");
    }
    for (table, expected) in [
        (
            "administrators",
            &["id", "username", "password_hash", "created_at"][..],
        ),
        (
            "aur_reviews",
            &[
                "pkgbase",
                "aur_commit",
                "tree_sha256",
                "comparison_kind",
                "baseline_aur_commit",
                "baseline_tree_sha256",
                "full_reason",
                "status",
                "blocker",
                "review_json_sha256",
                "changes_diff_sha256",
                "findings_json_sha256",
                "created_at",
                "updated_at",
            ][..],
        ),
        (
            "sessions",
            &[
                "token_sha256",
                "administrator_id",
                "created_at",
                "expires_at",
                "last_seen_at",
            ][..],
        ),
        (
            "tracked_packages",
            &[
                "pkgbase",
                "state",
                "approved_aur_commit",
                "approved_tree_sha256",
                "approved_at",
                "last_checked_at",
                "last_error",
                "created_at",
                "updated_at",
            ][..],
        ),
    ] {
        let query = format!("PRAGMA table_info({table})");
        let columns = sqlx::query(&query)
            .fetch_all(database)
            .await?
            .into_iter()
            .map(|row| row.get::<String, _>("name"))
            .collect::<Vec<_>>();
        if columns != expected {
            bail!("AURsmith core Schema 的 {table} 列集合不匹配");
        }
    }
    let current_index_sql: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_schema WHERE type = 'index' AND name = 'aur_reviews_one_current_per_package' AND tbl_name = 'aur_reviews'",
    )
    .fetch_optional(database)
    .await?;
    let normalized = current_index_sql
        .as_deref()
        .map(normalize_schema_sql)
        .unwrap_or_default();
    let expected = normalize_schema_sql(
        "CREATE UNIQUE INDEX aur_reviews_one_current_per_package ON aur_reviews(pkgbase) WHERE status IN ('prepared', 'input_blocked')",
    );
    if normalized != expected {
        bail!("AURsmith core Schema 缺少精确的单 current 审查 partial unique index");
    }
    Ok(())
}

fn normalize_schema_sql(sql: &str) -> String {
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fresh_database_has_exactly_four_tables_and_reopens() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("aursmith.db");
        let database = open_or_create(&path).await.unwrap();
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .fetch_all(&database)
        .await
        .unwrap();
        assert_eq!(
            tables,
            [
                "administrators",
                "aur_reviews",
                "sessions",
                "tracked_packages"
            ]
        );
        database.close().await;
        open_or_create(&path).await.unwrap().close().await;
        open_existing(&path, 1).await.unwrap().close().await;
    }

    #[tokio::test]
    async fn missing_empty_and_legacy_databases_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing.db");
        assert!(open_existing(&missing, 1).await.is_err());
        assert!(!missing.exists());

        for name in ["empty.db", "legacy.db"] {
            let path = directory.path().join(name);
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .unwrap();
            let database = connect(&path, 1).await.unwrap();
            if name == "legacy.db" {
                sqlx::query("CREATE TABLE administrators(id TEXT PRIMARY KEY, username TEXT)")
                    .execute(&database)
                    .await
                    .unwrap();
                sqlx::query("CREATE TABLE workers(id TEXT PRIMARY KEY)")
                    .execute(&database)
                    .await
                    .unwrap();
            }
            database.close().await;
            assert!(open_or_create(&path).await.is_err(), "{name}");
        }
    }

    #[tokio::test]
    async fn failed_fresh_schema_is_removed_and_the_same_path_can_retry() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("aursmith.db");
        let wrong_schema = r#"
            PRAGMA application_id = 0x41555253;
            PRAGMA user_version = 2;
            CREATE TABLE wrong_table(id INTEGER PRIMARY KEY) STRICT;
        "#;
        assert!(
            open_or_create_with_migration(&path, wrong_schema)
                .await
                .is_err()
        );
        assert!(!path.exists(), "失败的 fresh 初始化不得留下半成品数据库");
        open_or_create(&path).await.unwrap().close().await;
    }

    #[tokio::test]
    async fn failed_fresh_connect_is_removed_but_an_existing_database_is_never_deleted() {
        let directory = tempfile::tempdir().unwrap();
        let fresh_path = directory.path().join("fresh.db");
        let forced_failure =
            |_: PathBuf| async { Err::<SqlitePool, _>(anyhow::anyhow!("强制连接失败")) };
        assert!(
            open_or_create_with_connector(&fresh_path, CORE_MIGRATION, forced_failure)
                .await
                .is_err()
        );
        assert!(
            !fresh_path.exists(),
            "fresh connect 失败必须清理本次新建文件"
        );
        open_or_create(&fresh_path).await.unwrap().close().await;

        let existing_path = directory.path().join("existing.db");
        open_or_create(&existing_path).await.unwrap().close().await;
        assert!(
            open_or_create_with_connector(&existing_path, CORE_MIGRATION, |_: PathBuf| async {
                Err::<SqlitePool, _>(anyhow::anyhow!("强制既有库连接失败"))
            })
            .await
            .is_err()
        );
        assert!(
            existing_path.is_file(),
            "任何既有数据库都不得被失败清理删除"
        );
    }

    #[tokio::test]
    async fn approved_baseline_is_absent_or_complete_and_verifiable() {
        let directory = tempfile::tempdir().unwrap();
        let database = open_or_create(&directory.path().join("aursmith.db"))
            .await
            .unwrap();
        let now = chrono::Utc::now();
        sqlx::query("INSERT INTO tracked_packages(pkgbase, state, created_at, updated_at) VALUES ('paru', 'active', ?, ?)")
            .bind(now).bind(now).execute(&database).await.unwrap();
        sqlx::query("INSERT INTO tracked_packages(pkgbase, state, approved_aur_commit, approved_tree_sha256, approved_at, created_at, updated_at) VALUES ('yay', 'paused', ?, ?, ?, ?, ?)")
            .bind("a".repeat(40)).bind("b".repeat(64)).bind(now).bind(now).bind(now)
            .execute(&database).await.unwrap();
        for (pkgbase, commit, tree, approved_at) in [
            ("partial", Some("a".repeat(40)), None, None),
            (
                "bad-commit",
                Some("z".repeat(40)),
                Some("b".repeat(64)),
                Some(now.to_rfc3339()),
            ),
            (
                "bad-tree",
                Some("a".repeat(40)),
                Some("b".repeat(63)),
                Some(now.to_rfc3339()),
            ),
        ] {
            assert!(sqlx::query("INSERT INTO tracked_packages(pkgbase, state, approved_aur_commit, approved_tree_sha256, approved_at, created_at, updated_at) VALUES (?, 'active', ?, ?, ?, ?, ?)")
                .bind(pkgbase).bind(commit).bind(tree).bind(approved_at).bind(now).bind(now)
                .execute(&database).await.is_err(), "{pkgbase}");
        }
    }

    #[tokio::test]
    async fn version_one_intermediate_database_is_rejected_without_migration() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("intermediate.db");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        let database = connect(&path, 1).await.unwrap();
        sqlx::raw_sql(
            "PRAGMA application_id = 0x41555253; PRAGMA user_version = 1; \
             CREATE TABLE administrators(id INTEGER PRIMARY KEY) STRICT;",
        )
        .execute(&database)
        .await
        .unwrap();
        database.close().await;

        assert!(open_or_create(&path).await.is_err());
        assert!(path.is_file(), "既有中间数据库不得被删除或迁移");
    }

    #[tokio::test]
    async fn database_enforces_one_current_review_per_package() {
        let directory = tempfile::tempdir().unwrap();
        let database = open_or_create(&directory.path().join("aursmith.db"))
            .await
            .unwrap();
        let now = chrono::Utc::now();
        sqlx::query("INSERT INTO tracked_packages(pkgbase, state, created_at, updated_at) VALUES ('paru', 'active', ?, ?)")
            .bind(now)
            .bind(now)
            .execute(&database)
            .await
            .unwrap();
        for commit in ["a", "b"] {
            let result = sqlx::query(
                "INSERT INTO aur_reviews(pkgbase, aur_commit, tree_sha256, comparison_kind, full_reason, status, review_json_sha256, findings_json_sha256, created_at, updated_at) VALUES ('paru', ?, ?, 'full', 'initial', 'prepared', ?, ?, ?, ?)",
            )
            .bind(commit.repeat(40))
            .bind("c".repeat(64))
            .bind("d".repeat(64))
            .bind("e".repeat(64))
            .bind(now)
            .bind(now)
            .execute(&database)
            .await;
            if commit == "a" {
                result.unwrap();
            } else {
                assert!(result.is_err());
            }
        }
        let invalid_superseded = sqlx::query(
            "INSERT INTO aur_reviews(pkgbase, aur_commit, comparison_kind, full_reason, status, review_json_sha256, findings_json_sha256, created_at, updated_at) VALUES ('paru', ?, 'full', 'initial', 'superseded', ?, ?, ?, ?)",
        )
        .bind("f".repeat(40))
        .bind("d".repeat(64))
        .bind("e".repeat(64))
        .bind(now)
        .bind(now)
        .execute(&database)
        .await;
        assert!(
            invalid_superseded.is_err(),
            "superseded 不得绕过 prepared/input_blocked 数据形状"
        );
    }

    #[tokio::test]
    async fn missing_or_weakened_current_review_index_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        for weakened in [false, true] {
            let path = directory.path().join(format!("index-{weakened}.db"));
            let database = open_or_create(&path).await.unwrap();
            let mut connection = database.acquire().await.unwrap();
            sqlx::query("DROP INDEX aur_reviews_one_current_per_package")
                .execute(&mut *connection)
                .await
                .unwrap();
            if weakened {
                sqlx::query(
                    "CREATE INDEX aur_reviews_one_current_per_package ON aur_reviews(pkgbase)",
                )
                .execute(&mut *connection)
                .await
                .unwrap();
            }
            drop(connection);
            database.close().await;
            assert!(open_existing(&path, 1).await.is_err());
        }
    }
}
