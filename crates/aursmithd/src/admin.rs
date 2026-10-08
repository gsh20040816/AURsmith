//! `aursmithd admin`：在服务器本地管理唯一管理员。

use anyhow::{Context, bail};
use aursmith_core::credentials;
use chrono::Utc;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::{
    fs::{self, File},
    io::{IsTerminal, Read},
    os::unix::fs::PermissionsExt,
    path::Path,
};

const MAXIMUM_PASSWORD_BYTES: u64 = 64 * 1024;

/// 从 0600 文件或非终端标准输入读取口令；拒绝会回显的终端输入。
pub fn read_password(password_file: Option<&Path>) -> anyhow::Result<String> {
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
        File::open(path)?
            .take(MAXIMUM_PASSWORD_BYTES + 1)
            .read_to_end(&mut bytes)?;
    } else {
        let stdin = std::io::stdin();
        if stdin.is_terminal() {
            bail!("拒绝从会回显的终端读取密码；请使用安全管道或权限为 0600 的密码文件");
        }
        stdin
            .lock()
            .take(MAXIMUM_PASSWORD_BYTES + 1)
            .read_to_end(&mut bytes)?;
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

pub async fn init(db: &SqlitePool, username: &str, password: &str) -> anyhow::Result<Value> {
    if username.is_empty()
        || username.len() > 64
        || !username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    {
        bail!("用户名只能包含字母数字和 -_.，长度 1 到 64");
    }
    let hash = credentials::hash_password(password).map_err(|error| anyhow::anyhow!("{error}"))?;
    let inserted = sqlx::query(
        "INSERT OR IGNORE INTO admin(id, username, password_hash, updated_at) VALUES (1, ?, ?, ?)",
    )
    .bind(username)
    .bind(hash)
    .bind(Utc::now())
    .execute(db)
    .await?;
    if inserted.rows_affected() == 0 {
        bail!("管理员已存在；请使用 reset-password");
    }
    Ok(json!({"username": username, "created": true}))
}

pub async fn reset_password(db: &SqlitePool, password: &str) -> anyhow::Result<Value> {
    let hash = credentials::hash_password(password).map_err(|error| anyhow::anyhow!("{error}"))?;
    let mut transaction = db.begin().await?;
    let updated = sqlx::query("UPDATE admin SET password_hash = ?, updated_at = ? WHERE id = 1")
        .bind(hash)
        .bind(Utc::now())
        .execute(&mut *transaction)
        .await?;
    if updated.rows_affected() == 0 {
        bail!("管理员尚未初始化");
    }
    let revoked = sqlx::query("DELETE FROM sessions")
        .execute(&mut *transaction)
        .await?;
    transaction.commit().await?;
    Ok(json!({"password_reset": true, "revoked_sessions": revoked.rows_affected()}))
}

pub async fn revoke_sessions(db: &SqlitePool) -> anyhow::Result<Value> {
    let revoked = sqlx::query("DELETE FROM sessions").execute(db).await?;
    Ok(json!({"revoked_sessions": revoked.rows_affected()}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn admin_lifecycle() {
        let db = crate::db::memory().await;
        init(&db, "admin", "足够长的测试密码-123456").await.unwrap();
        assert!(init(&db, "admin", "足够长的测试密码-123456").await.is_err());
        sqlx::query("INSERT INTO sessions(token_sha256, created_at, last_seen_at, expires_at) VALUES ('x', '', '', '')")
            .execute(&db)
            .await
            .unwrap();
        let result = reset_password(&db, "另一个足够长的密码-654321")
            .await
            .unwrap();
        assert_eq!(result["revoked_sessions"], 1);
        let hash: String = sqlx::query_scalar("SELECT password_hash FROM admin")
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(credentials::verify_password(
            "另一个足够长的密码-654321",
            &hash
        ));
    }

    #[test]
    fn password_files_must_be_private() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("password");
        fs::write(&path, "足够长的测试密码-123456\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_password(Some(&path)).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            read_password(Some(&path)).unwrap(),
            "足够长的测试密码-123456"
        );
    }
}
