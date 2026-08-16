use crate::error::ApiError;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone, Serialize)]
pub struct TrackedPackage {
    pub pkgbase: String,
    pub state: String,
    pub approved_aur_commit: Option<String>,
    pub approved_tree_sha256: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub current_review_commit: Option<String>,
    pub current_review_tree_sha256: Option<String>,
    pub current_review_status: Option<String>,
    pub current_review_comparison: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub async fn list(database: &SqlitePool) -> Result<Vec<TrackedPackage>, ApiError> {
    let rows = sqlx::query("SELECT p.pkgbase, p.state, p.approved_aur_commit, p.approved_tree_sha256, p.approved_at, p.last_checked_at, p.last_error, p.created_at, p.updated_at, r.aur_commit AS current_review_commit, r.tree_sha256 AS current_review_tree_sha256, r.status AS current_review_status, r.comparison_kind AS current_review_comparison FROM tracked_packages p LEFT JOIN aur_reviews r ON r.pkgbase = p.pkgbase AND r.status IN ('prepared', 'input_blocked') ORDER BY p.pkgbase")
        .fetch_all(database)
        .await
        .map_err(ApiError::internal)?;
    rows.into_iter()
        .map(|row| {
            Ok(TrackedPackage {
                pkgbase: row.try_get("pkgbase").map_err(ApiError::internal)?,
                state: row.try_get("state").map_err(ApiError::internal)?,
                approved_aur_commit: row
                    .try_get("approved_aur_commit")
                    .map_err(ApiError::internal)?,
                approved_tree_sha256: row
                    .try_get("approved_tree_sha256")
                    .map_err(ApiError::internal)?,
                approved_at: row.try_get("approved_at").map_err(ApiError::internal)?,
                last_checked_at: row.try_get("last_checked_at").map_err(ApiError::internal)?,
                last_error: row.try_get("last_error").map_err(ApiError::internal)?,
                current_review_commit: row
                    .try_get("current_review_commit")
                    .map_err(ApiError::internal)?,
                current_review_tree_sha256: row
                    .try_get("current_review_tree_sha256")
                    .map_err(ApiError::internal)?,
                current_review_status: row
                    .try_get("current_review_status")
                    .map_err(ApiError::internal)?,
                current_review_comparison: row
                    .try_get("current_review_comparison")
                    .map_err(ApiError::internal)?,
                created_at: row.try_get("created_at").map_err(ApiError::internal)?,
                updated_at: row.try_get("updated_at").map_err(ApiError::internal)?,
            })
        })
        .collect()
}

pub async fn add(database: &SqlitePool, pkgbase: &str) -> Result<(), ApiError> {
    validate_pkgbase(pkgbase)?;
    let now = Utc::now();
    let inserted = sqlx::query("INSERT INTO tracked_packages(pkgbase, state, created_at, updated_at) VALUES (?, 'active', ?, ?) ON CONFLICT(pkgbase) DO NOTHING")
        .bind(pkgbase)
        .bind(now)
        .bind(now)
        .execute(database)
        .await
        .map_err(ApiError::internal)?;
    if inserted.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "PACKAGE_EXISTS",
            "该 pkgbase 已经在跟踪列表中",
        ));
    }
    if inserted.rows_affected() != 1 {
        return Err(ApiError::internal("添加 pkgbase 影响行数不是 1"));
    }
    Ok(())
}

pub async fn set_state(database: &SqlitePool, pkgbase: &str, state: &str) -> Result<(), ApiError> {
    validate_pkgbase(pkgbase)?;
    if !matches!(state, "active" | "paused") {
        return Err(ApiError::internal("代码传入了非法包状态"));
    }
    let updated =
        sqlx::query("UPDATE tracked_packages SET state = ?, updated_at = ? WHERE pkgbase = ?")
            .bind(state)
            .bind(Utc::now())
            .bind(pkgbase)
            .execute(database)
            .await
            .map_err(ApiError::internal)?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found("pkgbase 不在跟踪列表中"));
    }
    if updated.rows_affected() != 1 {
        return Err(ApiError::internal("修改 pkgbase 状态影响行数不是 1"));
    }
    Ok(())
}

pub async fn delete(database: &SqlitePool, pkgbase: &str) -> Result<(), ApiError> {
    validate_pkgbase(pkgbase)?;
    let deleted = sqlx::query("DELETE FROM tracked_packages WHERE pkgbase = ?")
        .bind(pkgbase)
        .execute(database)
        .await
        .map_err(ApiError::internal)?;
    if deleted.rows_affected() == 0 {
        return Err(ApiError::not_found("pkgbase 不在跟踪列表中"));
    }
    if deleted.rows_affected() != 1 {
        return Err(ApiError::internal("删除 pkgbase 影响行数不是 1"));
    }
    Ok(())
}

pub fn validate_pkgbase(pkgbase: &str) -> Result<(), ApiError> {
    let valid_length = (1..=128).contains(&pkgbase.len());
    let valid_characters = pkgbase.bytes().all(|byte| {
        byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || matches!(byte, b'@' | b'.' | b'_' | b'+' | b'-')
    });
    let valid_first = pkgbase
        .as_bytes()
        .first()
        .is_some_and(|byte| !matches!(byte, b'.' | b'-'));
    if !valid_length || !valid_characters || !valid_first {
        return Err(ApiError::bad_request(
            "INVALID_PKGBASE",
            "pkgbase 必须为 1 至 128 个小写 ASCII 字母、数字或 @._+-，且不能以点或连字符开头",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[test]
    fn pkgbase_accepts_arch_punctuation_without_allowing_paths_or_markup() {
        for valid in [
            "yay",
            "lib32-foo",
            "python_foo",
            "name+feature",
            "mail@host",
            "a.b",
            "@scope",
            "_internal",
            "+feature",
        ] {
            validate_pkgbase(valid).unwrap();
        }
        let too_long = "a".repeat(129);
        for invalid in [
            "",
            ".hidden",
            "-option",
            "Uppercase",
            "two words",
            "path/name",
            "<script>",
            "中文",
            &too_long,
        ] {
            assert!(validate_pkgbase(invalid).is_err(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn crud_is_persistent_and_delete_is_physical() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("aursmith.db");
        let database = db::open_or_create(&path).await.unwrap();
        add(&database, "paru").await.unwrap();
        assert!(add(&database, "paru").await.is_err());
        set_state(&database, "paru", "paused").await.unwrap();
        database.close().await;

        let database = db::open_existing(&path, 1).await.unwrap();
        assert_eq!(list(&database).await.unwrap()[0].state, "paused");
        set_state(&database, "paru", "active").await.unwrap();
        delete(&database, "paru").await.unwrap();
        assert!(list(&database).await.unwrap().is_empty());
        assert!(delete(&database, "paru").await.is_err());
    }
}
