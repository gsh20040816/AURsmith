use crate::{error::ApiError, routes::AppState};
use aursmith_protocol::{ControlPlaneBackup, ManifestEntry, SignedEnvelope};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const DATABASE_FILE: &str = "controller.db";
const ENVELOPE_FILE: &str = "backup-envelope.json";

pub async fn create_if_due(state: &AppState) -> Result<(), ApiError> {
    let latest: Option<String> = sqlx::query_scalar(
        "SELECT created_at FROM control_plane_backups WHERE state = 'verified' ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_optional(&state.database)
    .await
    .map_err(ApiError::internal)?;
    if latest
        .and_then(|value| value.parse::<chrono::DateTime<Utc>>().ok())
        .is_some_and(|value| value > Utc::now() - Duration::hours(24))
    {
        return Ok(());
    }
    create(state).await.map(|_| ())
}

pub async fn create(state: &AppState) -> Result<Value, ApiError> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let root = Path::new(&state.config.backup_dir);
    fs::create_dir_all(root).map_err(ApiError::internal)?;
    reject_symlink(root)?;
    let staging = root.join(format!(".{id}.creating"));
    let final_directory = root.join(id.to_string());
    fs::create_dir(&staging).map_err(ApiError::internal)?;
    sqlx::query("INSERT INTO control_plane_backups(id, state, directory, created_at) VALUES (?, 'creating', ?, ?)")
        .bind(id.to_string()).bind(final_directory.to_string_lossy().as_ref()).bind(now)
        .execute(&state.database).await.map_err(ApiError::internal)?;
    match create_files(state, id, now, &staging).await {
        Ok((entry, envelope)) => {
            fs::rename(&staging, &final_directory).map_err(ApiError::internal)?;
            sync_directory(root).map_err(ApiError::internal)?;
            sqlx::query("UPDATE control_plane_backups SET state = 'verified', database_sha256 = ?, database_size = ?, envelope_json = ?, verified_at = ? WHERE id = ?")
                .bind(&entry.sha256).bind(i64::try_from(entry.size).map_err(ApiError::internal)?)
                .bind(serde_json::to_string(&envelope).map_err(ApiError::internal)?)
                .bind(Utc::now()).bind(id.to_string()).execute(&state.database).await.map_err(ApiError::internal)?;
            Ok(json!({"id": id, "state": "verified", "database": entry, "created_at": now}))
        }
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            sqlx::query(
                "UPDATE control_plane_backups SET state = 'failed', last_error = ? WHERE id = ?",
            )
            .bind(error.to_string())
            .bind(id.to_string())
            .execute(&state.database)
            .await
            .map_err(ApiError::internal)?;
            Err(ApiError::internal(error))
        }
    }
}

async fn create_files(
    state: &AppState,
    id: Uuid,
    created_at: chrono::DateTime<Utc>,
    staging: &Path,
) -> anyhow::Result<(ManifestEntry, SignedEnvelope)> {
    let database_path = staging.join(DATABASE_FILE);
    sqlx::query("VACUUM INTO ?")
        .bind(database_path.to_string_lossy().as_ref())
        .execute(&state.database)
        .await?;
    let entry = file_entry(&database_path, DATABASE_FILE)?;
    verify_sqlite(&database_path).await?;
    let payload = ControlPlaneBackup {
        backup_id: id,
        database: entry.clone(),
        source_git_commit: state.config.source_git_commit.clone(),
        created_at,
    };
    let envelope = SignedEnvelope::sign(
        "aursmith.control_plane_backup",
        &payload,
        &state.signing_key,
    )?;
    let envelope_path = staging.join(ENVELOPE_FILE);
    fs::write(&envelope_path, serde_json::to_vec_pretty(&envelope)?)?;
    fs::File::open(&database_path)?.sync_all()?;
    fs::File::open(&envelope_path)?.sync_all()?;
    sync_directory(staging)?;
    Ok((entry, envelope))
}

pub async fn list(database: &SqlitePool) -> Result<Value, ApiError> {
    let rows = sqlx::query("SELECT id, state, database_sha256, database_size, last_error, created_at, verified_at FROM control_plane_backups ORDER BY created_at DESC LIMIT 200")
        .fetch_all(database).await.map_err(ApiError::internal)?;
    Ok(json!({"items": rows.into_iter().map(|row| json!({
        "id": row.get::<String,_>("id"), "state": row.get::<String,_>("state"),
        "database_sha256": row.get::<Option<String>,_>("database_sha256"),
        "database_size": row.get::<Option<i64>,_>("database_size"),
        "last_error": row.get::<Option<String>,_>("last_error"),
        "created_at": row.get::<String,_>("created_at"),
        "verified_at": row.get::<Option<String>,_>("verified_at"),
    })).collect::<Vec<_>>() }))
}

pub async fn verify_record(state: &AppState, id: &str) -> Result<Value, ApiError> {
    let directory: String =
        sqlx::query_scalar("SELECT directory FROM control_plane_backups WHERE id = ?")
            .bind(id)
            .fetch_optional(&state.database)
            .await
            .map_err(ApiError::internal)?
            .ok_or_else(|| ApiError::not_found("控制面备份不存在"))?;
    let payload = verify_directory(
        Path::new(&directory),
        state.signing_key.verifying_key().as_bytes(),
    )
    .await
    .map_err(ApiError::internal)?;
    if payload.backup_id.to_string() != id {
        return Err(ApiError::conflict(
            "BACKUP_ID_MISMATCH",
            "备份目录与签名身份不一致",
        ));
    }
    sqlx::query("UPDATE control_plane_backups SET state = 'verified', database_sha256 = ?, database_size = ?, last_error = NULL, verified_at = ? WHERE id = ?")
        .bind(&payload.database.sha256).bind(i64::try_from(payload.database.size).map_err(ApiError::internal)?)
        .bind(Utc::now()).bind(id).execute(&state.database).await.map_err(ApiError::internal)?;
    Ok(json!({"id": id, "state": "verified", "database": payload.database}))
}

pub async fn verify_directory(
    directory: &Path,
    verifying_key: &[u8],
) -> anyhow::Result<ControlPlaneBackup> {
    reject_symlink_raw(directory)?;
    let envelope_path = directory.join(ENVELOPE_FILE);
    reject_regular_file(&envelope_path, 1024 * 1024)?;
    let envelope: SignedEnvelope = serde_json::from_slice(&fs::read(&envelope_path)?)?;
    if envelope.verifying_key != verifying_key {
        anyhow::bail!("控制面备份不是由当前 Controller 签署");
    }
    let payload: ControlPlaneBackup = envelope.verify("aursmith.control_plane_backup")?;
    if payload.database.path != DATABASE_FILE {
        anyhow::bail!("控制面备份数据库路径无效");
    }
    let database_path = directory.join(DATABASE_FILE);
    reject_regular_file(&database_path, u64::MAX)?;
    if file_entry(&database_path, DATABASE_FILE)? != payload.database {
        anyhow::bail!("控制面备份数据库摘要不匹配");
    }
    verify_sqlite(&database_path).await?;
    Ok(payload)
}

async fn verify_sqlite(path: &Path) -> anyhow::Result<()> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .foreign_keys(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let result: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    pool.close().await;
    if result != "ok" {
        anyhow::bail!("SQLite integrity_check 失败：{result}");
    }
    Ok(())
}

fn file_entry(path: &Path, logical_path: &str) -> anyhow::Result<ManifestEntry> {
    let mut file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(ManifestEntry {
        path: logical_path.into(),
        sha256: hex::encode(digest.finalize()),
        size: metadata.len(),
    })
}

fn reject_symlink(path: &Path) -> Result<(), ApiError> {
    reject_symlink_raw(path).map_err(ApiError::internal)
}

fn reject_symlink_raw(path: &Path) -> anyhow::Result<()> {
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        anyhow::bail!("备份路径不是普通目录");
    }
    Ok(())
}

fn reject_regular_file(path: &Path, maximum: u64) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() > maximum {
        anyhow::bail!("备份文件类型或大小无效：{}", path.display());
    }
    Ok(())
}

fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

pub fn database_path(database_url: &str) -> anyhow::Result<PathBuf> {
    let value = database_url
        .strip_prefix("sqlite://")
        .ok_or_else(|| anyhow::anyhow!("恢复只支持 sqlite:// 文件数据库"))?;
    if value.is_empty() || value.contains('?') || value == ":memory:" {
        anyhow::bail!("恢复目标必须是无查询参数的 SQLite 文件路径");
    }
    Ok(PathBuf::from(value))
}

pub async fn restore(config: &crate::config::Config, backup: &Path) -> anyhow::Result<()> {
    let signing_key = config.load_signing_key()?;
    let payload = verify_directory(backup, signing_key.verifying_key().as_bytes()).await?;
    let target = database_path(&config.database_url)?;
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("数据库目标缺少父目录"))?;
    fs::create_dir_all(parent)?;
    reject_symlink_raw(parent)?;
    let _lock = RestoreLock::acquire(parent.join(".aursmith-restore.lock"))?;
    let file_name = target
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("数据库目标文件名无效"))?;
    let staging = parent.join(format!(".{file_name}.restoring"));
    if staging.exists() {
        anyhow::bail!("存在未清理的恢复暂存文件：{}", staging.display());
    }
    fs::copy(backup.join(DATABASE_FILE), &staging)?;
    if file_entry(&staging, DATABASE_FILE)? != payload.database {
        let _ = fs::remove_file(&staging);
        anyhow::bail!("恢复复制后的数据库摘要不匹配");
    }
    verify_sqlite(&staging).await?;
    fs::File::open(&staging)?.sync_all()?;
    let recovery = parent.join("recovery").join(format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        payload.backup_id
    ));
    fs::create_dir_all(&recovery)?;
    for suffix in ["", "-wal", "-shm"] {
        let source = parent.join(format!("{file_name}{suffix}"));
        if source.exists() {
            reject_regular_file(&source, u64::MAX)?;
            fs::rename(&source, recovery.join(format!("{file_name}{suffix}")))?;
        }
    }
    if let Err(error) = fs::rename(&staging, &target) {
        let original = recovery.join(file_name);
        if original.exists() {
            let _ = fs::rename(original, &target);
        }
        return Err(error.into());
    }
    sync_directory(parent)?;
    Ok(())
}

struct RestoreLock(PathBuf);

impl RestoreLock {
    fn acquire(path: PathBuf) -> anyhow::Result<Self> {
        use std::fs::OpenOptions;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self(path))
    }
}

impl Drop for RestoreLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn restore_target_rejects_memory_and_query_parameters() {
        assert_eq!(
            database_path("sqlite:///tmp/controller.db").unwrap(),
            PathBuf::from("/tmp/controller.db")
        );
        assert!(database_path("sqlite::memory:").is_err());
        assert!(database_path("sqlite:///tmp/controller.db?mode=ro").is_err());
    }

    #[tokio::test]
    async fn signed_backup_restores_consistent_database_and_preserves_previous_copy() {
        let temporary = tempfile::tempdir().unwrap();
        let database_path = temporary.path().join("controller.db");
        let signing_key_path = temporary.path().join("controller-signing-key");
        let backup_dir = temporary.path().join("backups");
        let signing_key = SigningKey::from_bytes(&[19_u8; 32]);
        fs::write(&signing_key_path, hex::encode(signing_key.to_bytes())).unwrap();
        let config = crate::config::Config {
            bind_address: "127.0.0.1:0".into(),
            database_url: format!("sqlite://{}", database_path.display()),
            setup_token: "test-setup-token-long-enough".into(),
            signing_key_file: signing_key_path.to_string_lossy().into(),
            ssh_identity_source_file: "/不存在".into(),
            ssh_identity_file: "/不存在".into(),
            ssh_known_hosts_file: "/不存在".into(),
            secure_cookies: false,
            session_hours: 1,
            low_agent_endpoints: Vec::new(),
            high_agent_endpoint: String::new(),
            agent_daily_call_limit: 0,
            agent_monthly_call_limit: 0,
            agent_monthly_cost_limit_microusd: 0,
            repository_name: "aursmith".into(),
            source_git_commit: "test-commit".into(),
            repository_base_url: "https://repo.test".into(),
            webhook_url: None,
            webhook_hmac_secret_file: "/不存在".into(),
            ntfy_url: None,
            backup_dir: backup_dir.to_string_lossy().into(),
        };
        let database = crate::db::connect(&config.database_url).await.unwrap();
        sqlx::query("INSERT INTO system_settings(key, value_json, updated_at) VALUES ('backup-fixture', '\"before\"', ?)")
            .bind(Utc::now()).execute(&database).await.unwrap();
        let state = AppState::new(database.clone(), config.clone(), signing_key);
        let result = create(&state).await.unwrap();
        sqlx::query(
            "UPDATE system_settings SET value_json = '\"after\"' WHERE key = 'backup-fixture'",
        )
        .execute(&database)
        .await
        .unwrap();
        database.close().await;

        let backup = backup_dir.join(result["id"].as_str().unwrap());
        restore(&config, &backup).await.unwrap();
        let restored = crate::db::connect(&config.database_url).await.unwrap();
        let value: String = sqlx::query_scalar(
            "SELECT value_json FROM system_settings WHERE key = 'backup-fixture'",
        )
        .fetch_one(&restored)
        .await
        .unwrap();
        assert_eq!(value, "\"before\"");
        assert!(
            temporary
                .path()
                .join("recovery")
                .read_dir()
                .unwrap()
                .next()
                .is_some()
        );
    }
}
