mod aur;
mod builder;

use anyhow::{Context, bail};
use aursmith_domain::{JobStatus, WorkerRole, WorkerState};
use aursmith_protocol::{JobSpec, PROTOCOL_MAJOR, SignedEnvelope, TransferCapability};
use chrono::Utc;
use clap::Parser;
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
    builder: Option<builder::BuilderRuntime>,
    transfer_endpoints: BTreeMap<String, String>,
    transfer_ssh_identity_file: Option<PathBuf>,
    transfer_ssh_known_hosts_file: Option<PathBuf>,
    landing_dir: PathBuf,
    writer_epoch: u64,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum WorkerCommand {
    Status,
    Drain,
    Submit { envelope: SignedEnvelope },
    Query { job_id: String },
    AurSearch { query: String },
    AurInfo { names: Vec<String> },
    AurProviders { names: Vec<String> },
    OfficialInfo { names: Vec<String> },
    AurSnapshot { package_base: String },
    AuthorizeExport { envelope: SignedEnvelope },
    ResolveExport { capability_id: String },
    AuthorizeImport { envelope: SignedEnvelope },
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
    let transfer_endpoints: BTreeMap<String, String> =
        serde_json::from_str(&cli.transfer_endpoints_json)
            .context("AURSMITH_TRANSFER_ENDPOINTS_JSON 不是字符串映射")?;
    let aur = aur::AurClient::new(&cli.aur_base_url)?;
    let worker = Arc::new(Worker {
        name: cli.name,
        role: cli.role.into(),
        database,
        trusted_controller_key,
        aur,
        builder: if matches!(cli.role, RoleArg::Builder) {
            Some(builder::BuilderRuntime::new(
                cli.profiles_dir.into(),
                cli.jobs_dir.into(),
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
    });
    if worker.builder.is_some() {
        builder::spawn(
            worker.database.clone(),
            worker.trusted_controller_key.clone(),
            worker.builder.clone().expect("已检查 Builder runtime"),
        );
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
        WorkerCommand::AurSnapshot { package_base } => aur_snapshot(worker, &package_base).await,
        WorkerCommand::AuthorizeExport { envelope } => authorize_export(worker, envelope).await,
        WorkerCommand::ResolveExport { capability_id } => {
            resolve_export(worker, &capability_id).await
        }
        WorkerCommand::AuthorizeImport { envelope } => authorize_import(worker, envelope).await,
    }
}

async fn authorize_export(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if worker.role != WorkerRole::Builder {
        return WorkerResponse::error("WRONG_ROLE", "只有 Builder 可以导出 Artifact");
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
    let Some(runtime) = &worker.builder else {
        return WorkerResponse::error("WRONG_ROLE", "Builder runtime 不可用");
    };
    let Some(attempt) = &capability.attempt else {
        return WorkerResponse::error("ATTEMPT_REQUIRED", "Artifact 导出必须绑定 Attempt");
    };
    let source = runtime
        .jobs_dir()
        .join("completed")
        .join(attempt.attempt_id.to_string())
        .join("output");
    let directory = runtime
        .jobs_dir()
        .join("transfers")
        .join(capability.id.to_string());
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

async fn authorize_import(worker: &Worker, envelope: SignedEnvelope) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以接收 Artifact");
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
    if capability.writer_epoch != worker.writer_epoch {
        return WorkerResponse::error(
            "WRITER_EPOCH_MISMATCH",
            "TransferCapability 不属于当前 Publisher writer epoch",
        );
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
        Ok(_) => WorkerResponse::ok(
            "IMPORT_VERIFIED",
            serde_json::json!({"capability_id": capability.id, "files": capability.files.len()}),
        ),
        Err(error) => WorkerResponse::error("JOURNAL_ERROR", error.to_string()),
    }
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

async fn aur_snapshot(worker: &Worker, package_base: &str) -> WorkerResponse {
    if worker.role != WorkerRole::Publisher {
        return WorkerResponse::error("WRONG_ROLE", "只有 Publisher 可以访问 AUR");
    }
    match worker.aur.snapshot(package_base).await {
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
        (Ok(state), Ok(instance_id)) => WorkerResponse::ok(
            "STATUS",
            serde_json::json!({
                "name": worker.name,
                "instance_id": instance_id,
                "role": role_name(worker.role),
                "state": state,
                "protocol_major": PROTOCOL_MAJOR,
                "writer_epoch": worker.writer_epoch,
                "profiles": worker.builder.as_ref().map(builder::BuilderRuntime::available_profiles).unwrap_or_default(),
                "time": Utc::now(),
            }),
        ),
        (Err(error), _) | (_, Err(error)) => {
            WorkerResponse::error("JOURNAL_ERROR", error.to_string())
        }
    }
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
            let guest_result_json = if status == "succeeded" {
                let Some(builder) = &worker.builder else {
                    return WorkerResponse::error(
                        "RESULT_UNAVAILABLE",
                        "该 Worker 角色没有 Builder 结果目录",
                    );
                };
                match builder.completed_result_json(&attempt_id) {
                    Ok(result) => Some(result),
                    Err(error) => {
                        return WorkerResponse::error("RESULT_UNAVAILABLE", error.to_string());
                    }
                }
            } else {
                None
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
}
