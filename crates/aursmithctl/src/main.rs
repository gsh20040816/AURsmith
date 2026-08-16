use anyhow::{Context, bail};
use aursmith_domain::credentials;
use aursmith_protocol::{BuildProfileSpec, ManifestEntry};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
use std::{
    env,
    ffi::OsString,
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
    /// 生成 Controller Ed25519 密钥；生产部署应直接写入私有文件，避免私钥经过终端。
    GenerateControllerKey {
        #[arg(long, requires = "public_key_file")]
        private_key_file: Option<PathBuf>,
        #[arg(long, requires = "private_key_file")]
        public_key_file: Option<PathBuf>,
    },
    /// 从固定 Profile 构建产物导出待 Controller 授权的候选清单。
    ExportProfile {
        #[arg(long, default_value = "/opt/aursmith-profile")]
        source: PathBuf,
        #[arg(long, default_value = "/out")]
        output: PathBuf,
        #[arg(long, default_value = "base")]
        name: String,
    },
    /// 仅供 Publisher 的 rsync 固定 remote-shell 使用。
    RsyncSsh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        arguments: Vec<OsString>,
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
    Drain,
    Query {
        job_id: String,
    },
    Submit {
        envelope_file: PathBuf,
    },
    AurSearch {
        query: String,
    },
    AurInfo {
        names: Vec<String>,
    },
    AurProviders {
        names: Vec<String>,
    },
    OfficialInfo {
        names: Vec<String>,
    },
    PublisherDoctor,
    AurSnapshot {
        package_base: String,
        #[arg(long)]
        previous_vcs_commit: Option<String>,
    },
    AuthorizeExport {
        envelope_file: PathBuf,
    },
    AuthorizeImport {
        envelope_file: PathBuf,
    },
    PreparePushImport {
        envelope_file: PathBuf,
    },
    FinalizePushImport {
        envelope_file: PathBuf,
    },
    CompleteExport {
        envelope_file: PathBuf,
    },
    AuthorizeRelease {
        envelope_file: PathBuf,
    },
    AuthorizeRollback {
        envelope_file: PathBuf,
    },
    QueryRelease {
        release_id: String,
    },
    ReleaseFiles {
        release_id: String,
    },
    Inventory {
        #[arg(long)]
        full_digest: bool,
    },
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
                WorkerCommand::Drain => json!({"command": "drain"}),
                WorkerCommand::Query { job_id } => json!({"command": "query", "job_id": job_id}),
                WorkerCommand::Submit { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file)
                        .await
                        .with_context(|| format!("无法读取 {}", envelope_file.display()))?;
                    let envelope: Value =
                        serde_json::from_slice(&bytes).context("Envelope 文件不是有效 JSON")?;
                    json!({"command": "submit", "envelope": envelope})
                }
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
                WorkerCommand::AurSnapshot {
                    package_base,
                    previous_vcs_commit,
                } => {
                    json!({"command": "aur_snapshot", "package_base": package_base, "previous_vcs_commit": previous_vcs_commit})
                }
                WorkerCommand::AuthorizeExport { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "authorize_export", "envelope": envelope})
                }
                WorkerCommand::AuthorizeImport { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "authorize_import", "envelope": envelope})
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
                WorkerCommand::CompleteExport { envelope_file } => {
                    let bytes = tokio::fs::read(&envelope_file).await?;
                    let envelope: Value = serde_json::from_slice(&bytes)?;
                    json!({"command": "complete_export", "envelope": envelope})
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
                WorkerCommand::Inventory { full_digest } => {
                    json!({"command": "inventory", "full_digest": full_digest})
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
        Command::GenerateControllerKey {
            private_key_file,
            public_key_file,
        } => generate_controller_key(private_key_file.as_deref(), public_key_file.as_deref())?,
        Command::ExportProfile {
            source,
            output,
            name,
        } => export_profile(&source, &output, &name)?,
        Command::RsyncSsh { arguments } => rsync_ssh(arguments)?,
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

fn rsync_ssh(arguments: Vec<OsString>) -> anyhow::Result<()> {
    if arguments.len() < 3 {
        bail!("rsync ssh 参数不足");
    }
    let command_offset = arguments
        .iter()
        .position(|value| value == "rsync")
        .context("rsync ssh 缺少远端 rsync 命令")?;
    let remote_parts = &arguments[..command_offset];
    let remote = if remote_parts.len() == 3 && remote_parts[0] == "-l" {
        format!(
            "{}@{}",
            remote_parts[1].to_string_lossy(),
            remote_parts[2].to_string_lossy()
        )
    } else if remote_parts.len() == 2 && !remote_parts[0].to_string_lossy().contains('@') {
        format!(
            "{}@{}",
            remote_parts[0].to_string_lossy(),
            remote_parts[1].to_string_lossy()
        )
    } else if remote_parts.len() == 1 {
        remote_parts[0].to_string_lossy().into_owned()
    } else {
        bail!("rsync ssh 远端参数形态无效：{arguments:?}");
    };
    if remote.is_empty()
        || !remote
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || "@._:-[]".contains(value))
    {
        bail!("rsync ssh 远端无效");
    }
    let remote_command = arguments[command_offset..]
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>();
    if remote_command.get(2).map(|value| value.as_ref()) != Some("--sender") {
        return rsync_receiver_ssh(arguments, command_offset, remote);
    }
    let option_cluster = remote_command.get(3).map(|value| value.as_ref());
    let path_offset = if remote_command.get(4).map(|value| value.as_ref()) == Some("--numeric-ids")
    {
        5
    } else {
        4
    };
    let transfer_path = remote_command
        .get(path_offset + 1)
        .map(|value| value.as_ref());
    let capability_id = transfer_path
        .and_then(|value| value.strip_prefix("/jobs/transfers/"))
        .and_then(|value| value.strip_suffix('/'));
    let allowed_cluster = matches!(
        option_cluster,
        Some("-logDtpre.iLsfxCIvu") | Some("-logDtpre.LsfxCIvu")
    );
    if remote_command.first().map(|value| value.as_ref()) != Some("rsync")
        || remote_command.get(1).map(|value| value.as_ref()) != Some("--server")
        || remote_command.get(2).map(|value| value.as_ref()) != Some("--sender")
        || !allowed_cluster
        || remote_command.get(path_offset).map(|value| value.as_ref()) != Some(".")
        || remote_command.len() != path_offset + 2
        || capability_id
            .and_then(|value| Uuid::parse_str(value).ok())
            .is_none()
    {
        bail!("rsync ssh 远端命令未被允许：{remote_command:?}");
    }
    let identity = env::var("AURSMITH_RSYNC_SSH_IDENTITY_FILE")?;
    let known_hosts = env::var("AURSMITH_RSYNC_SSH_KNOWN_HOSTS_FILE")?;
    let port = env::var("AURSMITH_RSYNC_SSH_PORT")?
        .parse::<u16>()
        .context("rsync SSH 端口无效")?;
    let error = ProcessCommand::new("/usr/bin/ssh")
        .arg("-T")
        .arg("-p")
        .arg(port.to_string())
        .arg("-i")
        .arg(identity)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg(format!("UserKnownHostsFile={known_hosts}"))
        .arg(&remote)
        .args(&arguments[command_offset..])
        .exec();
    Err(error).context("无法启动固定 rsync SSH")
}

fn rsync_receiver_ssh(
    arguments: Vec<OsString>,
    command_offset: usize,
    remote: String,
) -> anyhow::Result<()> {
    let remote_command = arguments[command_offset..]
        .iter()
        .map(|value| value.to_string_lossy())
        .collect::<Vec<_>>();
    let option_cluster = remote_command.get(2).map(|value| value.as_ref());
    let path_offset = if remote_command.get(3).map(|value| value.as_ref()) == Some("--numeric-ids")
    {
        4
    } else {
        3
    };
    let transfer_path = remote_command
        .get(path_offset + 1)
        .map(|value| value.as_ref());
    let capability_id = transfer_path
        .and_then(|value| value.strip_prefix("/landing/."))
        .and_then(|value| value.strip_suffix(".partial/"));
    let allowed_cluster = matches!(
        option_cluster,
        Some("-logDtpre.iLsfxCIvu") | Some("-logDtpre.LsfxCIvu")
    );
    if remote_command.first().map(|value| value.as_ref()) != Some("rsync")
        || remote_command.get(1).map(|value| value.as_ref()) != Some("--server")
        || !allowed_cluster
        || remote_command.get(path_offset).map(|value| value.as_ref()) != Some(".")
        || remote_command.len() != path_offset + 2
        || capability_id
            .and_then(|value| Uuid::parse_str(value).ok())
            .is_none()
    {
        bail!("rsync ssh receiver 命令未被允许：{remote_command:?}");
    }
    let identity = env::var("AURSMITH_RSYNC_SSH_IDENTITY_FILE")?;
    let known_hosts = env::var("AURSMITH_RSYNC_SSH_KNOWN_HOSTS_FILE")?;
    let port = env::var("AURSMITH_RSYNC_SSH_PORT")?
        .parse::<u16>()
        .context("rsync SSH 端口无效")?;
    let error = ProcessCommand::new("/usr/bin/ssh")
        .arg("-T")
        .arg("-p")
        .arg(port.to_string())
        .arg("-i")
        .arg(identity)
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-o")
        .arg("StrictHostKeyChecking=yes")
        .arg("-o")
        .arg(format!("UserKnownHostsFile={known_hosts}"))
        .arg(&remote)
        .args(&arguments[command_offset..])
        .exec();
    Err(error).context("无法启动固定 rsync receiver SSH")
}

fn export_profile(source: &Path, output: &Path, name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() || name.len() > 64 {
        bail!("Profile 名称长度必须为 1 至 64");
    }
    fs::create_dir_all(output)?;
    let mut entries = Vec::new();
    for file_name in ["root.qcow2", "vmlinuz-linux", "initramfs-linux.img"] {
        let source_file = source.join(file_name);
        let destination = output.join(file_name);
        if destination.exists() {
            bail!("拒绝覆盖已有 Profile 文件 {}", destination.display());
        }
        fs::copy(&source_file, &destination).with_context(|| format!("无法导出 {file_name}"))?;
        fs::set_permissions(&destination, fs::Permissions::from_mode(0o644))?;
        entries.push(profile_entry(&destination, file_name)?);
    }
    let packages: Vec<String> = fs::read_to_string(source.join("installed-packages.txt"))?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if packages.is_empty() {
        bail!("Profile 已安装包清单为空");
    }
    let created_at: DateTime<Utc> = fs::read_to_string(source.join("created-at"))?
        .trim()
        .parse()
        .context("Profile 创建时间无效")?;
    let mut spec = BuildProfileSpec {
        profile_sha256: String::new(),
        root_image: entries.remove(0),
        kernel: entries.remove(0),
        initramfs: entries.remove(0),
        installed_packages: packages,
        repository_mirror: fs::read_to_string(source.join("repository-mirror"))
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
        created_at,
    };
    spec.profile_sha256 = spec.content_sha256()?;
    let candidate = json!({"name": name, "spec": spec});
    let candidate_path = output.join("profile-candidate.json");
    if candidate_path.exists() {
        bail!("拒绝覆盖已有 Profile candidate");
    }
    fs::write(candidate_path, serde_json::to_vec_pretty(&candidate)?)?;
    Ok(())
}

fn profile_entry(path: &Path, name: &str) -> anyhow::Result<ManifestEntry> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(ManifestEntry {
        path: name.to_owned(),
        sha256: hex::encode(hasher.finalize()),
        size: fs::metadata(path)?.len(),
    })
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

#[cfg(test)]
mod tests {
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
    fn controller_key_can_be_written_without_printing_the_private_value() {
        let directory = tempfile::tempdir().unwrap();
        let private = directory.path().join("controller.key");
        let public = directory.path().join("controller.pub");
        generate_controller_key(Some(&private), Some(&public)).unwrap();
        let private_value = fs::read_to_string(&private).unwrap();
        let public_value = fs::read_to_string(&public).unwrap();
        assert_eq!(private_value.trim().len(), 64);
        assert_eq!(public_value.trim().len(), 64);
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(generate_controller_key(Some(&private), Some(&public)).is_err());
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
}

fn generate_controller_key(
    private_key_file: Option<&Path>,
    public_key_file: Option<&Path>,
) -> anyhow::Result<()> {
    let mut secret = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .context("无法打开系统随机源")?
        .read_exact(&mut secret)
        .context("无法从系统随机源读取密钥")?;
    let signing_key = SigningKey::from_bytes(&secret);
    let private_key_hex = hex::encode(secret);
    let public_key_hex = hex::encode(signing_key.verifying_key().to_bytes());
    match (private_key_file, public_key_file) {
        (Some(private_path), Some(public_path)) => {
            write_new_file(private_path, private_key_hex.as_bytes(), 0o600)?;
            write_new_file(public_path, public_key_hex.as_bytes(), 0o644)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "private_key_file": private_path,
                    "public_key_file": public_path,
                    "public_key_hex": public_key_hex,
                }))?
            );
        }
        (None, None) => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "private_key_hex": private_key_hex,
                "public_key_hex": public_key_hex,
            }))?
        ),
        _ => bail!("必须同时提供私钥与公钥输出文件"),
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .with_context(|| format!("无法创建 {}", path.display()))?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

async fn ssh_gateway(socket: &PathBuf) -> anyhow::Result<()> {
    let original = env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let parts: Vec<_> = original.split_ascii_whitespace().collect();
    if parts.first() == Some(&"rsync") {
        return rsync_gateway(socket, &parts).await;
    }
    let request = match parts.as_slice() {
        ["status"] => json!({"command": "status"}),
        ["drain"] => json!({"command": "drain"}),
        ["query", job_id] if uuid_like(job_id) => json!({"command": "query", "job_id": job_id}),
        ["submit"] => {
            let mut bytes = Vec::new();
            tokio::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .await
                .context("读取 JobSpec Envelope 失败")?;
            let envelope: Value =
                serde_json::from_slice(&bytes).context("JobSpec Envelope 不是有效 JSON")?;
            json!({"command": "submit", "envelope": envelope})
        }
        ["aur-search"] => read_limited_json_command("aur_search").await?,
        ["aur-info"] => read_limited_json_command("aur_info").await?,
        ["aur-providers"] => read_limited_json_command("aur_providers").await?,
        ["official-info"] => read_limited_json_command("official_info").await?,
        ["publisher-doctor"] => json!({"command": "publisher_doctor"}),
        ["aur-snapshot"] => read_limited_json_command("aur_snapshot").await?,
        ["authorize-export"]
        | ["authorize-import"]
        | ["prepare-push-import"]
        | ["finalize-push-import"]
        | ["authorize-release"]
        | ["authorize-rollback"]
        | ["complete-export"] => {
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
                    "authorize-export" => "authorize_export",
                    "authorize-import" => "authorize_import",
                    "prepare-push-import" => "prepare_push_import",
                    "finalize-push-import" => "finalize_push_import",
                    "complete-export" => "complete_export",
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
        ["inventory"] => json!({"command": "inventory", "full_digest": false}),
        ["inventory", "--full-digest"] => json!({"command": "inventory", "full_digest": true}),
        _ => bail!("SSH 命令未被允许"),
    };
    let response = worker_request(socket, request).await?;
    println!("{}", serde_json::to_string(&response)?);
    if !response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        bail!("Worker 返回失败");
    }
    Ok(())
}

async fn rsync_gateway(socket: &PathBuf, parts: &[&str]) -> anyhow::Result<()> {
    if !parts.contains(&"--sender") {
        let error = ProcessCommand::new("/usr/sbin/rrsync")
            .args(["-wo", "/landing"])
            .exec();
        return Err(error).context("无法启动官方 rrsync 收件器");
    }
    let valid_shape = matches!(
        parts,
        [
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            "--numeric-ids",
            ".",
            _
        ] | [
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.iLsfxCIvu",
            ".",
            _
        ] | [
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.LsfxCIvu",
            "--numeric-ids",
            ".",
            _
        ] | [
            "rsync",
            "--server",
            "--sender",
            "-logDtpre.LsfxCIvu",
            ".",
            _
        ]
    );
    if !valid_shape {
        bail!("rsync 参数未被允许");
    }
    let requested = parts.last().context("rsync 缺少导出路径")?;
    let normalized = requested.trim_end_matches('/');
    let prefix = "/jobs/transfers/";
    let capability_id = normalized
        .strip_prefix(prefix)
        .filter(|value| uuid_like(value))
        .context("rsync 导出路径未绑定 Capability")?;
    let response = worker_request(
        socket,
        json!({"command": "resolve_export", "capability_id": capability_id}),
    )
    .await?;
    if !response.get("ok").and_then(Value::as_bool).unwrap_or(false)
        || response["data"]["directory"].as_str() != Some(normalized)
    {
        bail!("Worker 未授权该 rsync 导出");
    }
    let error = ProcessCommand::new("/usr/bin/rsync")
        .args(&parts[1..])
        .exec();
    Err(error).context("无法启动受限 rsync sender")
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
        checks.push(json!({"check": "kvm", "ok": std::path::Path::new("/dev/kvm").exists()}));
        checks.push(json!({"check": "qemu-system-x86_64", "ok": command_works("/usr/bin/qemu-system-x86_64", "--version")}));
        checks.push(
            json!({"check": "qemu-img", "ok": command_works("/usr/bin/qemu-img", "--version")}),
        );
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
