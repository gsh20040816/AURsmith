mod aur;
mod builder;
mod package_inspection;

use anyhow::{Context, bail};
use aursmith_domain::{ArchiveState, JobStatus, WorkerRole, WorkerState};
use aursmith_protocol::{
    ArchiveInventory, ArchiveReceipt, BackupArchiveReceipt, ControlPlaneBackup, JobSpec,
    PROTOCOL_MAJOR, ReleaseAuthorization, ReleaseManifest, ReleaseRollbackAuthorization,
    SignedEnvelope, TransferCapability,
};
use chrono::Utc;
use clap::Parser;
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{
    Row, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "aursmith-worker", version)]
struct Cli {
    #[arg(long, env = "AURSMITH_WORKER_NAME")]
    name: String,
    #[arg(long, env = "AURSMITH_WORKER_ROLE")]
    role: RoleArg,
    #[arg(
        long,
        env = "AURSMITH_WORKER_SOCKET",
        default_value = "/run/aursmith/worker.sock"
    )]
    socket: String,
    #[arg(
        long,
        env = "AURSMITH_WORKER_DATABASE",
        default_value = "sqlite://runtime/worker.db"
    )]
    database: String,
    #[arg(long, env = "AURSMITH_CONTROLLER_VERIFYING_KEY_HEX")]
    controller_verifying_key_hex: String,
    #[arg(
        long,
        env = "AURSMITH_AUR_BASE_URL",
        default_value = "https://aur.archlinux.org/"
    )]
    aur_base_url: String,
    #[arg(long, env = "AURSMITH_SOURCE_PROXY_URL")]
    source_proxy_url: Option<String>,
    #[arg(long, env = "AURSMITH_PROFILES_DIR", default_value = "/profiles")]
    profiles_dir: String,
    #[arg(long, env = "AURSMITH_JOBS_DIR", default_value = "/jobs")]
    jobs_dir: String,
    #[arg(long, env = "AURSMITH_FETCH_PROXY")]
    fetch_proxy: Option<SocketAddr>,
    #[arg(long, env = "AURSMITH_TRANSFER_ENDPOINTS_JSON", default_value = "{}")]
    transfer_endpoints_json: String,
    #[arg(long, env = "AURSMITH_TRANSFER_SSH_IDENTITY_FILE")]
    transfer_ssh_identity_file: Option<PathBuf>,
    #[arg(long, env = "AURSMITH_TRANSFER_SSH_KNOWN_HOSTS_FILE")]
    transfer_ssh_known_hosts_file: Option<PathBuf>,
    #[arg(long, env = "AURSMITH_LANDING_DIR", default_value = "/landing")]
    landing_dir: PathBuf,
    #[arg(long, env = "AURSMITH_WRITER_EPOCH", default_value_t = 0)]
    writer_epoch: u64,
    #[arg(long, env = "AURSMITH_SIGNER_INBOX", default_value = "/signer-inbox")]
    signer_inbox: PathBuf,
    #[arg(long, env = "AURSMITH_SIGNER_OUTPUT", default_value = "/signer-output")]
    signer_output: PathBuf,
    #[arg(long, env = "AURSMITH_REPOSITORY_DIR", default_value = "/repository")]
    repository_dir: PathBuf,
    #[arg(long, env = "AURSMITH_REPOSITORY_ARCH", default_value = "x86_64")]
    repository_arch: String,
    #[arg(long, env = "AURSMITH_REPOSITORY_GPG_PUBLIC_KEY_FILE")]
    repository_gpg_public_key_file: Option<PathBuf>,
    #[arg(
        long,
        env = "AURSMITH_PUBLISHER_GPG_HOME",
        default_value = "/run/aursmith-gpg"
    )]
    publisher_gpg_home: PathBuf,
    #[arg(long, env = "AURSMITH_ARCHIVE_DIR", default_value = "/archive")]
    archive_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum RoleArg {
    Builder,
    Publisher,
    Archiver,
}

impl From<RoleArg> for WorkerRole {
    fn from(value: RoleArg) -> Self {
        match value {
            RoleArg::Builder => Self::Builder,
            RoleArg::Publisher => Self::Publisher,
            RoleArg::Archiver => Self::Archiver,
        }
    }
}

#[derive(Clone)]
struct Worker {
    name: String,
    role: WorkerRole,
    database: SqlitePool,
    trusted_controller_key: Vec<u8>,
    aur: aur::AurClient,
    source_proxy_url: Option<String>,
    builder: Option<builder::BuilderRuntime>,
    transfer_endpoints: BTreeMap<String, String>,
    transfer_ssh_identity_file: Option<PathBuf>,
    transfer_ssh_known_hosts_file: Option<PathBuf>,
    landing_dir: PathBuf,
    writer_epoch: u64,
    signer_inbox: PathBuf,
    signer_output: PathBuf,
    repository_dir: PathBuf,
    repository_arch: String,
    publisher_gpg_home: PathBuf,
    jobs_dir: PathBuf,
    archive_dir: PathBuf,
    identity_signing_key: SigningKey,
    repository_gpg_fingerprint: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum WorkerCommand {
    Status,
    Drain,
    Submit {
        envelope: SignedEnvelope,
    },
    Query {
        job_id: String,
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
        #[serde(default)]
        previous_vcs_commit: Option<String>,
    },
    AuthorizeExport {
        envelope: SignedEnvelope,
    },
    ResolveExport {
        capability_id: String,
    },
    CompleteExport {
        envelope: SignedEnvelope,
    },
    AuthorizeImport {
        envelope: SignedEnvelope,
    },
    AuthorizeRelease {
        envelope: SignedEnvelope,
    },
    QueryRelease {
        release_id: String,
    },
    ReleaseFiles {
        release_id: String,
    },
    AuthorizeRollback {
        envelope: SignedEnvelope,
    },
    Inventory {
        full_digest: bool,
    },
}

#[derive(Debug, Serialize)]
struct WorkerResponse {
    ok: bool,
    code: &'static str,
    message: String,
    data: serde_json::Value,
}

impl WorkerResponse {
    fn ok(code: &'static str, data: serde_json::Value) -> Self {
        Self {
            ok: true,
            code,
            message: String::new(),
            data,
        }
    }

    fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            ok: false,
            code,
            message: message.into(),
            data: serde_json::Value::Null,
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "aursmith=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();
    let cli = Cli::parse();
    let trusted_controller_key = hex::decode(&cli.controller_verifying_key_hex)
        .context("Controller verifying key 必须是十六进制")?;
    if trusted_controller_key.len() != 32 {
        bail!("Controller verifying key 必须是 32 字节 Ed25519 公钥");
    }
    let database = connect(&cli.database).await?;
    let identity_signing_key = load_or_create_identity_signing_key(&database).await?;
    let jobs_dir = PathBuf::from(&cli.jobs_dir);
    let transfer_endpoints: BTreeMap<String, String> =
        serde_json::from_str(&cli.transfer_endpoints_json)
            .context("AURSMITH_TRANSFER_ENDPOINTS_JSON 不是字符串映射")?;
    let aur = aur::AurClient::new(&cli.aur_base_url)?;
    let repository_public_key = cli.repository_gpg_public_key_file.clone();
    let repository_gpg_fingerprint = if matches!(cli.role, RoleArg::Publisher) {
        Some(initialize_publisher_gpg(
            &cli.publisher_gpg_home,
            repository_public_key
                .as_deref()
                .context("Publisher 必须配置仓库 GPG 公钥")?,
        )?)
    } else {
        None
    };
    let worker = Arc::new(Worker {
        name: cli.name,
        role: cli.role.into(),
        database,
        trusted_controller_key,
        aur,
        source_proxy_url: cli.source_proxy_url,
        builder: if matches!(cli.role, RoleArg::Builder) {
            Some(builder::BuilderRuntime::new(
                cli.profiles_dir.into(),
                jobs_dir.clone(),
                cli.fetch_proxy,
            ))
        } else {
            None
        },
        transfer_endpoints,
        transfer_ssh_identity_file: cli.transfer_ssh_identity_file,
        transfer_ssh_known_hosts_file: cli.transfer_ssh_known_hosts_file,
        landing_dir: cli.landing_dir,
        writer_epoch: cli.writer_epoch,
        signer_inbox: cli.signer_inbox,
        signer_output: cli.signer_output,
        repository_dir: cli.repository_dir,
        repository_arch: cli.repository_arch,
        publisher_gpg_home: cli.publisher_gpg_home,
        jobs_dir,
        archive_dir: cli.archive_dir,
        identity_signing_key,
        repository_gpg_fingerprint,
    });
    if worker.builder.is_some() {
        builder::spawn(
            worker.database.clone(),
            worker.trusted_controller_key.clone(),
            worker.builder.clone().expect("已检查 Builder runtime"),
        );
    }
    if worker.role == WorkerRole::Publisher {
        publish_repository_public_key(
            &worker,
            repository_public_key
                .as_deref()
                .context("Publisher 必须配置仓库 GPG 公钥")?,
        )?;
        spawn_publisher(worker.clone());
    }
    prepare_socket(&cli.socket).await?;
    let listener = UnixListener::bind(&cli.socket)
        .with_context(|| format!("无法监听 Unix Socket {}", cli.socket))?;
    tracing::info!(socket = %cli.socket, role = ?worker.role, "Worker daemon 已启动");
    loop {
        let (stream, _) = listener.accept().await?;
        let worker = worker.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(worker, stream).await {
                tracing::warn!(%error, "处理 Worker 控制连接失败");
            }
        });
    }
}

async fn connect(database_url: &str) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal);
    let pool = SqlitePoolOptions::new()
        .max_connections(3)
        .connect_with(options)
        .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS worker_state(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_exports(capability_id TEXT PRIMARY KEY, expires_at TEXT NOT NULL, directory TEXT NOT NULL, manifest_json TEXT NOT NULL, state TEXT NOT NULL);",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS transfer_imports(capability_id TEXT PRIMARY KEY, expires_at TEXT NOT NULL, directory TEXT NOT NULL, manifest_json TEXT NOT NULL, state TEXT NOT NULL);",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS publisher_releases(release_id TEXT PRIMARY KEY, writer_epoch INTEGER NOT NULL, envelope_sha256 TEXT NOT NULL, authorization_json TEXT NOT NULL, state TEXT NOT NULL, manifest_sha256 TEXT, last_error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS archive_receipts(release_id TEXT PRIMARY KEY, capability_id TEXT NOT NULL UNIQUE, envelope_json TEXT NOT NULL, directory TEXT NOT NULL, created_at TEXT NOT NULL);",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS backup_archive_receipts(backup_id TEXT PRIMARY KEY, capability_id TEXT NOT NULL UNIQUE, envelope_json TEXT NOT NULL, directory TEXT NOT NULL, created_at TEXT NOT NULL);",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS attempts(\
         job_id TEXT NOT NULL, attempt_id TEXT NOT NULL, generation INTEGER NOT NULL, \
         envelope_sha256 TEXT NOT NULL, status TEXT NOT NULL, received_at TEXT NOT NULL, \
         result_sha256 TEXT, spec_json TEXT, failure_code TEXT, PRIMARY KEY(job_id, generation), UNIQUE(attempt_id));",
    )
    .execute(&pool)
    .await?;
    for statement in [
        "ALTER TABLE attempts ADD COLUMN spec_json TEXT",
        "ALTER TABLE attempts ADD COLUMN failure_code TEXT",
    ] {
        if let Err(error) = sqlx::query(statement).execute(&pool).await
            && !error.to_string().contains("duplicate column name")
        {
            return Err(error.into());
        }
    }
    sqlx::query(
        "INSERT INTO worker_state(key, value) VALUES ('state', 'online') ON CONFLICT(key) DO NOTHING",
    )
    .execute(&pool)
    .await?;
    sqlx::query(
        "INSERT INTO worker_state(key, value) VALUES ('instance_id', ?) ON CONFLICT(key) DO NOTHING",
    )
    .bind(uuid::Uuid::new_v4().to_string())
    .execute(&pool)
    .await?;
    Ok(pool)
}

async fn load_or_create_identity_signing_key(database: &SqlitePool) -> anyhow::Result<SigningKey> {
    let existing: Option<String> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'identity_signing_key'")
            .fetch_optional(database)
            .await?;
    let secret = if let Some(value) = existing {
        let bytes = hex::decode(value)?;
        <[u8; 32]>::try_from(bytes.as_slice())
            .map_err(|_| anyhow::anyhow!("Worker 身份密钥长度无效"))?
    } else {
        use std::io::Read;
        let mut secret = [0_u8; 32];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut secret)?;
        sqlx::query("INSERT INTO worker_state(key, value) VALUES ('identity_signing_key', ?)")
            .bind(hex::encode(secret))
            .execute(database)
            .await?;
        secret
    };
    Ok(SigningKey::from_bytes(&secret))
}

async fn prepare_socket(socket: &str) -> anyhow::Result<()> {
    if let Some(parent) = Path::new(socket).parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    match tokio::fs::remove_file(socket).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

async fn handle_connection(worker: Arc<Worker>, stream: UnixStream) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut line = String::new();
    BufReader::new(reader).read_line(&mut line).await?;
    let response = match serde_json::from_str::<WorkerCommand>(&line) {
        Ok(command) => execute_command(&worker, command).await,
        Err(error) => WorkerResponse::error("INVALID_COMMAND", error.to_string()),
    };
    let mut bytes = serde_json::to_vec(&response)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.shutdown().await?;
    Ok(())
}

async fn execute_command(worker: &Worker, command: WorkerCommand) -> WorkerResponse {
    match command {
        WorkerCommand::Status => status(worker).await,
        WorkerCommand::Drain => drain(worker).await,
        WorkerCommand::Submit { envelope } => submit(worker, envelope).await,
        WorkerCommand::Query { job_id } => query(worker, &job_id).await,
        WorkerCommand::AurSearch { query } => aur_search(worker, &query).await,
        WorkerCommand::AurInfo { names } => aur_info(worker, &names).await,
        WorkerCommand::AurProviders { names } => aur_providers(worker, &names).await,
        WorkerCommand::OfficialInfo { names } => official_info(worker, &names).await,
        WorkerCommand::PublisherDoctor => publisher_doctor(worker).await,
        WorkerCommand::AurSnapshot {
            package_base,
            previous_vcs_commit,
        } => aur_snapshot(worker, &package_base, previous_vcs_commit.as_deref()).await,
        WorkerCommand::AuthorizeExport { envelope } => authorize_export(worker, envelope).await,
        WorkerCommand::ResolveExport { capability_id } => {
            resolve_export(worker, &capability_id).await
        }
        WorkerCommand::CompleteExport { envelope } => complete_export(worker, envelope).await,
        WorkerCommand::AuthorizeImport { envelope } => authorize_import(worker, envelope).await,
        WorkerCommand::AuthorizeRelease { envelope } => authorize_release(worker, envelope).await,
        WorkerCommand::QueryRelease { release_id } => query_release(worker, &release_id).await,
        WorkerCommand::ReleaseFiles { release_id } => release_files(worker, &release_id).await,
        WorkerCommand::AuthorizeRollback { envelope } => authorize_rollback(worker, envelope).await,
        WorkerCommand::Inventory { full_digest } => inventory(worker, full_digest).await,
    }
}

async fn authorize_export(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if !matches!(worker.role, WorkerRole::Builder | WorkerRole::Publisher) {
        return WorkerResponse::error("WRONG_ROLE", "当前 Worker 不能导出文件");
    }
    if envelope.verifying_key != worker.trusted_controller_key {
        return WorkerResponse::error("UNTRUSTED_CONTROLLER", "TransferCapability 签名无效");
    }
    let capability: TransferCapability = match envelope.verify("aursmith.transfer_capability") {
        Ok(value) => value,
        Err(error) => return WorkerResponse::error("INVALID_CAPABILITY", error.to_string()),
    };
    if capability.expires_at < Utc::now() || capability.files.is_empty() {
        return WorkerResponse::error("INVALID_CAPABILITY", "TransferCapability 已过期或为空");
    }
    let instance_id: Result<String, _> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await;
    let expected_source = capability.source_worker.to_string();
    if !matches!(instance_id.as_deref(), Ok(value) if value == expected_source) {
        return WorkerResponse::error("CAPABILITY_SOURCE_MISMATCH", "Capability 不属于本 Worker");
    }
    let (source, transfer_root) = if worker.role == WorkerRole::Builder {
        let Some(runtime) = &worker.builder else {
            return WorkerResponse::error("WRONG_ROLE", "Builder runtime 不可用");
        };
        let Some(attempt) = &capability.attempt else {
            return WorkerResponse::error("ATTEMPT_REQUIRED", "Artifact 导出必须绑定 Attempt");
        };
        if capability.release_id.is_some() || capability.backup_id.is_some() {
            return WorkerResponse::error(
                "INVALID_CAPABILITY",
                "Builder Capability 不能绑定 Release",
            );
        }
        (
            runtime
                .jobs_dir()
                .join("completed")
                .join(attempt.attempt_id.to_string())
                .join("output"),
            runtime.jobs_dir().join("transfers"),
        )
    } else {
        let Some(release_id) = capability.release_id else {
            return WorkerResponse::error("RELEASE_REQUIRED", "Publisher 导出必须绑定 Release");
        };
        if capability.attempt.is_some()
            || capability.backup_id.is_some()
            || capability.writer_epoch != worker.writer_epoch
        {
            return WorkerResponse::error(
                "INVALID_CAPABILITY",
                "Publisher Release Capability 的 Attempt 或 writer epoch 无效",
            );
        }
        (
            worker
                .repository_dir
                .join(&worker.repository_arch)
                .join("releases")
                .join(release_id.to_string()),
            worker.jobs_dir.join("transfers"),
        )
    };
    let directory = transfer_root.join(capability.id.to_string());
    if directory.exists() {
        let existing: Option<String> = sqlx::query_scalar(
            "SELECT manifest_json FROM transfer_exports WHERE capability_id = ?",
        )
        .bind(capability.id.to_string())
        .fetch_optional(&worker.database)
        .await
        .ok()
        .flatten();
        if existing.as_deref() == serde_json::to_string(&capability.files).ok().as_deref() {
            return WorkerResponse::ok(
                "IDEMPOTENT_EXPORT",
                serde_json::json!({"capability_id": capability.id}),
            );
        }
        return WorkerResponse::error("CAPABILITY_CONFLICT", "Capability ID 已被其他内容使用");
    }
    if let Err(error) = materialize_export(&source, &directory, &capability.files) {
        let _ = std::fs::remove_dir_all(&directory);
        return WorkerResponse::error("EXPORT_INVALID", error.to_string());
    }
    let result = sqlx::query("INSERT INTO transfer_exports(capability_id, expires_at, directory, manifest_json, state) VALUES (?, ?, ?, ?, 'ready')")
        .bind(capability.id.to_string()).bind(capability.expires_at).bind(directory.to_string_lossy().as_ref())
        .bind(serde_json::to_string(&capability.files).unwrap_or_default()).execute(&worker.database).await;
    match result {
        Ok(_) => WorkerResponse::ok(
            "EXPORT_READY",
            serde_json::json!({"capability_id": capability.id}),
        ),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

fn materialize_export(
    source: &Path,
    destination: &Path,
    files: &[aursmith_protocol::ManifestEntry],
) -> anyhow::Result<()> {
    if files.len() > 4096 {
        bail!("导出文件过多");
    }
    std::fs::create_dir_all(destination)?;
    for entry in files {
        aursmith_protocol::validate_relative_path(&entry.path)?;
        let source_file = source.join(&entry.path);
        let metadata = std::fs::symlink_metadata(&source_file)?;
        if !metadata.file_type().is_file()
            || metadata.len() != entry.size
            || file_sha256(&source_file)? != entry.sha256
        {
            bail!("导出文件与 Manifest 不匹配：{}", entry.path);
        }
        let target = destination.join(&entry.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source_file, target)?;
    }
    Ok(())
}

fn file_sha256(path: &Path) -> anyhow::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex::encode(hasher.finalize()))
}

async fn resolve_export(worker: &Worker, capability_id: &str) -> WorkerResponse {
    if uuid::Uuid::parse_str(capability_id).is_err() {
        return WorkerResponse::error("INVALID_CAPABILITY", "Capability ID 无效");
    }
    let row = sqlx::query("SELECT directory, expires_at FROM transfer_exports WHERE capability_id = ? AND state = 'ready'")
        .bind(capability_id).fetch_optional(&worker.database).await;
    match row {
        Ok(Some(row)) => {
            let expires_at: String = row.get("expires_at");
            if expires_at
                .parse::<chrono::DateTime<Utc>>()
                .is_ok_and(|value| value >= Utc::now())
            {
                WorkerResponse::ok(
                    "EXPORT_ALLOWED",
                    serde_json::json!({"directory": row.get::<String,_>("directory")}),
                )
            } else {
                WorkerResponse::error("CAPABILITY_EXPIRED", "Capability 已过期")
            }
        }
        Ok(None) => WorkerResponse::error("CAPABILITY_NOT_FOUND", "Capability 不存在"),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

async fn complete_export(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if envelope.verifying_key != worker.trusted_controller_key {
        return WorkerResponse::error("UNTRUSTED_CONTROLLER", "TransferCapability 签名无效");
    }
    let capability: TransferCapability = match envelope.verify("aursmith.transfer_capability") {
        Ok(value) => value,
        Err(error) => return WorkerResponse::error("INVALID_CAPABILITY", error.to_string()),
    };
    let instance_id: Result<String, _> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await;
    if !matches!(instance_id.as_deref(), Ok(value) if value == capability.source_worker.to_string())
    {
        return WorkerResponse::error("CAPABILITY_SOURCE_MISMATCH", "Capability 不属于本 Worker");
    }
    let row = sqlx::query(
        "SELECT directory, manifest_json FROM transfer_exports WHERE capability_id = ?",
    )
    .bind(capability.id.to_string())
    .fetch_optional(&worker.database)
    .await;
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => {
            return WorkerResponse::ok(
                "IDEMPOTENT_EXPORT_CLEANUP",
                serde_json::json!({"capability_id": capability.id}),
            );
        }
        Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    };
    if row.get::<String, _>("manifest_json")
        != serde_json::to_string(&capability.files).unwrap_or_default()
    {
        return WorkerResponse::error("CAPABILITY_CONFLICT", "导出 Manifest 与授权不一致");
    }
    let directory = PathBuf::from(row.get::<String, _>("directory"));
    if directory.exists()
        && let Err(error) = std::fs::remove_dir_all(&directory)
    {
        return WorkerResponse::error("EXPORT_CLEANUP_FAILED", error.to_string());
    }
    match sqlx::query("UPDATE transfer_exports SET state = 'completed' WHERE capability_id = ?")
        .bind(capability.id.to_string())
        .execute(&worker.database)
        .await
    {
        Ok(_) => WorkerResponse::ok(
            "EXPORT_CLEANED",
            serde_json::json!({"capability_id": capability.id}),
        ),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

async fn authorize_import(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if !matches!(worker.role, WorkerRole::Publisher | WorkerRole::Archiver) {
        return WorkerResponse::error("WRONG_ROLE", "当前 Worker 不能接收传输");
    }
    if envelope.verifying_key != worker.trusted_controller_key {
        return WorkerResponse::error("UNTRUSTED_CONTROLLER", "TransferCapability 签名无效");
    }
    let capability: TransferCapability = match envelope.verify("aursmith.transfer_capability") {
        Ok(value) => value,
        Err(error) => return WorkerResponse::error("INVALID_CAPABILITY", error.to_string()),
    };
    if capability.expires_at < Utc::now() || capability.files.is_empty() {
        return WorkerResponse::error("INVALID_CAPABILITY", "TransferCapability 已过期或为空");
    }
    if worker.role == WorkerRole::Publisher && capability.writer_epoch != worker.writer_epoch {
        return WorkerResponse::error(
            "WRITER_EPOCH_MISMATCH",
            "TransferCapability 不属于当前 Publisher writer epoch",
        );
    }
    if (worker.role == WorkerRole::Publisher
        && (capability.attempt.is_none()
            || capability.release_id.is_some()
            || capability.backup_id.is_some()))
        || (worker.role == WorkerRole::Archiver
            && (capability.attempt.is_some()
                || capability.release_id.is_some() == capability.backup_id.is_some()))
    {
        return WorkerResponse::error("INVALID_CAPABILITY", "Capability 聚合类型与目标角色不匹配");
    }
    if worker.role == WorkerRole::Archiver {
        let (table, aggregate_id) = if let Some(release_id) = capability.release_id {
            ("archive_receipts", release_id.to_string())
        } else {
            (
                "backup_archive_receipts",
                capability.backup_id.unwrap_or_default().to_string(),
            )
        };
        let query = format!(
            "SELECT envelope_json FROM {table} WHERE {} = ?",
            if capability.release_id.is_some() {
                "release_id"
            } else {
                "backup_id"
            }
        );
        let existing: Option<String> = sqlx::query_scalar(&query)
            .bind(aggregate_id)
            .fetch_optional(&worker.database)
            .await
            .ok()
            .flatten();
        if let Some(receipt) =
            existing.and_then(|value| serde_json::from_str::<SignedEnvelope>(&value).ok())
        {
            return WorkerResponse::ok(
                "IDEMPOTENT_ARCHIVE",
                serde_json::json!({"capability_id": capability.id, "receipt": receipt}),
            );
        }
    }
    let instance_id: Result<String, _> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await;
    if !matches!(instance_id.as_deref(), Ok(value) if value == capability.destination_worker.to_string())
    {
        return WorkerResponse::error(
            "CAPABILITY_DESTINATION_MISMATCH",
            "Capability 不属于本 Publisher",
        );
    }
    let Some(endpoint) = worker
        .transfer_endpoints
        .get(&capability.source_worker.to_string())
    else {
        return WorkerResponse::error("SOURCE_ENDPOINT_MISSING", "没有配置 Builder 静态端点");
    };
    let Some(identity) = &worker.transfer_ssh_identity_file else {
        return WorkerResponse::error("TRANSFER_SSH_MISSING", "没有配置 Builder 拉取私钥");
    };
    let Some(known_hosts) = &worker.transfer_ssh_known_hosts_file else {
        return WorkerResponse::error("TRANSFER_SSH_MISSING", "没有配置 Builder known_hosts");
    };
    let endpoint = match url::Url::parse(endpoint) {
        Ok(value)
            if value.scheme() == "ssh"
                && value.password().is_none()
                && value.query().is_none()
                && value.fragment().is_none()
                && (value.path().is_empty() || value.path() == "/") =>
        {
            value
        }
        _ => return WorkerResponse::error("INVALID_SOURCE_ENDPOINT", "Builder SSH 端点无效"),
    };
    let host = match endpoint.host_str() {
        Some(value) => value,
        None => return WorkerResponse::error("INVALID_SOURCE_ENDPOINT", "Builder 端点缺少主机"),
    };
    let user = if endpoint.username().is_empty() {
        "aursmith"
    } else {
        endpoint.username()
    };
    if !identity.is_file() || !known_hosts.is_file() {
        return WorkerResponse::error("TRANSFER_SSH_MISSING", "传输 SSH 文件不存在");
    }
    let final_directory = worker.landing_dir.join(capability.id.to_string());
    if final_directory.exists() {
        if worker.role == WorkerRole::Archiver {
            return archived_receipt_response(worker, &capability).await;
        }
        return match verify_manifest_directory(&final_directory, &capability.files) {
            Ok(()) => WorkerResponse::ok(
                "IDEMPOTENT_IMPORT",
                serde_json::json!({"capability_id": capability.id}),
            ),
            Err(error) => WorkerResponse::error("CAPABILITY_CONFLICT", error.to_string()),
        };
    }
    let staging = worker
        .landing_dir
        .join(format!(".{}.partial", capability.id));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    if let Err(error) = std::fs::create_dir_all(&staging) {
        return WorkerResponse::error("LANDING_ERROR", error.to_string());
    }
    let remote = if host.contains(':') {
        format!("{user}@[{}]", host)
    } else {
        format!("{user}@{host}")
    };
    let source = format!("{remote}:/jobs/transfers/{}/", capability.id);
    let control_tool = match std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("aursmithctl")))
        .filter(|path| path.is_file())
    {
        Some(path) => path,
        None => {
            return WorkerResponse::error("TRANSFER_TOOL_MISSING", "Worker 同目录缺少 aursmithctl");
        }
    };
    let output = tokio::process::Command::new("/usr/bin/rsync")
        .args(["-a", "--numeric-ids", "--partial", "--delay-updates", "-e"])
        .arg(format!("{} rsync-ssh", control_tool.display()))
        .arg(source)
        .arg(format!("{}/", staging.display()))
        .env("AURSMITH_RSYNC_SSH_IDENTITY_FILE", identity)
        .env("AURSMITH_RSYNC_SSH_KNOWN_HOSTS_FILE", known_hosts)
        .env(
            "AURSMITH_RSYNC_SSH_PORT",
            endpoint.port().unwrap_or(22).to_string(),
        )
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    match output {
        Ok(value) if value.status.success() => {}
        Ok(value) => {
            let _ = std::fs::remove_dir_all(&staging);
            return WorkerResponse::error(
                "RSYNC_FAILED",
                String::from_utf8_lossy(&value.stderr)
                    .chars()
                    .take(512)
                    .collect::<String>(),
            );
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return WorkerResponse::error("RSYNC_FAILED", error.to_string());
        }
    }
    if let Err(error) = verify_manifest_directory(&staging, &capability.files) {
        let _ = std::fs::remove_dir_all(&staging);
        return WorkerResponse::error("TRANSFER_DIGEST_MISMATCH", error.to_string());
    }
    if let Err(error) = std::fs::rename(&staging, &final_directory) {
        let _ = std::fs::remove_dir_all(&staging);
        return WorkerResponse::error("LANDING_ERROR", error.to_string());
    }
    let inserted = sqlx::query("INSERT INTO transfer_imports(capability_id, expires_at, directory, manifest_json, state) VALUES (?, ?, ?, ?, 'verified')")
        .bind(capability.id.to_string()).bind(capability.expires_at)
        .bind(final_directory.to_string_lossy().as_ref()).bind(serde_json::to_string(&capability.files).unwrap_or_default())
        .execute(&worker.database).await;
    match inserted {
        Ok(_) if worker.role == WorkerRole::Archiver => {
            let archived = if capability.backup_id.is_some() {
                archive_control_plane_backup(worker, &capability, &final_directory).await
            } else {
                archive_release(worker, &capability, &final_directory).await
            };
            match archived {
                Ok(receipt) => WorkerResponse::ok(
                    "ARCHIVE_VERIFIED",
                    serde_json::json!({"capability_id": capability.id, "receipt": receipt}),
                ),
                Err(error) => WorkerResponse::error("ARCHIVE_FAILED", error.to_string()),
            }
        }
        Ok(_) => WorkerResponse::ok(
            "IMPORT_VERIFIED",
            serde_json::json!({"capability_id": capability.id, "files": capability.files.len()}),
        ),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

async fn archived_receipt_response(
    worker: &Worker,
    capability: &TransferCapability,
) -> WorkerResponse {
    let imported = worker.landing_dir.join(capability.id.to_string());
    let archived = if capability.backup_id.is_some() {
        archive_control_plane_backup(worker, capability, &imported).await
    } else {
        archive_release(worker, capability, &imported).await
    };
    match archived {
        Ok(receipt) => WorkerResponse::ok(
            "ARCHIVE_VERIFIED",
            serde_json::json!({"capability_id": capability.id, "receipt": receipt}),
        ),
        Err(error) => WorkerResponse::error("ARCHIVE_FAILED", error.to_string()),
    }
}

async fn archive_control_plane_backup(
    worker: &Worker,
    capability: &TransferCapability,
    imported: &Path,
) -> anyhow::Result<SignedEnvelope> {
    let backup_id =
        validate_control_plane_backup_input(&worker.trusted_controller_key, capability, imported)?;
    let root = worker.archive_dir.join("control-plane-backups");
    std::fs::create_dir_all(&root)?;
    let committed = root.join(backup_id.to_string());
    if !committed.exists() {
        let staging = root.join(format!(".{backup_id}.staging"));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        materialize_export(imported, &staging, &capability.files)?;
        verify_manifest_directory(&staging, &capability.files)?;
        sync_directory(&staging)?;
        std::fs::rename(&staging, &committed)?;
        sync_directory(&root)?;
    } else {
        verify_manifest_directory(&committed, &capability.files)?;
    }
    let instance_id: String =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await?;
    let receipt = BackupArchiveReceipt {
        backup_id,
        archive_worker: uuid::Uuid::parse_str(&instance_id)?,
        files: directory_manifest(&committed)?,
        verified_at: Utc::now(),
    };
    let envelope = SignedEnvelope::sign(
        "aursmith.backup_archive_receipt",
        &receipt,
        &worker.identity_signing_key,
    )?;
    sqlx::query("INSERT INTO backup_archive_receipts(backup_id, capability_id, envelope_json, directory, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(backup_id) DO NOTHING")
        .bind(backup_id.to_string()).bind(capability.id.to_string()).bind(serde_json::to_string(&envelope)?)
        .bind(committed.to_string_lossy().as_ref()).bind(Utc::now()).execute(&worker.database).await?;
    let _ = std::fs::remove_dir_all(imported);
    Ok(envelope)
}

fn validate_control_plane_backup_input(
    trusted_controller_key: &[u8],
    capability: &TransferCapability,
    imported: &Path,
) -> anyhow::Result<uuid::Uuid> {
    let backup_id = capability
        .backup_id
        .context("备份 Capability 缺少 Backup ID")?;
    verify_manifest_directory(imported, &capability.files)?;
    let backup_envelope: SignedEnvelope =
        serde_json::from_slice(&std::fs::read(imported.join("backup-envelope.json"))?)?;
    if backup_envelope.verifying_key != trusted_controller_key {
        bail!("控制面备份不是由当前 Controller 签署");
    }
    let backup: ControlPlaneBackup = backup_envelope.verify("aursmith.control_plane_backup")?;
    if backup.backup_id != backup_id {
        bail!("控制面备份 ID 与 Capability 不一致");
    }
    let database = capability
        .files
        .iter()
        .find(|entry| entry.path == backup.database.path)
        .context("Capability 缺少控制面数据库")?;
    if database != &backup.database {
        bail!("控制面数据库与签名 Manifest 不一致");
    }
    Ok(backup_id)
}

async fn archive_release(
    worker: &Worker,
    capability: &TransferCapability,
    imported: &Path,
) -> anyhow::Result<SignedEnvelope> {
    let release_id = capability
        .release_id
        .context("归档 Capability 缺少 Release ID")?;
    verify_manifest_directory(imported, &capability.files)?;
    let releases = worker.archive_dir.join("releases");
    std::fs::create_dir_all(&releases)?;
    let committed = releases.join(release_id.to_string());
    if !committed.exists() {
        let staging = releases.join(format!(".{release_id}.staging"));
        if staging.exists() {
            std::fs::remove_dir_all(&staging)?;
        }
        std::fs::create_dir_all(&staging)?;
        let previous = std::fs::read_dir(&releases)?
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && !entry.file_name().to_string_lossy().starts_with('.')
                    && entry.file_name().to_string_lossy() != release_id.to_string()
            })
            .max_by_key(|entry| entry.file_name());
        let mut command = tokio::process::Command::new("/usr/bin/rsync");
        command.args(["-a", "--numeric-ids"]);
        if let Some(previous) = previous {
            command.arg(format!("--link-dest={}", previous.path().display()));
        }
        let output = command
            .arg(format!("{}/", imported.display()))
            .arg(format!("{}/", staging.display()))
            .stdin(std::process::Stdio::null())
            .output()
            .await?;
        if !output.status.success() {
            bail!(
                "归档 rsync 失败：{}",
                String::from_utf8_lossy(&output.stderr)
                    .chars()
                    .take(512)
                    .collect::<String>()
            );
        }
        verify_manifest_directory(&staging, &capability.files)?;
        sync_directory(&staging)?;
        std::fs::rename(&staging, &committed)?;
        sync_directory(&releases)?;
    } else {
        verify_manifest_directory(&committed, &capability.files)?;
    }
    let files = directory_manifest(&committed)?;
    let release_manifest = files
        .iter()
        .find(|entry| entry.path == "release-manifest.json")
        .context("归档 Release 缺少 Manifest")?;
    let instance_id: String =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await?;
    let receipt = ArchiveReceipt {
        release_id,
        archive_worker: uuid::Uuid::parse_str(&instance_id)?,
        release_manifest_sha256: release_manifest.sha256.clone(),
        files,
        state: ArchiveState::Verified,
        verified_at: Utc::now(),
    };
    let envelope = SignedEnvelope::sign(
        "aursmith.archive_receipt",
        &receipt,
        &worker.identity_signing_key,
    )?;
    sqlx::query("INSERT INTO archive_receipts(release_id, capability_id, envelope_json, directory, created_at) VALUES (?, ?, ?, ?, ?) ON CONFLICT(release_id) DO NOTHING")
        .bind(release_id.to_string()).bind(capability.id.to_string())
        .bind(serde_json::to_string(&envelope)?).bind(committed.to_string_lossy().as_ref())
        .bind(Utc::now()).execute(&worker.database).await?;
    let _ = std::fs::remove_dir_all(imported);
    Ok(envelope)
}

async fn inventory(worker: &Worker, full_digest: bool) -> WorkerResponse {
    if worker.role != WorkerRole::Archiver {
        return WorkerResponse::error("WRONG_ROLE", "只有 Archiver 可以执行库存巡检");
    }
    let rows = match sqlx::query(
        "SELECT envelope_json, directory FROM archive_receipts ORDER BY release_id",
    )
    .fetch_all(&worker.database)
    .await
    {
        Ok(rows) => rows,
        Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    };
    let instance_id: String =
        match sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await
        {
            Ok(value) => value,
            Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
        };
    let mut release_count = 0_u64;
    let mut backup_count = 0_u64;
    let mut file_count = 0_u64;
    let mut byte_count = 0_u64;
    let mut failures = Vec::new();
    for row in rows {
        let envelope = serde_json::from_str::<SignedEnvelope>(row.get("envelope_json"));
        let receipt = envelope.and_then(|value| {
            if value.verifying_key != worker.identity_signing_key.verifying_key().as_bytes() {
                return Err(serde_json::Error::io(std::io::Error::other(
                    "Receipt 身份公钥不匹配",
                )));
            }
            value
                .verify::<ArchiveReceipt>("aursmith.archive_receipt")
                .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
        });
        let receipt = match receipt {
            Ok(value) => value,
            Err(error) => {
                failures.push(format!("Receipt 无效：{error}"));
                continue;
            }
        };
        release_count += 1;
        file_count = file_count.saturating_add(receipt.files.len() as u64);
        byte_count =
            byte_count.saturating_add(receipt.files.iter().map(|entry| entry.size).sum::<u64>());
        let directory = PathBuf::from(row.get::<String, _>("directory"));
        let result = if full_digest {
            verify_manifest_directory(&directory, &receipt.files)
        } else {
            verify_manifest_directory_shallow(&directory, &receipt.files)
        };
        if let Err(error) = result {
            failures.push(format!("Release {}：{error}", receipt.release_id));
        }
        if failures.len() >= 100 {
            break;
        }
    }
    if failures.len() < 100 {
        let backup_rows = match sqlx::query(
            "SELECT envelope_json, directory FROM backup_archive_receipts ORDER BY backup_id",
        )
        .fetch_all(&worker.database)
        .await
        {
            Ok(rows) => rows,
            Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
        };
        for row in backup_rows {
            let envelope = serde_json::from_str::<SignedEnvelope>(row.get("envelope_json"));
            let receipt = envelope.and_then(|value| {
                if value.verifying_key != worker.identity_signing_key.verifying_key().as_bytes() {
                    return Err(serde_json::Error::io(std::io::Error::other(
                        "备份 Receipt 身份公钥不匹配",
                    )));
                }
                value
                    .verify::<BackupArchiveReceipt>("aursmith.backup_archive_receipt")
                    .map_err(|error| serde_json::Error::io(std::io::Error::other(error)))
            });
            let receipt = match receipt {
                Ok(value) => value,
                Err(error) => {
                    failures.push(format!("备份 Receipt 无效：{error}"));
                    continue;
                }
            };
            backup_count += 1;
            file_count = file_count.saturating_add(receipt.files.len() as u64);
            byte_count = byte_count
                .saturating_add(receipt.files.iter().map(|entry| entry.size).sum::<u64>());
            let directory = PathBuf::from(row.get::<String, _>("directory"));
            let result = if full_digest {
                verify_manifest_directory(&directory, &receipt.files)
            } else {
                verify_manifest_directory_shallow(&directory, &receipt.files)
            };
            if let Err(error) = result {
                failures.push(format!("控制面备份 {}：{error}", receipt.backup_id));
            }
            if failures.len() >= 100 {
                break;
            }
        }
    }
    let report = ArchiveInventory {
        archive_worker: match uuid::Uuid::parse_str(&instance_id) {
            Ok(value) => value,
            Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
        },
        full_digest,
        release_count,
        backup_count,
        file_count,
        byte_count,
        failures,
        checked_at: Utc::now(),
    };
    match SignedEnvelope::sign(
        "aursmith.archive_inventory",
        &report,
        &worker.identity_signing_key,
    ) {
        Ok(envelope) => {
            WorkerResponse::ok("ARCHIVE_INVENTORY", serde_json::json!({"report": envelope}))
        }
        Err(error) => WorkerResponse::error("INVENTORY_ERROR", error.to_string()),
    }
}

async fn authorize_release(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以提交 Release");
    }
    if envelope.verifying_key != worker.trusted_controller_key {
        return WorkerResponse::error("UNTRUSTED_CONTROLLER", "ReleaseAuthorization 签名无效");
    }
    let authorization: ReleaseAuthorization =
        match envelope.verify("aursmith.release_authorization") {
            Ok(value) => value,
            Err(error) => return WorkerResponse::error("INVALID_RELEASE", error.to_string()),
        };
    if authorization.writer_epoch != worker.writer_epoch
        || authorization.expires_at < Utc::now()
        || (authorization.artifacts.is_empty() && authorization.removed_package_names.is_empty())
    {
        return WorkerResponse::error(
            "INVALID_RELEASE",
            "ReleaseAuthorization 已过期、为空或 writer epoch 不匹配",
        );
    }
    if let Err(error) = validate_release_authorization_for_publisher(&authorization) {
        return WorkerResponse::error("INVALID_RELEASE", error.to_string());
    }
    let release_id = authorization.release_id.to_string();
    let existing =
        sqlx::query("SELECT envelope_sha256, state FROM publisher_releases WHERE release_id = ?")
            .bind(&release_id)
            .fetch_optional(&worker.database)
            .await;
    match existing {
        Ok(Some(row)) if row.get::<String, _>("envelope_sha256") == envelope.payload_sha256 => {
            return WorkerResponse::ok(
                "IDEMPOTENT_RELEASE",
                serde_json::json!({"release_id": release_id, "state": row.get::<String,_>("state")}),
            );
        }
        Ok(Some(_)) => {
            return WorkerResponse::error("RELEASE_CONFLICT", "同一 Release ID 已绑定其他授权");
        }
        Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
        Ok(None) => {}
    }
    let staging = worker.signer_inbox.join(format!(".{release_id}.staging"));
    let committed = worker.signer_inbox.join(&release_id);
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    if committed.exists() {
        let recovered: Result<SignedEnvelope, _> =
            std::fs::read(committed.join("authorization.json"))
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(std::io::Error::other));
        if !matches!(recovered.as_ref(), Ok(value) if value.payload_sha256 == envelope.payload_sha256)
        {
            return WorkerResponse::error("RELEASE_CONFLICT", "Signer inbox 已存在其他授权");
        }
        let now = Utc::now();
        let inserted = sqlx::query("INSERT INTO publisher_releases(release_id, writer_epoch, envelope_sha256, authorization_json, state, created_at, updated_at) VALUES (?, ?, ?, ?, 'awaiting_signer', ?, ?)")
            .bind(&release_id).bind(i64::try_from(authorization.writer_epoch).unwrap_or(i64::MAX))
            .bind(&envelope.payload_sha256).bind(serde_json::to_string(&envelope).unwrap_or_default())
            .bind(now).bind(now).execute(&worker.database).await;
        return match inserted {
            Ok(_) => WorkerResponse::ok(
                "RELEASE_RECOVERED",
                serde_json::json!({"release_id": release_id}),
            ),
            Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
        };
    }
    if let Err(error) = materialize_release_inbox(worker, &authorization, &envelope, &staging).await
    {
        let _ = std::fs::remove_dir_all(&staging);
        return WorkerResponse::error("RELEASE_INPUT_INVALID", error.to_string());
    }
    if let Err(error) = std::fs::rename(&staging, &committed) {
        let _ = std::fs::remove_dir_all(&staging);
        return WorkerResponse::error("SIGNER_INBOX_ERROR", error.to_string());
    }
    let now = Utc::now();
    let inserted = sqlx::query("INSERT INTO publisher_releases(release_id, writer_epoch, envelope_sha256, authorization_json, state, created_at, updated_at) VALUES (?, ?, ?, ?, 'awaiting_signer', ?, ?)")
        .bind(&release_id).bind(i64::try_from(authorization.writer_epoch).unwrap_or(i64::MAX))
        .bind(&envelope.payload_sha256).bind(serde_json::to_string(&envelope).unwrap_or_default())
        .bind(now).bind(now).execute(&worker.database).await;
    match inserted {
        Ok(_) => WorkerResponse::ok(
            "RELEASE_QUEUED",
            serde_json::json!({"release_id": release_id}),
        ),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

fn validate_release_authorization_for_publisher(
    authorization: &ReleaseAuthorization,
) -> anyhow::Result<()> {
    if authorization.artifacts.is_empty() && authorization.removed_package_names.is_empty() {
        bail!("Release 没有软件包或清除目标");
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut package_names = std::collections::BTreeSet::new();
    for artifact in &authorization.artifacts {
        aursmith_protocol::validate_relative_path(&artifact.path)?;
        let file_name = Path::new(&artifact.path)
            .file_name()
            .context("Artifact 缺少文件名")?
            .to_string_lossy();
        if file_name != artifact.path
            || !paths.insert(artifact.path.clone())
            || !artifact.path.contains(".pkg.tar.")
            || artifact.sha256.len() != 64
            || artifact.size == 0
            || artifact.package_name.is_none()
            || artifact.package_version.is_none()
            || artifact.architecture.is_none()
        {
            bail!("Release Artifact 元数据无效：{}", artifact.path);
        }
        package_names.insert(artifact.package_name.clone().unwrap_or_default());
    }
    let mut removed = std::collections::BTreeSet::new();
    for package_name in &authorization.removed_package_names {
        if package_name.is_empty()
            || !package_name
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || "@._+-".contains(value))
            || !removed.insert(package_name)
            || package_names.contains(package_name)
        {
            bail!("Release 清除目标无效：{package_name}");
        }
    }
    Ok(())
}

async fn materialize_release_inbox(
    worker: &Worker,
    authorization: &ReleaseAuthorization,
    envelope: &SignedEnvelope,
    staging: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(staging)?;
    let mut inspections = Vec::new();
    let imports = sqlx::query(
        "SELECT directory, manifest_json FROM transfer_imports WHERE state = 'verified'",
    )
    .fetch_all(&worker.database)
    .await?;
    let mut used_paths = std::collections::BTreeSet::new();
    for artifact in &authorization.artifacts {
        aursmith_protocol::validate_relative_path(&artifact.path)?;
        if !used_paths.insert(artifact.path.clone()) {
            bail!("Release 包含重复 Artifact 路径：{}", artifact.path);
        }
        let mut source = None;
        for row in &imports {
            let manifest: Vec<aursmith_protocol::ManifestEntry> =
                serde_json::from_str(row.get("manifest_json"))?;
            if let Some(entry) = manifest.iter().find(|entry| {
                entry.sha256 == artifact.sha256
                    && entry.size == artifact.size
                    && entry.path == artifact.path
            }) {
                source = Some(PathBuf::from(row.get::<String, _>("directory")).join(&entry.path));
                break;
            }
        }
        if source.is_none() {
            let hot = worker
                .repository_dir
                .join(&worker.repository_arch)
                .join(&artifact.path);
            if hot.is_file() {
                source = Some(hot);
            }
        }
        let source = source.context("Release Artifact 没有已验证的 TransferCapability")?;
        let metadata = std::fs::symlink_metadata(&source)?;
        if !metadata.file_type().is_file()
            || metadata.len() != artifact.size
            || file_sha256(&source)? != artifact.sha256
        {
            bail!("Release Artifact 落地内容不匹配：{}", artifact.path);
        }
        let target = staging.join(&artifact.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, &target)?;
        inspections.push(package_inspection::inspect_package(&target, artifact)?);
    }
    std::fs::write(
        staging.join("artifact-inspections.json"),
        serde_json::to_vec_pretty(&inspections)?,
    )?;
    std::fs::write(
        staging.join("authorization.json"),
        serde_json::to_vec(envelope)?,
    )?;
    Ok(())
}

async fn query_release(worker: &Worker, release_id: &str) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher || uuid::Uuid::parse_str(release_id).is_err() {
        return WorkerResponse::error("INVALID_RELEASE", "Release ID 或 Worker 角色无效");
    }
    match sqlx::query("SELECT state, manifest_sha256, last_error, updated_at FROM publisher_releases WHERE release_id = ?")
        .bind(release_id).fetch_optional(&worker.database).await
    {
        Ok(Some(row)) => WorkerResponse::ok("RELEASE_STATUS", serde_json::json!({
            "release_id": release_id,
            "state": row.get::<String,_>("state"),
            "manifest_sha256": row.get::<Option<String>,_>("manifest_sha256"),
            "last_error": row.get::<Option<String>,_>("last_error"),
            "updated_at": row.get::<String,_>("updated_at"),
        })),
        Ok(None) => WorkerResponse::error("RELEASE_NOT_FOUND", "Release 不存在"),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

async fn authorize_rollback(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher
        || envelope.verifying_key != worker.trusted_controller_key
    {
        return WorkerResponse::error("UNTRUSTED_ROLLBACK", "回滚授权角色或签名无效");
    }
    let authorization: ReleaseRollbackAuthorization =
        match envelope.verify("aursmith.release_rollback_authorization") {
            Ok(value) => value,
            Err(error) => return WorkerResponse::error("INVALID_ROLLBACK", error.to_string()),
        };
    if authorization.expires_at < Utc::now() || authorization.writer_epoch != worker.writer_epoch {
        return WorkerResponse::error("INVALID_ROLLBACK", "回滚授权已过期或 writer epoch 不匹配");
    }
    let committed = worker
        .repository_dir
        .join(&worker.repository_arch)
        .join("releases")
        .join(authorization.release_id.to_string());
    match activate_committed_release(worker, &committed) {
        Ok(manifest) if manifest.release_id == authorization.release_id => WorkerResponse::ok(
            "RELEASE_ROLLED_BACK",
            serde_json::json!({
                "release_id": manifest.release_id,
                "manifest_sha256": file_sha256(&committed.join("release-manifest.json")).unwrap_or_default(),
                "artifacts": manifest.artifacts,
            }),
        ),
        Ok(_) => WorkerResponse::error("ROLLBACK_MISMATCH", "Release 目录与授权 ID 不匹配"),
        Err(error) => WorkerResponse::error("ROLLBACK_FAILED", error.to_string()),
    }
}

async fn release_files(worker: &Worker, release_id: &str) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher || uuid::Uuid::parse_str(release_id).is_err() {
        return WorkerResponse::error("INVALID_RELEASE", "Release ID 或 Worker 角色无效");
    }
    let directory = worker
        .repository_dir
        .join(&worker.repository_arch)
        .join("releases")
        .join(release_id);
    let manifest = directory.join("release-manifest.json");
    if !manifest.is_file() {
        return WorkerResponse::error("RELEASE_NOT_FOUND", "已提交 Release 不存在");
    }
    match directory_manifest(&directory) {
        Ok(files) if files.len() <= 8192 => WorkerResponse::ok(
            "RELEASE_FILES",
            serde_json::json!({
                "release_id": release_id,
                "release_manifest_sha256": file_sha256(&manifest).unwrap_or_default(),
                "files": files,
            }),
        ),
        Ok(_) => WorkerResponse::error("RELEASE_TOO_LARGE", "Release 文件数量超过上限"),
        Err(error) => WorkerResponse::error("RELEASE_INVALID", error.to_string()),
    }
}

fn directory_manifest(root: &Path) -> anyhow::Result<Vec<aursmith_protocol::ManifestEntry>> {
    let mut paths = std::collections::BTreeSet::new();
    collect_regular_files(root, root, &mut paths)?;
    paths
        .into_iter()
        .map(|path| {
            let file = root.join(&path);
            Ok(aursmith_protocol::ManifestEntry {
                path,
                sha256: file_sha256(&file)?,
                size: std::fs::metadata(file)?.len(),
            })
        })
        .collect()
}

fn initialize_publisher_gpg(home: &Path, public_key: &Path) -> anyhow::Result<String> {
    std::fs::create_dir_all(home)?;
    let metadata = std::fs::symlink_metadata(public_key)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        bail!("仓库 GPG 公钥类型或大小无效");
    }
    let status = std::process::Command::new("/usr/bin/gpg")
        .args([
            "--homedir",
            home.to_string_lossy().as_ref(),
            "--batch",
            "--import",
        ])
        .arg(public_key)
        .stdin(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        bail!("无法导入仓库 GPG 公钥");
    }
    let output = std::process::Command::new("/usr/bin/gpg")
        .arg("--homedir")
        .arg(home)
        .args(["--batch", "--with-colons", "--fingerprint"])
        .stdin(std::process::Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("无法读取仓库 GPG 指纹");
    }
    String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.split(':').collect::<Vec<_>>().get(9).copied())
        .find(|value| {
            value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .context("仓库 GPG 公钥没有有效指纹")
}

fn publish_repository_public_key(worker: &Worker, public_key: &Path) -> anyhow::Result<()> {
    let arch_root = worker.repository_dir.join(&worker.repository_arch);
    std::fs::create_dir_all(&arch_root)?;
    let target = arch_root.join("aursmith-repository-key.asc");
    if target.exists() {
        if file_sha256(public_key)? != file_sha256(&target)? {
            bail!("公开仓库已经存在不同 GPG 公钥");
        }
        return Ok(());
    }
    copy_regular_synced(public_key, &target)?;
    sync_directory(&arch_root)
}

fn spawn_publisher(worker: Arc<Worker>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            if let Err(error) = reconcile_publisher_one(&worker).await {
                tracing::warn!(%error, "Publisher 对账失败");
            }
        }
    });
}

async fn reconcile_publisher_one(worker: &Worker) -> anyhow::Result<()> {
    let row = sqlx::query("SELECT release_id, authorization_json FROM publisher_releases WHERE state = 'awaiting_signer' ORDER BY created_at LIMIT 1")
        .fetch_optional(&worker.database).await?;
    let Some(row) = row else {
        return Ok(());
    };
    let release_id: String = row.get("release_id");
    let signed = worker.signer_output.join(&release_id);
    if !signed.is_dir() {
        return Ok(());
    }
    let envelope: SignedEnvelope = serde_json::from_str(row.get("authorization_json"))?;
    let authorization: ReleaseAuthorization = envelope.verify("aursmith.release_authorization")?;
    match verify_and_publish_release(worker, &authorization, &envelope, &signed) {
        Ok(manifest_sha256) => {
            sqlx::query("UPDATE publisher_releases SET state = 'published', manifest_sha256 = ?, last_error = NULL, updated_at = ? WHERE release_id = ?")
                .bind(manifest_sha256).bind(Utc::now()).bind(release_id).execute(&worker.database).await?;
        }
        Err(error) => {
            sqlx::query("UPDATE publisher_releases SET state = 'failed', last_error = ?, updated_at = ? WHERE release_id = ?")
                .bind(error.to_string()).bind(Utc::now()).bind(release_id).execute(&worker.database).await?;
        }
    }
    Ok(())
}

fn verify_and_publish_release(
    worker: &Worker,
    authorization: &ReleaseAuthorization,
    envelope: &SignedEnvelope,
    signed: &Path,
) -> anyhow::Result<String> {
    if authorization.writer_epoch != worker.writer_epoch {
        bail!("签名完成后 writer epoch 已变化");
    }
    let manifest_path = signed.join("release-manifest.json");
    verify_gpg_signature(
        &worker.publisher_gpg_home,
        &manifest_path,
        &signed.join("release-manifest.json.sig"),
    )?;
    let manifest_bytes = std::fs::read(&manifest_path)?;
    let manifest: ReleaseManifest = serde_json::from_slice(&manifest_bytes)?;
    if manifest.release_id != authorization.release_id
        || manifest.batch_id != authorization.batch_id
        || manifest.writer_epoch != authorization.writer_epoch
        || manifest.repository_name != authorization.repository_name
        || manifest.source_git_commit != authorization.source_git_commit
        || manifest.artifacts != authorization.artifacts
        || manifest.removed_package_names != authorization.removed_package_names
    {
        bail!("ReleaseManifest 与 Controller 授权不一致");
    }
    let database_name = format!("{}.db.tar.gz", authorization.repository_name);
    let files_name = format!("{}.files.tar.gz", authorization.repository_name);
    if manifest.repository_database.path != database_name
        || manifest.repository_files.path != files_name
        || manifest
            .artifact_inspections
            .as_ref()
            .map(|entry| entry.path.as_str())
            != Some("artifact-inspections.json")
        || manifest
            .release_authorization
            .as_ref()
            .map(|entry| entry.path.as_str())
            != Some("authorization.json")
    {
        bail!("ReleaseManifest 仓库数据库名称无效");
    }
    for entry in [&manifest.repository_database, &manifest.repository_files] {
        verify_signed_entry(worker, signed, entry)?;
    }
    verify_manifest_entry(
        signed,
        manifest
            .artifact_inspections
            .as_ref()
            .context("ReleaseManifest 缺少 Artifact 检查报告")?,
    )?;
    let authorization_entry = manifest
        .release_authorization
        .as_ref()
        .context("ReleaseManifest 缺少 ReleaseAuthorization")?;
    verify_manifest_entry(signed, authorization_entry)?;
    if std::fs::read(signed.join(&authorization_entry.path))? != serde_json::to_vec(envelope)? {
        bail!("Signer 输出的 ReleaseAuthorization 与 Controller Envelope 不一致");
    }
    let mut package_names = std::collections::BTreeSet::new();
    for artifact in &authorization.artifacts {
        aursmith_protocol::validate_relative_path(&artifact.path)?;
        let file_name = Path::new(&artifact.path)
            .file_name()
            .context("Artifact 缺少文件名")?
            .to_string_lossy()
            .into_owned();
        if artifact.path != file_name || !package_names.insert(file_name.clone()) {
            bail!("Release Artifact 必须使用唯一的纯文件名");
        }
        verify_signed_entry(
            worker,
            signed,
            &aursmith_protocol::ManifestEntry {
                path: file_name,
                sha256: artifact.sha256.clone(),
                size: artifact.size,
            },
        )?;
    }

    let arch_root = worker.repository_dir.join(&worker.repository_arch);
    let releases_root = arch_root.join("releases");
    let release_id = authorization.release_id.to_string();
    let committed = releases_root.join(&release_id);
    let manifest_sha256 = hex::encode(Sha256::digest(&manifest_bytes));
    if committed.exists() {
        let existing = committed.join("release-manifest.json");
        if file_sha256(&existing)? != manifest_sha256 {
            bail!("公开 Release ID 已存在不同 Manifest");
        }
        activate_committed_release(worker, &committed)?;
        return Ok(manifest_sha256);
    }
    let staging = releases_root.join(format!(".{release_id}.staging"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;
    let mut release_files = vec![
        database_name.clone(),
        format!("{database_name}.sig"),
        files_name.clone(),
        format!("{files_name}.sig"),
        "release-manifest.json".into(),
        "release-manifest.json.sig".into(),
        "artifact-inspections.json".into(),
        "authorization.json".into(),
    ];
    for name in &package_names {
        release_files.push(name.clone());
        release_files.push(format!("{name}.sig"));
    }
    for name in &release_files {
        copy_regular_synced(&signed.join(name), &staging.join(name))?;
    }
    sync_directory(&staging)?;
    std::fs::create_dir_all(&releases_root)?;
    std::fs::rename(&staging, &committed)?;
    sync_directory(&releases_root)?;

    activate_committed_release(worker, &committed)?;
    Ok(manifest_sha256)
}

fn activate_committed_release(
    worker: &Worker,
    committed: &Path,
) -> anyhow::Result<ReleaseManifest> {
    let manifest_path = committed.join("release-manifest.json");
    verify_gpg_signature(
        &worker.publisher_gpg_home,
        &manifest_path,
        &committed.join("release-manifest.json.sig"),
    )?;
    let manifest: ReleaseManifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    if manifest.writer_epoch != worker.writer_epoch {
        bail!("Release writer epoch 与当前 Publisher 不一致");
    }
    verify_signed_entry(worker, committed, &manifest.repository_database)?;
    verify_signed_entry(worker, committed, &manifest.repository_files)?;
    if let Some(inspection) = &manifest.artifact_inspections {
        verify_manifest_entry(committed, inspection)?;
    }
    if let Some(authorization) = &manifest.release_authorization {
        verify_manifest_entry(committed, authorization)?;
    }
    let arch_root = worker.repository_dir.join(&worker.repository_arch);
    std::fs::create_dir_all(&arch_root)?;
    for artifact in &manifest.artifacts {
        let entry = aursmith_protocol::ManifestEntry {
            path: artifact.path.clone(),
            sha256: artifact.sha256.clone(),
            size: artifact.size,
        };
        verify_signed_entry(worker, committed, &entry)?;
        copy_new_or_verify(
            &committed.join(&artifact.path),
            &arch_root.join(&artifact.path),
        )?;
        let hot_signature = arch_root.join(format!("{}.sig", artifact.path));
        if hot_signature.exists() {
            verify_gpg_signature(
                &worker.publisher_gpg_home,
                &arch_root.join(&artifact.path),
                &hot_signature,
            )?;
        } else {
            copy_regular_synced(
                &committed.join(format!("{}.sig", artifact.path)),
                &hot_signature,
            )?;
        }
    }
    let release_id = manifest.release_id;
    let repository_name = &manifest.repository_name;
    atomic_release_link(
        &arch_root,
        &format!("{repository_name}.db.sig"),
        &format!(
            "releases/{release_id}/{}.sig",
            manifest.repository_database.path
        ),
    )?;
    atomic_release_link(
        &arch_root,
        &format!("{repository_name}.files.sig"),
        &format!(
            "releases/{release_id}/{}.sig",
            manifest.repository_files.path
        ),
    )?;
    atomic_release_link(
        &arch_root,
        &format!("{repository_name}.files"),
        &format!("releases/{release_id}/{}", manifest.repository_files.path),
    )?;
    atomic_release_link(
        &arch_root,
        &format!("{repository_name}.db"),
        &format!(
            "releases/{release_id}/{}",
            manifest.repository_database.path
        ),
    )?;
    sync_directory(&arch_root)?;
    Ok(manifest)
}

fn verify_signed_entry(
    worker: &Worker,
    root: &Path,
    entry: &aursmith_protocol::ManifestEntry,
) -> anyhow::Result<()> {
    verify_manifest_entry(root, entry)?;
    let path = root.join(&entry.path);
    verify_gpg_signature(
        &worker.publisher_gpg_home,
        &path,
        &root.join(format!("{}.sig", entry.path)),
    )
}

fn verify_manifest_entry(
    root: &Path,
    entry: &aursmith_protocol::ManifestEntry,
) -> anyhow::Result<()> {
    aursmith_protocol::validate_relative_path(&entry.path)?;
    let path = root.join(&entry.path);
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file()
        || metadata.len() != entry.size
        || file_sha256(&path)? != entry.sha256
    {
        bail!("Signer 输出与 Manifest 不匹配：{}", entry.path);
    }
    Ok(())
}

fn verify_gpg_signature(home: &Path, data: &Path, signature: &Path) -> anyhow::Result<()> {
    let signature_metadata = std::fs::symlink_metadata(signature)?;
    if !signature_metadata.file_type().is_file() {
        bail!("GPG 签名不是普通文件");
    }
    let status = std::process::Command::new("/usr/bin/gpg")
        .arg("--homedir")
        .arg(home)
        .args(["--batch", "--verify"])
        .arg(signature)
        .arg(data)
        .stdin(std::process::Stdio::null())
        .status()?;
    if !status.success() {
        bail!("GPG 签名验证失败：{}", data.display());
    }
    Ok(())
}

fn copy_regular_synced(source: &Path, target: &Path) -> anyhow::Result<()> {
    if !std::fs::symlink_metadata(source)?.file_type().is_file() {
        bail!("拒绝复制非普通文件：{}", source.display());
    }
    std::fs::copy(source, target)?;
    std::fs::File::open(target)?.sync_all()?;
    Ok(())
}

fn copy_new_or_verify(source: &Path, target: &Path) -> anyhow::Result<()> {
    if target.exists() {
        if file_sha256(source)? != file_sha256(target)? {
            bail!("公开 hot set 存在同名不同内容：{}", target.display());
        }
        return Ok(());
    }
    copy_regular_synced(source, target)
}

fn atomic_release_link(root: &Path, name: &str, target: &str) -> anyhow::Result<()> {
    let temporary = root.join(format!(".{name}.new"));
    if temporary.exists() || std::fs::symlink_metadata(&temporary).is_ok() {
        std::fs::remove_file(&temporary)?;
    }
    std::os::unix::fs::symlink(target, &temporary)?;
    std::fs::rename(temporary, root.join(name))?;
    Ok(())
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
}

fn verify_manifest_directory(
    directory: &Path,
    files: &[aursmith_protocol::ManifestEntry],
) -> anyhow::Result<()> {
    let mut expected = std::collections::BTreeSet::new();
    for entry in files {
        aursmith_protocol::validate_relative_path(&entry.path)?;
        if !expected.insert(entry.path.clone()) {
            bail!("Manifest 包含重复路径：{}", entry.path);
        }
        let path = directory.join(&entry.path);
        let metadata = std::fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.len() != entry.size
            || file_sha256(&path)? != entry.sha256
        {
            bail!("接收文件与 Manifest 不匹配：{}", entry.path);
        }
    }
    let mut actual = std::collections::BTreeSet::new();
    collect_regular_files(directory, directory, &mut actual)?;
    if actual != expected {
        bail!("接收目录文件集合与 Manifest 不一致");
    }
    Ok(())
}

fn verify_manifest_directory_shallow(
    directory: &Path,
    files: &[aursmith_protocol::ManifestEntry],
) -> anyhow::Result<()> {
    let mut expected = std::collections::BTreeSet::new();
    for entry in files {
        aursmith_protocol::validate_relative_path(&entry.path)?;
        if !expected.insert(entry.path.clone()) {
            bail!("Manifest 包含重复路径：{}", entry.path);
        }
        let metadata = std::fs::symlink_metadata(directory.join(&entry.path))?;
        if !metadata.file_type().is_file() || metadata.len() != entry.size {
            bail!("接收文件类型或大小与 Manifest 不匹配：{}", entry.path);
        }
    }
    let mut actual = std::collections::BTreeSet::new();
    collect_regular_files(directory, directory, &mut actual)?;
    if actual != expected {
        bail!("接收目录文件集合与 Manifest 不一致");
    }
    Ok(())
}

fn collect_regular_files(
    root: &Path,
    directory: &Path,
    files: &mut std::collections::BTreeSet<String>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if metadata.is_dir() {
            collect_regular_files(root, &entry.path(), files)?;
        } else if metadata.is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)?
                .to_string_lossy()
                .into_owned();
            aursmith_protocol::validate_relative_path(&relative)?;
            files.insert(relative);
        } else {
            bail!("接收目录包含非普通文件");
        }
    }
    Ok(())
}

async fn aur_search(worker: &Worker, query: &str) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以访问 AUR");
    }
    match worker.aur.search(query).await {
        Ok(packages) => WorkerResponse::ok("AUR_SEARCH", serde_json::json!({"items": packages})),
        Err(error) => WorkerResponse::error("AUR_UPSTREAM_ERROR", error.to_string()),
    }
}

async fn aur_info(worker: &Worker, names: &[String]) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以访问 AUR");
    }
    match worker.aur.info(names).await {
        Ok(packages) => WorkerResponse::ok("AUR_INFO", serde_json::json!({"items": packages})),
        Err(error) => WorkerResponse::error("AUR_UPSTREAM_ERROR", error.to_string()),
    }
}

async fn aur_providers(worker: &Worker, names: &[String]) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以访问 AUR");
    }
    if names.is_empty() || names.len() > 50 {
        return WorkerResponse::error("INVALID_REQUEST", "每次只能查询 1 至 50 个 Provider");
    }
    let mut items = serde_json::Map::new();
    for name in names {
        match worker.aur.providers(name).await {
            Ok(packages) => {
                items.insert(name.clone(), serde_json::json!(packages));
            }
            Err(error) => return WorkerResponse::error("AUR_UPSTREAM_ERROR", error.to_string()),
        }
    }
    WorkerResponse::ok("AUR_PROVIDERS", serde_json::Value::Object(items))
}

async fn official_info(worker: &Worker, names: &[String]) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以访问官方仓库元数据");
    }
    if names.is_empty() || names.len() > 50 {
        return WorkerResponse::error("INVALID_REQUEST", "每次只能查询 1 至 50 个官方包名");
    }
    let mut items = serde_json::Map::new();
    for name in names {
        match worker.aur.official(name).await {
            Ok(packages) => {
                items.insert(name.clone(), serde_json::json!(packages));
            }
            Err(error) => return WorkerResponse::error("ARCH_UPSTREAM_ERROR", error.to_string()),
        }
    }
    WorkerResponse::ok("OFFICIAL_INFO", serde_json::Value::Object(items))
}

async fn publisher_doctor(worker: &Worker) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以执行上游 Doctor");
    }
    let aur = match worker.aur.search("aursmith-doctor-connectivity").await {
        Ok(_) => serde_json::json!({"ok": true, "message": "AUR RPC 可达"}),
        Err(error) => serde_json::json!({"ok": false, "message": format!("AUR RPC 失败：{error}")}),
    };
    let source_proxy = match worker.source_proxy_url.as_deref() {
        None => serde_json::json!({"ok": false, "message": "未配置 AURSMITH_SOURCE_PROXY_URL"}),
        Some(proxy_url) => {
            let result = async {
                validate_source_proxy_url(proxy_url)?;
                let client = reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(5))
                    .timeout(std::time::Duration::from_secs(15))
                    .redirect(reqwest::redirect::Policy::none())
                    .proxy(reqwest::Proxy::all(proxy_url)?)
                    .build()?;
                client
                    .get("https://archlinux.org/robots.txt")
                    .send()
                    .await?
                    .error_for_status()?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            match result {
                Ok(()) => {
                    serde_json::json!({"ok": true, "message": "source proxy 可转发公开 HTTPS"})
                }
                Err(error) => {
                    serde_json::json!({"ok": false, "message": format!("source proxy 失败：{error}")})
                }
            }
        }
    };
    WorkerResponse::ok(
        "PUBLISHER_DOCTOR",
        serde_json::json!({"checks": {"aur": aur, "source_proxy": source_proxy}}),
    )
}

fn validate_source_proxy_url(value: &str) -> anyhow::Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(value).context("source proxy URL 无效")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        bail!("source proxy 必须是无内嵌凭据、查询参数和片段的 HTTP(S) URL");
    }
    Ok(parsed)
}

async fn aur_snapshot(
    worker: &Worker,
    package_base: &str,
    previous_vcs_commit: Option<&str>,
) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以访问 AUR");
    }
    match worker.aur.snapshot(package_base, previous_vcs_commit).await {
        Ok(snapshot) => WorkerResponse::ok("AUR_SNAPSHOT", serde_json::json!(snapshot)),
        Err(error) => WorkerResponse::error("AUR_UPSTREAM_ERROR", error.to_string()),
    }
}

async fn status(worker: &Worker) -> WorkerResponse {
    let state: Result<String, _> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'state'")
            .fetch_one(&worker.database)
            .await;
    let instance_id: Result<String, _> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'instance_id'")
            .fetch_one(&worker.database)
            .await;
    match (state, instance_id) {
        (Ok(state), Ok(instance_id)) => {
            let storage_path = match worker.role {
                WorkerRole::Builder => &worker.jobs_dir,
                WorkerRole::Publisher => &worker.repository_dir,
                WorkerRole::Archiver => &worker.archive_dir,
            };
            WorkerResponse::ok(
                "STATUS",
                serde_json::json!({
                    "name": worker.name,
                    "instance_id": instance_id,
                    "role": role_name(worker.role),
                    "state": state,
                    "protocol_major": PROTOCOL_MAJOR,
                    "writer_epoch": worker.writer_epoch,
                    "identity_signing_key_hex": hex::encode(worker.identity_signing_key.verifying_key().as_bytes()),
                    "repository_gpg_fingerprint": worker.repository_gpg_fingerprint,
                    "storage": disk_usage(storage_path),
                    "cgroup_v2": Path::new("/sys/fs/cgroup/cgroup.controllers").exists(),
                    "kvm_available": worker.role != WorkerRole::Builder || Path::new("/dev/kvm").exists(),
                    "profiles": worker.builder.as_ref().map(builder::BuilderRuntime::available_profiles).unwrap_or_default(),
                    "time": Utc::now(),
                }),
            )
        }
        (Err(error), _) | (_, Err(error)) => {
            WorkerResponse::error("JOURNAL_ERROR", error.to_string())
        }
    }
}

fn disk_usage(path: &Path) -> Option<serde_json::Value> {
    let output = std::process::Command::new("/usr/bin/df")
        .args(["-Pk", "--"])
        .arg(path)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let fields = text.lines().last()?.split_whitespace().collect::<Vec<_>>();
    if fields.len() < 6 {
        return None;
    }
    let total = fields[1].parse::<u64>().ok()?.saturating_mul(1024);
    let available = fields[3].parse::<u64>().ok()?.saturating_mul(1024);
    Some(serde_json::json!({
        "path": path,
        "total_bytes": total,
        "available_bytes": available,
        "available_percent": if total == 0 { 0 } else { available.saturating_mul(100) / total },
    }))
}

async fn drain(worker: &Worker) -> WorkerResponse {
    match sqlx::query("UPDATE worker_state SET value = 'draining' WHERE key = 'state'")
        .execute(&worker.database)
        .await
    {
        Ok(_) => WorkerResponse::ok("DRAINING", serde_json::json!({"state": "draining"})),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

async fn submit(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if envelope.schema_major != PROTOCOL_MAJOR {
        return WorkerResponse::error("INCOMPATIBLE_PROTOCOL", "协议 major version 不兼容");
    }
    if envelope.verifying_key != worker.trusted_controller_key {
        return WorkerResponse::error("UNTRUSTED_CONTROLLER", "授权不是由受信任 Controller 签发");
    }
    let spec: JobSpec = match envelope.verify("aursmith.job_spec") {
        Ok(spec) => spec,
        Err(error) => return WorkerResponse::error("INVALID_ENVELOPE", error.to_string()),
    };
    if spec.required_role != worker.role {
        return WorkerResponse::error("WRONG_ROLE", "任务角色与 Worker 不匹配");
    }
    if spec.is_expired_at(Utc::now()) {
        return WorkerResponse::error("EXPIRED_JOB", "JobSpec 已过期");
    }
    if spec.job_id != spec.attempt.job_id {
        return WorkerResponse::error("INVALID_ATTEMPT", "JobSpec 和 Attempt 的 job_id 不一致");
    }
    let state: Result<String, _> =
        sqlx::query_scalar("SELECT value FROM worker_state WHERE key = 'state'")
            .fetch_one(&worker.database)
            .await;
    if !matches!(state.as_deref(), Ok("online")) {
        return WorkerResponse::error("WORKER_DRAINING", "Worker 当前不接收新任务");
    }
    let envelope_sha256 = hex::encode(Sha256::digest(&envelope.payload));
    let spec_json = match serde_json::to_string(&envelope) {
        Ok(value) => value,
        Err(error) => return WorkerResponse::error("INVALID_ENVELOPE", error.to_string()),
    };
    let existing = sqlx::query(
        "SELECT attempt_id, generation, envelope_sha256, status FROM attempts WHERE job_id = ? ORDER BY generation DESC LIMIT 1",
    )
    .bind(spec.job_id.to_string())
    .fetch_optional(&worker.database)
    .await;
    let existing = match existing {
        Ok(value) => value,
        Err(error) => return WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    };
    if let Some(row) = existing {
        let generation: i64 = row.get("generation");
        let attempt_id: String = row.get("attempt_id");
        let previous_sha256: String = row.get("envelope_sha256");
        let previous_status: String = row.get("status");
        if i64::from(spec.attempt.generation) < generation {
            return WorkerResponse::error("STALE_ATTEMPT", "Attempt generation 已经过期");
        }
        if i64::from(spec.attempt.generation) == generation {
            if attempt_id == spec.attempt.attempt_id.to_string()
                && previous_sha256 == envelope_sha256
            {
                return WorkerResponse::ok(
                    "IDEMPOTENT_REPLAY",
                    serde_json::json!({"status": previous_status}),
                );
            }
            return WorkerResponse::error(
                "ATTEMPT_CONFLICT",
                "相同 generation 的 Attempt 内容冲突",
            );
        }
    }

    if !spec.inline_inputs.is_empty() && spec.source_attempt_id.is_some() {
        return WorkerResponse::error(
            "AMBIGUOUS_JOB_INPUT",
            "Job 不能同时携带内联输入和已准备源码引用",
        );
    }
    let needs_materialization = !spec.inline_inputs.is_empty() || spec.source_attempt_id.is_some();
    let initial_status = if !needs_materialization {
        "queued"
    } else {
        "preparing"
    };
    let inserted = sqlx::query(
        "INSERT INTO attempts(job_id, attempt_id, generation, envelope_sha256, status, received_at, spec_json) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(spec.job_id.to_string())
    .bind(spec.attempt.attempt_id.to_string())
    .bind(i64::from(spec.attempt.generation))
    .bind(envelope_sha256)
    .bind(initial_status)
    .bind(Utc::now())
    .bind(spec_json)
    .execute(&worker.database)
    .await;
    match inserted {
        Ok(_) => {
            if needs_materialization {
                let Some(builder) = &worker.builder else {
                    let _ = sqlx::query("DELETE FROM attempts WHERE attempt_id = ?")
                        .bind(spec.attempt.attempt_id.to_string())
                        .execute(&worker.database)
                        .await;
                    return WorkerResponse::error(
                        "INLINE_INPUT_UNSUPPORTED",
                        "只有 Builder 可以接收内联构建输入",
                    );
                };
                let materialized = if spec.source_attempt_id.is_some() {
                    builder.materialize_prepared_source(&spec)
                } else {
                    builder.materialize_inline_inputs(&spec)
                };
                if let Err(error) = materialized {
                    let _ = sqlx::query("DELETE FROM attempts WHERE attempt_id = ?")
                        .bind(spec.attempt.attempt_id.to_string())
                        .execute(&worker.database)
                        .await;
                    return WorkerResponse::error("INVALID_INLINE_INPUT", error.to_string());
                }
                if let Err(error) = sqlx::query(
                    "UPDATE attempts SET status = 'queued' WHERE attempt_id = ? AND status = 'preparing'",
                )
                .bind(spec.attempt.attempt_id.to_string())
                .execute(&worker.database)
                .await
                {
                    let _ = sqlx::query("DELETE FROM attempts WHERE attempt_id = ?")
                        .bind(spec.attempt.attempt_id.to_string())
                        .execute(&worker.database)
                        .await;
                    let _ = std::fs::remove_dir_all(
                        builder
                            .jobs_dir()
                            .join("staging")
                            .join(spec.attempt.attempt_id.to_string()),
                    );
                    return WorkerResponse::error("JOURNAL_ERROR", error.to_string());
                }
            }
            WorkerResponse::ok(
                "ACCEPTED",
                serde_json::json!({
                    "job_id": spec.job_id,
                    "attempt_id": spec.attempt.attempt_id,
                    "generation": spec.attempt.generation,
                    "status": status_name(JobStatus::Queued),
                }),
            )
        }
        Err(error) => {
            if let Some(builder) = &worker.builder {
                let _ = std::fs::remove_dir_all(
                    builder
                        .jobs_dir()
                        .join("staging")
                        .join(spec.attempt.attempt_id.to_string()),
                );
            }
            WorkerResponse::error("JOURNAL_ERROR", error.to_string())
        }
    }
}

async fn query(worker: &Worker, job_id: &str) -> WorkerResponse {
    let row = sqlx::query(
        "SELECT attempt_id, generation, status, received_at, result_sha256, failure_code FROM attempts WHERE job_id = ? ORDER BY generation DESC LIMIT 1",
    )
    .bind(job_id)
    .fetch_optional(&worker.database)
    .await;
    match row {
        Ok(Some(row)) => {
            let attempt_id = row.get::<String, _>("attempt_id");
            let status = row.get::<String, _>("status");
            let (guest_result_json, evidence_logs) = if status == "succeeded" {
                let Some(builder) = &worker.builder else {
                    return WorkerResponse::error(
                        "RESULT_UNAVAILABLE",
                        "该 Worker 角色没有 Builder 结果目录",
                    );
                };
                match builder.completed_result_json(&attempt_id) {
                    Ok(result) => match builder.attempt_logs(&attempt_id, true) {
                        Ok(logs) => (Some(result), logs),
                        Err(error) => {
                            return WorkerResponse::error("RESULT_UNAVAILABLE", error.to_string());
                        }
                    },
                    Err(error) => {
                        return WorkerResponse::error("RESULT_UNAVAILABLE", error.to_string());
                    }
                }
            } else if status == "failed" {
                let Some(builder) = &worker.builder else {
                    return WorkerResponse::error(
                        "RESULT_UNAVAILABLE",
                        "该 Worker 角色没有 Builder 诊断目录",
                    );
                };
                match builder.attempt_logs(&attempt_id, false) {
                    Ok(logs) => (None, logs),
                    Err(error) => {
                        return WorkerResponse::error("RESULT_UNAVAILABLE", error.to_string());
                    }
                }
            } else {
                (None, Vec::new())
            };
            WorkerResponse::ok(
                "JOB_STATUS",
                serde_json::json!({
                    "job_id": job_id,
                    "attempt_id": attempt_id,
                    "generation": row.get::<i64, _>("generation"),
                    "status": status,
                    "received_at": row.get::<String, _>("received_at"),
                    "result_sha256": row.get::<Option<String>, _>("result_sha256"),
                    "failure_code": row.get::<Option<String>, _>("failure_code"),
                    "guest_result_json": guest_result_json,
                    "evidence_logs": evidence_logs,
                }),
            )
        }
        Ok(None) => WorkerResponse::error("JOB_NOT_FOUND", "Worker Journal 中没有该任务"),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
}

fn role_name(role: WorkerRole) -> &'static str {
    match role {
        WorkerRole::Builder => "builder",
        WorkerRole::Publisher => "publisher",
        WorkerRole::Archiver => "archiver",
    }
}

fn status_name(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::NoEligibleWorker => "no_eligible_worker",
        JobStatus::Dispatched => "dispatched",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "succeeded",
        JobStatus::Failed => "failed",
        JobStatus::Cancelled => "cancelled",
        JobStatus::Uncertain => "uncertain",
    }
}

#[allow(dead_code)]
fn worker_state_name(state: WorkerState) -> &'static str {
    match state {
        WorkerState::Online => "online",
        WorkerState::Draining => "draining",
        WorkerState::Offline => "offline",
        WorkerState::Degraded => "degraded",
        WorkerState::Incompatible => "incompatible",
    }
}

#[cfg(test)]
mod transfer_tests {
    use super::*;

    #[test]
    fn source_proxy_url_rejects_credentials_and_query_parameters() {
        assert!(validate_source_proxy_url("http://source-proxy:3128").is_ok());
        assert!(validate_source_proxy_url("http://user:secret@source-proxy:3128").is_err());
        assert!(validate_source_proxy_url("http://source-proxy:3128/?target=private").is_err());
    }

    #[test]
    fn export_materialization_verifies_digest_and_path() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("fixture.pkg.tar.zst"), b"package").unwrap();
        let entry = aursmith_protocol::ManifestEntry {
            path: "fixture.pkg.tar.zst".into(),
            sha256: hex::encode(Sha256::digest(b"package")),
            size: 7,
        };
        materialize_export(&source, &destination, std::slice::from_ref(&entry)).unwrap();
        assert_eq!(
            std::fs::read(destination.join(&entry.path)).unwrap(),
            b"package"
        );

        let traversal = aursmith_protocol::ManifestEntry {
            path: "../secret".into(),
            sha256: entry.sha256,
            size: entry.size,
        };
        assert!(materialize_export(&source, &root.path().join("bad"), &[traversal]).is_err());
    }

    #[test]
    fn release_authorization_requires_complete_flat_package_metadata() {
        let mut authorization = ReleaseAuthorization {
            release_id: uuid::Uuid::new_v4(),
            batch_id: uuid::Uuid::new_v4(),
            writer_epoch: 1,
            repository_name: "aursmith".into(),
            source_git_commit: "a".repeat(40),
            revision_sha256s: vec!["b".repeat(64)],
            audit_report_sha256s: vec!["c".repeat(64)],
            artifacts: vec![aursmith_protocol::ArtifactRecord {
                path: "fixture-1-1-any.pkg.tar.zst".into(),
                sha256: "d".repeat(64),
                size: 1,
                package_name: Some("fixture".into()),
                package_version: Some("1-1".into()),
                architecture: Some("any".into()),
            }],
            removed_package_names: vec![],
            evidence: Default::default(),
            issued_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        assert!(validate_release_authorization_for_publisher(&authorization).is_ok());
        authorization.artifacts[0].path = "nested/fixture-1-1-any.pkg.tar.zst".into();
        assert!(validate_release_authorization_for_publisher(&authorization).is_err());
        authorization.artifacts.clear();
        authorization.removed_package_names = vec!["fixture".into()];
        assert!(validate_release_authorization_for_publisher(&authorization).is_ok());
        authorization.removed_package_names = vec!["../fixture".into()];
        assert!(validate_release_authorization_for_publisher(&authorization).is_err());
    }

    #[test]
    fn archive_inventory_distinguishes_shallow_and_full_digest_checks() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("release-manifest.json"), b"good").unwrap();
        let entry = aursmith_protocol::ManifestEntry {
            path: "release-manifest.json".into(),
            sha256: hex::encode(Sha256::digest(b"good")),
            size: 4,
        };
        assert!(
            verify_manifest_directory_shallow(root.path(), std::slice::from_ref(&entry)).is_ok()
        );
        assert!(verify_manifest_directory(root.path(), std::slice::from_ref(&entry)).is_ok());
        std::fs::write(root.path().join("release-manifest.json"), b"evil").unwrap();
        assert!(
            verify_manifest_directory_shallow(root.path(), std::slice::from_ref(&entry)).is_ok()
        );
        assert!(verify_manifest_directory(root.path(), &[entry]).is_err());
    }

    #[test]
    fn control_plane_backup_requires_controller_signature_and_bound_database() {
        let root = tempfile::tempdir().unwrap();
        let database_path = root.path().join("controller.db");
        std::fs::write(&database_path, b"sqlite-backup").unwrap();
        let database = aursmith_protocol::ManifestEntry {
            path: "controller.db".into(),
            sha256: file_sha256(&database_path).unwrap(),
            size: 13,
        };
        let backup_id = uuid::Uuid::new_v4();
        let controller_key = SigningKey::from_bytes(&[23_u8; 32]);
        let backup = ControlPlaneBackup {
            backup_id,
            database: database.clone(),
            source_git_commit: "test".into(),
            created_at: Utc::now(),
        };
        let envelope =
            SignedEnvelope::sign("aursmith.control_plane_backup", &backup, &controller_key)
                .unwrap();
        let envelope_path = root.path().join("backup-envelope.json");
        std::fs::write(&envelope_path, serde_json::to_vec(&envelope).unwrap()).unwrap();
        let envelope_entry = aursmith_protocol::ManifestEntry {
            path: "backup-envelope.json".into(),
            sha256: file_sha256(&envelope_path).unwrap(),
            size: envelope_path.metadata().unwrap().len(),
        };
        let capability = TransferCapability {
            id: uuid::Uuid::new_v4(),
            source_worker: uuid::Uuid::new_v4(),
            destination_worker: uuid::Uuid::new_v4(),
            attempt: None,
            release_id: None,
            backup_id: Some(backup_id),
            writer_epoch: 0,
            files: vec![database, envelope_entry],
            expires_at: Utc::now() + chrono::Duration::minutes(5),
        };
        assert_eq!(
            validate_control_plane_backup_input(
                controller_key.verifying_key().as_bytes(),
                &capability,
                root.path(),
            )
            .unwrap(),
            backup_id
        );
        assert!(
            validate_control_plane_backup_input(&[0_u8; 32], &capability, root.path()).is_err()
        );
    }
}
