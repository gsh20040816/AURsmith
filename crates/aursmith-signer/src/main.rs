use anyhow::{Context, bail};
use aursmith_protocol::{
    ArtifactRecord, ManifestEntry, ReleaseAuthorization, ReleaseManifest, SignedEnvelope,
};
use chrono::Utc;
use clap::Parser;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Debug, Parser)]
#[command(name = "aursmith-signer", version)]
struct Cli {
    #[arg(long, env = "AURSMITH_SIGNER_INBOX", default_value = "/inbox")]
    inbox: PathBuf,
    #[arg(long, env = "AURSMITH_SIGNER_OUTPUT", default_value = "/signed")]
    output: PathBuf,
    #[arg(long, env = "AURSMITH_CONTROLLER_VERIFYING_KEY_HEX")]
    controller_key_hex: String,
    #[arg(long, env = "AURSMITH_GPG_PRIVATE_KEY_FILE")]
    gpg_private_key: PathBuf,
    #[arg(long, env = "AURSMITH_GPG_HOME", default_value = "/run/aursmith-gnupg")]
    gpg_home: PathBuf,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "aursmith=info".into()))
        .with(tracing_subscriber::fmt::layer().json())
        .init();
    let cli = Cli::parse();
    let controller_key = hex::decode(&cli.controller_key_hex)?;
    if controller_key.len() != 32 {
        bail!("Controller verifying key 必须是 32 字节");
    }
    initialize_gpg(&cli)?;
    loop {
        if let Err(error) = process_one(&cli, &controller_key) {
            tracing::warn!(%error, "Signer 处理 Release 失败");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn initialize_gpg(cli: &Cli) -> anyhow::Result<()> {
    fs::create_dir_all(&cli.gpg_home)?;
    let metadata = fs::symlink_metadata(&cli.gpg_private_key)?;
    if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
        bail!("GPG 私钥 secret 类型或大小无效");
    }
    run_checked(
        "/usr/bin/gpg",
        &[
            "--homedir".into(),
            cli.gpg_home.as_os_str().into(),
            "--batch".into(),
            "--import".into(),
            cli.gpg_private_key.as_os_str().into(),
        ],
    )
}

fn process_one(cli: &Cli, controller_key: &[u8]) -> anyhow::Result<()> {
    let Some(entry) = fs::read_dir(&cli.inbox)?
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && !entry.file_name().to_string_lossy().starts_with('.')
        })
    else {
        return Ok(());
    };
    let authorization_path = entry.path().join("authorization.json");
    let envelope: SignedEnvelope = serde_json::from_slice(&fs::read(&authorization_path)?)?;
    if envelope.verifying_key != controller_key {
        bail!("ReleaseAuthorization 不是由当前 Controller 签发");
    }
    let authorization: ReleaseAuthorization = envelope.verify("aursmith.release_authorization")?;
    validate_authorization(&authorization, &entry.path())?;
    let release_id = authorization.release_id.to_string();
    if entry.file_name().to_string_lossy() != release_id {
        bail!("inbox 目录与 Release ID 不匹配");
    }
    let staging = cli.output.join(format!(".{release_id}.staging"));
    let committed = cli.output.join(&release_id);
    if committed.exists() {
        return Ok(());
    }
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    fs::create_dir_all(&staging)?;
    let mut package_paths = Vec::new();
    for artifact in &authorization.artifacts {
        let source = entry.path().join(&artifact.path);
        let destination = staging.join(
            Path::new(&artifact.path)
                .file_name()
                .context("Artifact 缺少文件名")?,
        );
        fs::copy(source, &destination)?;
        gpg_sign(cli, &destination)?;
        package_paths.push(destination);
    }
    package_paths.sort();
    let database = staging.join(format!("{}.db.tar.gz", authorization.repository_name));
    let mut repo_arguments = vec![database.as_os_str().to_owned()];
    repo_arguments.extend(package_paths.iter().map(|path| path.as_os_str().to_owned()));
    run_checked("/usr/bin/repo-add", &repo_arguments)?;
    gpg_sign(cli, &database)?;
    let files_database = staging.join(format!("{}.files.tar.gz", authorization.repository_name));
    gpg_sign(cli, &files_database)?;
    let inspection_source = entry.path().join("artifact-inspections.json");
    let inspection_bytes = fs::read(&inspection_source)?;
    let inspections: Vec<serde_json::Value> = serde_json::from_slice(&inspection_bytes)?;
    if inspection_bytes.len() > 10 * 1024 * 1024
        || inspections.len() != authorization.artifacts.len()
    {
        bail!("Publisher Artifact 检查报告数量或大小无效");
    }
    let inspection_destination = staging.join("artifact-inspections.json");
    fs::write(&inspection_destination, inspection_bytes)?;
    let manifest = ReleaseManifest {
        release_id: authorization.release_id,
        batch_id: authorization.batch_id,
        source_git_commit: authorization.source_git_commit,
        repository_name: authorization.repository_name,
        writer_epoch: authorization.writer_epoch,
        artifacts: authorization.artifacts,
        repository_database: file_entry(&database)?,
        repository_files: file_entry(&files_database)?,
        artifact_inspections: Some(file_entry(&inspection_destination)?),
        committed_at: Utc::now(),
    };
    fs::write(
        staging.join("release-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    gpg_sign(cli, &staging.join("release-manifest.json"))?;
    fs::rename(staging, committed)?;
    Ok(())
}

fn validate_authorization(authorization: &ReleaseAuthorization, root: &Path) -> anyhow::Result<()> {
    if authorization.expires_at < Utc::now() {
        bail!("ReleaseAuthorization 已过期");
    }
    if authorization.repository_name.is_empty()
        || !authorization
            .repository_name
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '-' || value == '_')
    {
        bail!("仓库名称无效");
    }
    if authorization.artifacts.is_empty() {
        bail!("Release 不包含软件包");
    }
    let mut artifact_paths = std::collections::BTreeSet::new();
    for artifact in &authorization.artifacts {
        aursmith_protocol::validate_relative_path(&artifact.path)?;
        let file_name = Path::new(&artifact.path)
            .file_name()
            .context("Artifact 缺少文件名")?
            .to_string_lossy();
        if file_name != artifact.path
            || !artifact_paths.insert(artifact.path.clone())
            || !artifact.path.contains(".pkg.tar.")
        {
            bail!("Artifact 不是 Arch 软件包");
        }
        let path = root.join(&artifact.path);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.len() != artifact.size
            || digest_file(&path)? != artifact.sha256
        {
            bail!("Artifact Manifest 不匹配：{}", artifact.path);
        }
        validate_package_metadata(&path, artifact)?;
    }
    Ok(())
}

fn validate_package_metadata(path: &Path, artifact: &ArtifactRecord) -> anyhow::Result<()> {
    let output = Command::new("/usr/bin/bsdtar")
        .args(["-xOf"])
        .arg(path)
        .arg(".PKGINFO")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("软件包缺少有效 .PKGINFO");
    }
    let text = String::from_utf8(output.stdout)?;
    for (field, expected) in [
        ("pkgname", artifact.package_name.as_deref()),
        ("pkgver", artifact.package_version.as_deref()),
        ("arch", artifact.architecture.as_deref()),
    ] {
        let actual = text
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .find_map(|(name, value)| (name == field).then_some(value));
        if expected.is_none() || actual != expected {
            bail!("软件包元数据与 BuildResult 不匹配：{field}");
        }
    }
    Ok(())
}

fn gpg_sign(cli: &Cli, path: &Path) -> anyhow::Result<()> {
    run_checked(
        "/usr/bin/gpg",
        &[
            "--homedir".into(),
            cli.gpg_home.as_os_str().into(),
            "--batch".into(),
            "--yes".into(),
            "--detach-sign".into(),
            path.as_os_str().into(),
        ],
    )
}

fn run_checked(program: &str, arguments: &[OsString]) -> anyhow::Result<()> {
    let status = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("子进程失败：{program}，状态 {status}");
    }
    Ok(())
}

fn file_entry(path: &Path) -> anyhow::Result<ManifestEntry> {
    Ok(ManifestEntry {
        path: path
            .file_name()
            .context("文件缺少名称")?
            .to_string_lossy()
            .into_owned(),
        sha256: digest_file(path)?,
        size: fs::metadata(path)?.len(),
    })
}

fn digest_file(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use uuid::Uuid;

    #[test]
    fn authorization_rejects_traversal_before_running_tools() {
        let root = tempfile::tempdir().unwrap();
        let authorization = ReleaseAuthorization {
            release_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            writer_epoch: 1,
            repository_name: "aursmith".into(),
            source_git_commit: "a".repeat(40),
            revision_sha256s: vec!["b".repeat(64)],
            audit_report_sha256s: vec!["c".repeat(64)],
            artifacts: vec![ArtifactRecord {
                path: "../evil.pkg.tar.zst".into(),
                sha256: "d".repeat(64),
                size: 1,
                package_name: Some("evil".into()),
                package_version: Some("1-1".into()),
                architecture: Some("any".into()),
            }],
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        };
        assert!(validate_authorization(&authorization, root.path()).is_err());
    }
}
