use anyhow::{Context, bail};
use aursmith_protocol::{BuildProfileSpec, ManifestEntry};
use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use ed25519_dalek::SigningKey;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
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
enum WorkerCommand {
    Status,
    Drain,
    Query { job_id: String },
    Submit { envelope_file: PathBuf },
    AurSearch { query: String },
    AurInfo { names: Vec<String> },
    AurProviders { names: Vec<String> },
    OfficialInfo { names: Vec<String> },
    AurSnapshot { package_base: String },
    AuthorizeExport { envelope_file: PathBuf },
    AuthorizeImport { envelope_file: PathBuf },
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
                WorkerCommand::AurSnapshot { package_base } => {
                    json!({"command": "aur_snapshot", "package_base": package_base})
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
        Command::ExportProfile {
            source,
            output,
            name,
        } => export_profile(&source, &output, &name)?,
        Command::RsyncSsh { arguments } => rsync_ssh(arguments)?,
    }
    Ok(())
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
    if parts.first() == Some(&"rsync") {
        return rsync_sender_gateway(socket, &parts).await;
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
        ["aur-snapshot"] => read_limited_json_command("aur_snapshot").await?,
        ["authorize-export"] | ["authorize-import"] => {
            let mut bytes = Vec::new();
            tokio::io::stdin()
                .take(4 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .await
                .context("读取 TransferCapability Envelope 失败")?;
            let envelope: Value = serde_json::from_slice(&bytes)
                .context("TransferCapability Envelope 不是有效 JSON")?;
            json!({
                "command": if parts[0] == "authorize-export" { "authorize_export" } else { "authorize_import" },
                "envelope": envelope
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

async fn rsync_sender_gateway(socket: &PathBuf, parts: &[&str]) -> anyhow::Result<()> {
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
