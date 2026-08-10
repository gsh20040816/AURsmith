use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use std::{
    env,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[derive(Debug, Parser)]
#[command(name = "aursmithctl", version, about = "AURsmith 容器内运维工具")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
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
    /// 生成 Controller Ed25519 密钥；私钥只输出一次，必须写入 secret。
    GenerateControllerKey,
}

#[derive(Debug, Subcommand)]
enum WorkerCommand {
    Status,
    Drain,
    Query { job_id: String },
    Submit { envelope_file: PathBuf },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
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
        Command::GenerateControllerKey => generate_controller_key()?,
    }
    Ok(())
}

fn run_sshd(
    host_key_source: &Path,
    authorized_keys_source: &Path,
    private_directory: &Path,
    config: &Path,
) -> anyhow::Result<()> {
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
            "/usr/bin/sshd",
            "-D",
            "-e",
            "-f",
        ])
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

fn generate_controller_key() -> anyhow::Result<()> {
    let mut secret = [0_u8; 32];
    std::fs::File::open("/dev/urandom")
        .context("无法打开系统随机源")?
        .read_exact(&mut secret)
        .context("无法从系统随机源读取密钥")?;
    let signing_key = SigningKey::from_bytes(&secret);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "private_key_hex": hex::encode(secret),
            "public_key_hex": hex::encode(signing_key.verifying_key().to_bytes()),
        }))?
    );
    Ok(())
}

async fn ssh_gateway(socket: &PathBuf) -> anyhow::Result<()> {
    let original = env::var("SSH_ORIGINAL_COMMAND").unwrap_or_default();
    let parts: Vec<_> = original.split_ascii_whitespace().collect();
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
        _ => bail!("SSH 命令未被允许"),
    };
    let response = worker_request(socket, request).await?;
    println!("{}", serde_json::to_string(&response)?);
    if !response.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        bail!("Worker 返回失败");
    }
    Ok(())
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
        checks.push(
            json!({"check": "virtiofsd", "ok": command_works("/usr/lib/virtiofsd", "--version")}),
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
