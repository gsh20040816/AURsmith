use super::*;

fn sqlite_url(path: &Path) -> String {
    format!("sqlite://{}", path.display())
}

async fn create_test_admin_schema(path: &Path) {
    let database = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&sqlite_url(path))
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE administrators(id TEXT PRIMARY KEY, username TEXT NOT NULL UNIQUE, password_hash TEXT NOT NULL, created_at TEXT NOT NULL)",
    )
    .execute(&database)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TABLE sessions(token_sha256 TEXT PRIMARY KEY, administrator_id TEXT NOT NULL REFERENCES administrators(id) ON DELETE CASCADE, created_at TEXT NOT NULL, expires_at TEXT NOT NULL, last_seen_at TEXT NOT NULL)",
    )
    .execute(&database)
    .await
    .unwrap();
    database.close().await;
}

#[test]
fn builder_jobs_directory_must_be_absolute_and_writable() {
    let directory = tempfile::tempdir().unwrap();
    assert!(jobs_directory_usable(directory.path()));
    assert!(!jobs_directory_usable(Path::new("relative/jobs")));
}

#[tokio::test]
async fn local_admin_commands_enforce_one_administrator_and_revoke_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("controller.db");
    create_test_admin_schema(&database_path).await;
    let database = connect_admin_database(&sqlite_url(&database_path))
        .await
        .unwrap();
    initialize_administrator(&database, "admin", "第一段足够长的密码-123456")
        .await
        .unwrap();
    assert!(
        initialize_administrator(&database, "other", "另一段足够长的密码-123456")
            .await
            .is_err()
    );
    let row: (String, String) = sqlx::query_as("SELECT id, password_hash FROM administrators")
        .fetch_one(&database)
        .await
        .unwrap();
    assert!(credentials::verify_password(
        "第一段足够长的密码-123456",
        &row.1
    ));
    sqlx::query("INSERT INTO sessions(token_sha256, administrator_id, created_at, expires_at, last_seen_at) VALUES ('token', ?, ?, ?, ?)")
        .bind(&row.0)
        .bind(Utc::now())
        .bind(Utc::now() + chrono::Duration::hours(1))
        .bind(Utc::now())
        .execute(&database)
        .await
        .unwrap();

    let reset = reset_administrator_password(&database, "重置后足够长的密码-123456")
        .await
        .unwrap();
    assert_eq!(reset["revoked_sessions"], 1);
    let password_hash: String = sqlx::query_scalar("SELECT password_hash FROM administrators")
        .fetch_one(&database)
        .await
        .unwrap();
    assert!(!credentials::verify_password(
        "第一段足够长的密码-123456",
        &password_hash
    ));
    assert!(credentials::verify_password(
        "重置后足够长的密码-123456",
        &password_hash
    ));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM sessions")
            .fetch_one(&database)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        revoke_administrator_sessions(&database).await.unwrap()["revoked_sessions"],
        0
    );
}

#[tokio::test]
async fn local_admin_commands_refuse_missing_database_or_schema() {
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("missing.db");
    assert!(connect_admin_database(&sqlite_url(&missing)).await.is_err());
    assert!(!missing.exists(), "管理员命令不得静默创建数据库");

    let empty = directory.path().join("empty.db");
    let database = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::from_str(&sqlite_url(&empty))
                .unwrap()
                .create_if_missing(true),
        )
        .await
        .unwrap();
    database.close().await;
    assert!(connect_admin_database(&sqlite_url(&empty)).await.is_err());
}

#[test]
fn password_files_must_be_private_regular_files() {
    let directory = tempfile::tempdir().unwrap();
    let password = directory.path().join("password");
    fs::write(&password, "足够长的文件密码-123456\n").unwrap();
    fs::set_permissions(&password, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_password(Some(&password)).unwrap(),
        "足够长的文件密码-123456"
    );
    fs::set_permissions(&password, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(read_password(Some(&password)).is_err());
}

#[test]
fn terminal_password_input_is_rejected_before_reading() {
    assert!(reject_terminal_password_input(true).is_err());
    assert!(reject_terminal_password_input(false).is_ok());
}

#[test]
fn publisher_rsync_gateway_rejects_read_requests() {
    assert!(
        rsync_gateway(&["rsync", "--server", "--sender", ".", "/landing"])
            .unwrap_err()
            .to_string()
            .contains("只允许写入")
    );
}
