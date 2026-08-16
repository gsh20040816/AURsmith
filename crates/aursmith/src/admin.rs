use crate::credentials;
use anyhow::{Context, bail};
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::{
    fs::{self, File},
    io::{IsTerminal, Read},
    os::unix::fs::PermissionsExt,
    path::Path,
};

pub fn read_password(password_file: Option<&Path>) -> anyhow::Result<String> {
    const MAXIMUM_PASSWORD_BYTES: u64 = 64 * 1024;
    let mut bytes = Vec::new();
    if let Some(path) = password_file {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("无法检查密码文件 {}", path.display()))?;
        if !metadata.file_type().is_file()
            || metadata.len() == 0
            || metadata.len() > MAXIMUM_PASSWORD_BYTES
            || metadata.permissions().mode() & 0o077 != 0
        {
            bail!("密码文件必须是仅属主可读的有界普通文件");
        }
        File::open(path)
            .with_context(|| format!("无法打开密码文件 {}", path.display()))?
            .take(MAXIMUM_PASSWORD_BYTES + 1)
            .read_to_end(&mut bytes)?;
    } else {
        let stdin = std::io::stdin();
        reject_terminal_password_input(stdin.is_terminal())?;
        stdin
            .lock()
            .take(MAXIMUM_PASSWORD_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("无法从标准输入读取密码")?;
    }
    if bytes.len() as u64 > MAXIMUM_PASSWORD_BYTES {
        bail!("密码输入超过 64 KiB 上限");
    }
    let password = String::from_utf8(bytes)
        .context("密码必须是 UTF-8")?
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    credentials::validate_password(&password).map_err(anyhow::Error::msg)?;
    Ok(password)
}

fn reject_terminal_password_input(is_terminal: bool) -> anyhow::Result<()> {
    if is_terminal {
        bail!("拒绝从会回显的终端读取密码；请使用安全管道或权限为 0600 的密码文件");
    }
    Ok(())
}

pub async fn initialize(
    database: &SqlitePool,
    username: &str,
    password: &str,
) -> anyhow::Result<Value> {
    let username = validate_username(username)?;
    credentials::validate_password(password).map_err(anyhow::Error::msg)?;
    let password_hash = credentials::hash_password(password)
        .map_err(|error| anyhow::anyhow!("无法计算密码摘要：{error}"))?;
    let mut transaction = database.begin().await?;
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM administrators")
        .fetch_one(&mut *transaction)
        .await?;
    if count != 0 {
        bail!("管理员已经初始化；请使用 reset-password 修改密码");
    }
    sqlx::query(
        "INSERT INTO administrators(id, username, password_hash, created_at) VALUES (1, ?, ?, ?)",
    )
    .bind(username)
    .bind(password_hash)
    .bind(Utc::now())
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(json!({"action": "initialized", "administrator_id": 1, "username": username}))
}

pub async fn reset_password(database: &SqlitePool, password: &str) -> anyhow::Result<Value> {
    credentials::validate_password(password).map_err(anyhow::Error::msg)?;
    let password_hash = credentials::hash_password(password)
        .map_err(|error| anyhow::anyhow!("无法计算密码摘要：{error}"))?;
    let mut transaction = database.begin().await?;
    let username: Option<String> =
        sqlx::query_scalar("SELECT username FROM administrators WHERE id = 1")
            .fetch_optional(&mut *transaction)
            .await?;
    let username = username.context("数据库必须包含固定 id=1 的唯一管理员")?;
    sqlx::query("UPDATE administrators SET password_hash = ? WHERE id = 1")
        .bind(password_hash)
        .execute(&mut *transaction)
        .await?;
    let revoked = sqlx::query("DELETE FROM sessions")
        .execute(&mut *transaction)
        .await?
        .rows_affected();
    transaction.commit().await?;
    Ok(
        json!({"action": "password_reset", "administrator_id": 1, "username": username, "revoked_sessions": revoked}),
    )
}

pub async fn revoke_sessions(database: &SqlitePool) -> anyhow::Result<Value> {
    let administrator_exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM administrators WHERE id = 1")
            .fetch_one(database)
            .await?;
    if administrator_exists != 1 {
        bail!("数据库必须包含固定 id=1 的唯一管理员");
    }
    let revoked = sqlx::query("DELETE FROM sessions")
        .execute(database)
        .await?
        .rows_affected();
    Ok(json!({"action": "sessions_revoked", "revoked_sessions": revoked}))
}

fn validate_username(username: &str) -> anyhow::Result<&str> {
    let username = username.trim();
    if !(3..=64).contains(&username.chars().count()) || username.chars().any(char::is_control) {
        bail!("管理员用户名必须为 3 至 64 个非控制字符");
    }
    Ok(username)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn admin_lifecycle_enforces_fixed_identity_and_revokes_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let database = db::open_or_create(&directory.path().join("aursmith.db"))
            .await
            .unwrap();
        initialize(&database, "admin", "第一段足够长的密码-123456")
            .await
            .unwrap();
        assert!(
            initialize(&database, "other", "另一段足够长的密码-123456")
                .await
                .is_err()
        );
        let row: (i64, String) = sqlx::query_as("SELECT id, password_hash FROM administrators")
            .fetch_one(&database)
            .await
            .unwrap();
        assert_eq!(row.0, 1);
        assert!(credentials::verify_password(
            "第一段足够长的密码-123456",
            &row.1
        ));
        let now = Utc::now();
        sqlx::query("INSERT INTO sessions(token_sha256, administrator_id, created_at, expires_at, last_seen_at) VALUES (?, 1, ?, ?, ?)")
            .bind("a".repeat(64)).bind(now).bind(now + chrono::Duration::hours(1)).bind(now)
            .execute(&database).await.unwrap();
        let result = reset_password(&database, "重置后足够长的密码-123456")
            .await
            .unwrap();
        assert_eq!(result["revoked_sessions"], 1);
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
                .fetch_one(&database)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            revoke_sessions(&database).await.unwrap()["revoked_sessions"],
            0
        );
    }

    #[test]
    fn password_files_are_private_and_terminal_input_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("password");
        fs::write(&path, "足够长的文件密码-123456\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_password(Some(&path)).unwrap(),
            "足够长的文件密码-123456"
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_password(Some(&path)).is_err());
        assert!(reject_terminal_password_input(true).is_err());
        assert!(reject_terminal_password_input(false).is_ok());
    }
}
