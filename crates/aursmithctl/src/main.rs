use anyhow::{Context, bail};
use aursmith_domain::credentials;
use chrono::Utc;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{IsTerminal, Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    str::FromStr,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "aursmithctl", version, about = "AURsmith 容器内运维工具")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 只在公网核心设备本地管理唯一管理员。
    Admin {
        #[arg(long, env = "AURSMITH_DATABASE_URL", value_name = "DATABASE_URL")]
        database_url: String,
        #[command(subcommand)]
        command: AdminCommand,
    },
    Worker {
        #[arg(long, default_value = "/run/aursmith/worker.sock")]
        socket: PathBuf,
        #[command(subcommand)]
        command: WorkerCommand,
    },
    Doctor {
        #[arg(long, default_value = "controller")]
        role: String,
    },
    /// 供 OpenSSH forced command 调用，拒绝所有未明确允许的命令。
    SshGateway {
        #[arg(long, default_value = "/run/aursmith/worker.sock")]
        socket: PathBuf,
    },
    /// 将 Compose 文件型 secret 收敛到私有 tmpfs，然后替换为非 root sshd。
    RunSshd {
        #[arg(long, default_value = "/run/secrets/ssh_host_ed25519_key")]
        host_key_source: PathBuf,
        #[arg(long, default_value = "/run/secrets/authorized_keys")]
        authorized_keys_source: PathBuf,
        #[arg(long, default_value = "/run/private")]
        private_directory: PathBuf,
        #[arg(long, default_value = "/etc/ssh/sshd_config")]
        config: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AdminCommand {
    Init {
        #[arg(long, default_value = "admin")]
        username: String,
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    ResetPassword {
        #[arg(long)]
        password_file: Option<PathBuf>,
    },
    RevokeSessions,
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    Status,
    Query { job_id: String },
    AurSearch { query: String },
    AurInfo { names: Vec<String> },
    AurProviders { names: Vec<String> },
    OfficialInfo { names: Vec<String> },
    PublisherDoctor,
    AurSnapshot { package_base: String },
    PreparePushImport { envelope_file: PathBuf },
    FinalizePushImport { envelope_file: PathBuf },
    AuthorizeRelease { envelope_file: PathBuf },
    AuthorizeRollback { envelope_file: PathBuf },
    QueryRelease { release_id: String },
    ReleaseFiles { release_id: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Admin {
            database_url,
            command,
        } => {
            let database = connect_admin_database(&database_url).await?;
            let result = match command {
                AdminCommand::Init {
                    username,
                    password_file,
                } => {
                    let password = read_password(password_file.as_deref())?;
                    initialize_administrator(&database, &username, &password).await?
                }
                AdminCommand::ResetPassword { password_file } => {
                    let password = read_password(password_file.as_deref())?;
                    reset_administrator_password(&database, &password).await?
                }
                AdminCommand::RevokeSessions => revoke_administrator_sessions(&database).await?,
            };
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Worker { socket, command } => {
            let request = match command {
                WorkerCommand::Status => json!({"command": "status"}),
                WorkerCommand::Query { job_id } => json!({"command": "query", "job_id": job_id}),
                WorkerCommand::AurSearch { query } => {
                    json!({"command": "aur_search", "query": query})
                }
                WorkerCommand::AurInfo { names } => {
                    json!({"command": "aur_info", "names": names})
                }
                WorkerCommand::AurProviders { names } => {
                    json!({"command": "aur_providers", "names": names})
                }
                WorkerCommand::OfficialInfo { names } => {
                    json!({"command": "official_info", "names": names})
                }
                WorkerCommand::PublisherDoctor => json!({"command": "publisher_doctor"}),
                WorkerCommand::AurSnapshot { package_base } => {
                    json!({"command": "aur_snapshot", "package_base": package_base})
                }
                WorkerCommand::PreparePushImport { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "prepare_push_import", "envelope": envelope})
                }
                WorkerCommand::FinalizePushImport { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "finalize_push_import", "envelope": envelope})
                }
                WorkerCommand::AuthorizeRelease { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "authorize_release", "envelope": envelope})
                }
                WorkerCommand::AuthorizeRollback { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "authorize_rollback", "envelope": envelope})
                }
                WorkerCommand::QueryRelease { release_id } => {
                    json!({"command": "query_release", "release_id": release_id})
                }
                WorkerCommand::ReleaseFiles { release_id } => {
                    json!({"command": "release_files", "release_id": release_id})
                }
            };
            let response = worker_request(&socket, request).await?;
            println!("{}", serde_json::to_string_pretty(&response)?);
            if !response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
                bail!("Worker 返回失败");
            }
        }
        Command::Doctor { role } => doctor(&role)?,
        Command::SshGateway { socket } => ssh_gateway(&socket).await?,
        Command::RunSshd {
            host_key_source,
            authorized_keys_source,
            private_directory,
            config,
        } => run_sshd(
            &host_key_source,
            &authorized_keys_source,
            &private_directory,
            &config,
        )?,
    }
    Ok(())
}

async fn connect_admin_database(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)
        .context("SQLite URL 无效")?
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let database = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .context("无法连接既有 AURsmith SQLite；本地管理员命令不会创建数据库")?;
    let required_tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_schema WHERE type = 'table' AND name IN ('administrators', 'sessions')",
    )
    .fetch_one(&database)
    .await
    .context("无法检查 AURsmith 管理员 Schema")?;
    if required_tables != 2 {
        bail!("既有数据库缺少 administrators 或 sessions 表");
    }
    Ok(database)
}

fn read_password(password_file: Option<&Path>) -> anyhow::Result<String> {
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

async fn initialize_administrator(
    database: &SqlitePool,
    username: &str,
    password: &str,
) -> anyhow::Result<Value> {
    let username = username.trim();
    if username.chars().count() < 3 {
        bail!("管理员用户名至少需要 3 个字符");
    }
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
    let administrator_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO administrators(id, username, password_hash, created_at) VALUES (?, ?, ?, ?)",
    )
    .bind(&administrator_id)
    .bind(username)
    .bind(password_hash)
    .bind(Utc::now())
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(json!({
        "action": "initialized",
        "administrator_id": administrator_id,
        "username": username,
    }))
}

async fn reset_administrator_password(
    database: &SqlitePool,
    password: &str,
) -> anyhow::Result<Value> {
    credentials::validate_password(password).map_err(anyhow::Error::msg)?;
    let password_hash = credentials::hash_password(password)
        .map_err(|error| anyhow::anyhow!("无法计算密码摘要：{error}"))?;
    let mut transaction = database.begin().await?;
    let administrators: Vec<(String, String)> =
        sqlx::query_as("SELECT id, username FROM administrators ORDER BY created_at")
            .fetch_all(&mut *transaction)
            .await?;
    let [(administrator_id, username)] = administrators.as_slice() else {
        bail!("数据库必须且只能包含一个管理员");
    };
    sqlx::query("UPDATE administrators SET password_hash = ? WHERE id = ?")
        .bind(password_hash)
        .bind(administrator_id)
        .execute(&mut *transaction)
        .await?;
    let revoked = sqlx::query("DELETE FROM sessions")
        .execute(&mut *transaction)
        .await?
        .rows_affected();
    transaction.commit().await?;
    Ok(json!({
        "action": "password_reset",
        "administrator_id": administrator_id,
        "username": username,
        "revoked_sessions": revoked,
    }))
}

async fn revoke_administrator_sessions(database: &SqlitePool) -> anyhow::Result<Value> {
    let administrators: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM administrators")
        .fetch_one(database)
        .await?;
    if administrators != 1 {
        bail!("数据库必须且只能包含一个管理员");
    }
    let revoked = sqlx::query("DELETE FROM sessions")
        .execute(database)
        .await?
        .rows_affected();
    Ok(json!({
        "action": "sessions_revoked",
        "revoked_sessions": revoked,
    }))
}

fn run_sshd(
    host_key_source: &Path,
    authorized_keys_source: &Path,
    private_directory: &Path,
    config: &Path,
) -> anyhow::Result<()> {
    let sshd = [Path::new("/usr/bin/sshd"), Path::new("/usr/sbin/sshd")]
        .into_iter()
        .find(|candidate| candidate.is_file())
        .context("无法在受支持的固定路径找到 sshd")?;
    let host_key = private_directory.join("ssh_host_ed25519_key");
    let authorized_keys = private_directory.join("authorized_keys");
    materialize_private_file(host_key_source, &host_key, 64 * 1024)?;
    materialize_private_file(authorized_keys_source, &authorized_keys, 1024 * 1024)?;

    let ownership = ProcessCommand::new("/usr/bin/chown")
        .arg("10001:10001")
        .args([&host_key, &authorized_keys])
        .status()
        .context("无法设置 SSH 私有文件属主")?;
    if !ownership.success() {
        bail!("设置 SSH 私有文件属主失败");
    }

    let error = ProcessCommand::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            "10001",
            "--regid",
            "10001",
            "--clear-groups",
            "--bounding-set=-all",
            "--inh-caps=-all",
            "--ambient-caps=-all",
            "--no-new-privs",
        ])
        .arg(sshd)
        .args(["-D", "-e", "-f"])
        .arg(config)
        .exec();
    Err(error).context("无法启动 sshd")
}

fn materialize_private_file(source: &Path, target: &Path, maximum_size: u64) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(source)
        .with_context(|| format!("无法检查 secret {}", source.display()))?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > maximum_size {
        bail!("secret {} 类型或大小不合法", source.display());
    }
    let bytes =
        fs::read(source).with_context(|| format!("无法读取 secret {}", source.display()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
        .with_context(|| format!("无法创建私有文件 {}", target.display()))?;
    file.write_all(&bytes)
        .with_context(|| format!("无法写入私有文件 {}", target.display()))?;
    file.sync_all()
        .with_context(|| format!("无法同步私有文件 {}", target.display()))?;
    Ok(())
}

async fn ssh_gateway(socket: &PathBuf) -> anyhow::Result<()> {
    let original = env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let parts: Vec<_> = original.split_ascii_whitespace().collect();
    if parts.first() == Some(&"rsync") {
        return rsync_gateway(&parts);
    }
    let request = match parts.as_slice() {
        ["status"] => json!({"command": "status"}),
        ["query", job_id] if uuid_like(job_id) => json!({"command": "query", "job_id": job_id}),
        ["aur-search"] => read_limited_json_command("aur_search").await?,
        ["aur-info"] => read_limited_json_command("aur_info").await?,
        ["aur-providers"] => read_limited_json_command("aur_providers").await?,
        ["official-info"] => read_limited_json_command("official_info").await?,
        ["publisher-doctor"] => json!({"command": "publisher_doctor"}),
        ["aur-snapshot"] => read_limited_json_command("aur_snapshot").await?,
        ["prepare-push-import"]
        | ["finalize-push-import"]
        | ["authorize-release"]
        | ["authorize-rollback"] => {
            let mut bytes = Vec::new();
            tokio::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .await
                .context("读取签名授权 Envelope 失败")?;
            let envelope: Value =
                serde_json::from_slice(&bytes).context("签名授权 Envelope 不是有效 JSON")?;
            json!({
                "command": match parts[0] {
                    "prepare-push-import" => "prepare_push_import",
                    "finalize-push-import" => "finalize_push_import",
                    "authorize-rollback" => "authorize_rollback",
                    _ => "authorize_release",
                },
                "envelope": envelope
            })
        }
        ["query-release"] | ["release-files"] => {
            let mut bytes = Vec::new();
            tokio::io::stdin()
                .take(64)
                .read_to_end(&mut bytes)
                .await
                .context("读取 Release ID 失败")?;
            let release_id = String::from_utf8(bytes)?.trim().to_owned();
            if !uuid_like(&release_id) {
                bail!("Release ID 无效");
            }
            json!({
                "command": if parts[0] == "query-release" { "query_release" } else { "release_files" },
                "release_id": release_id
            })
        }
        _ => bail!("SSH 命令未被允许"),
    };
    let response = worker_request(socket, request).await?;
    println!("{}", serde_json::to_string(&response)?);
    if !response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        bail!("Worker 返回失败");
    }
    Ok(())
}

fn rsync_gateway(parts: &[&str]) -> anyhow::Result<()> {
    if parts.contains(&"--sender") {
        bail!("Publisher 的 rsync 入口只允许写入");
    }
    let error = ProcessCommand::new("/usr/sbin/rrsync")
        .args(["-wo", "/landing"])
        .exec();
    Err(error).context("无法启动官方 rrsync 收件器")
}

async fn read_limited_json_command(command: &str) -> anyhow::Result<Value> {
    let mut bytes = Vec::new();
    tokio::io::stdin()
        .take(1024 * 1024)
        .read_to_end(&mut bytes)
        .await
        .context("读取上游请求失败")?;
    let mut request: Value = serde_json::from_slice(&bytes).context("上游请求不是有效 JSON")?;
    let object = request
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("上游请求必须是 JSON 对象"))?;
    object.insert("command".into(), Value::String(command.into()));
    Ok(request)
}

fn uuid_like(value: &str) -> bool {
    value.len() == 36
        && value
            .chars()
            .all(|character| character.is_ascii_hexdigit() || character == '-')
}

async fn worker_request(socket: &PathBuf, request: Value) -> anyhow::Result<Value> {
    let mut stream = UnixStream::connect(socket)
        .await
        .with_context(|| format!("无法连接 Worker Socket {}", socket.display()))?;
    let mut bytes = serde_json::to_vec(&request)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await?;
    serde_json::from_str(&line).context("Worker 返回了无效 JSON")
}

fn doctor(role: &str) -> anyhow::Result<()> {
    let mut checks = Vec::new();
    checks.push(json!({"check": "proc", "ok": std::path::Path::new("/proc").exists()}));
    if role == "builder" {
        checks.push(
            json!({"check": "docker-cli", "ok": command_works("/usr/bin/docker", "--version")}),
        );
        checks.push(
            json!({"check": "docker-daemon", "ok": command_works("/usr/bin/docker", "version")}),
        );
        let jobs = env::var_os("AURSMITH_JOBS_DIR").map(PathBuf::from);
        checks.push(json!({
            "check": "jobs-directory",
            "path": jobs.as_ref().map(|path| path.display().to_string()),
            "ok": jobs.as_deref().is_some_and(jobs_directory_usable)
        }));
    }
    let ok = checks.iter().all(|check| check["ok"] == true);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({"role": role, "ok": ok, "checks": checks}))?
    );
    if !ok {
        bail!("Doctor 检查失败");
    }
    Ok(())
}

fn command_works(program: &str, argument: &str) -> bool {
    std::process::Command::new(program)
        .arg(argument)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn jobs_directory_usable(path: &Path) -> bool {
    if !path.is_absolute() || !path.is_dir() {
        return false;
    }
    let probe = path.join(format!(".aursmith-doctor-{}", std::process::id()));
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|file| file.sync_all())
        .is_ok();
    if created {
        fs::remove_file(probe).is_ok()
    } else {
        false
    }
}

#[cfg(test)]
mod tests;
