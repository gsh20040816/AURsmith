use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::{env, path::PathBuf};
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
    }
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
