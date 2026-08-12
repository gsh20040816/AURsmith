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
        if let Err(error) = process_pending(&cli, &controller_key) {
            tracing::warn!(%error, "Signer 扫描 Release inbox 失败");
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

fn process_pending(cli: &Cli, controller_key: &[u8]) -> anyhow::Result<()> {
    for entry in fs::read_dir(&cli.inbox)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && !entry.file_name().to_string_lossy().starts_with('.')
        })
    {
        if let Err(error) = process_release(cli, controller_key, &entry) {
            tracing::warn!(
                %error,
                release_directory = %entry.file_name().to_string_lossy(),
                "Signer 处理 Release 失败"
            );
        }
    }
    Ok(())
}

fn process_release(cli: &Cli, controller_key: &[u8], entry: &fs::DirEntry) -> anyhow::Result<()> {
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
    let repository_keyring = if authorization.include_repository_keyring {
        let artifact = create_repository_keyring_package(cli, &authorization, &staging)?;
        let path = staging.join(&artifact.path);
        gpg_sign(cli, &path)?;
        package_paths.push(path);
        Some(artifact)
    } else {
        None
    };
    let mut evidence_files = Vec::new();
    for evidence in &authorization.evidence_files {
        let source = entry.path().join(&evidence.path);
        let destination = staging.join(&evidence.path);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, &destination)?;
        evidence_files.push(file_entry_with_path(&destination, evidence.path.clone())?);
    }
    package_paths.sort();
    let database = staging.join(format!("{}.db.tar.gz", authorization.repository_name));
    let files_database = staging.join(format!("{}.files.tar.gz", authorization.repository_name));
    if package_paths.is_empty() {
        create_empty_repository_database(&database)?;
        create_empty_repository_database(&files_database)?;
    } else {
        let mut repo_arguments = vec![database.as_os_str().to_owned()];
        repo_arguments.extend(package_paths.iter().map(|path| path.as_os_str().to_owned()));
        run_checked("/usr/bin/repo-add", &repo_arguments)?;
    }
    gpg_sign(cli, &database)?;
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
    let authorization_destination = staging.join("authorization.json");
    fs::copy(&authorization_path, &authorization_destination)?;
    let manifest = ReleaseManifest {
        release_id: authorization.release_id,
        batch_id: authorization.batch_id,
        source_git_commit: authorization.source_git_commit,
        repository_name: authorization.repository_name,
        writer_epoch: authorization.writer_epoch,
        artifacts: authorization.artifacts,
        evidence_files,
        removed_package_names: authorization.removed_package_names,
        repository_keyring,
        repository_database: file_entry(&database)?,
        repository_files: file_entry(&files_database)?,
        artifact_inspections: Some(file_entry(&inspection_destination)?),
        release_authorization: Some(file_entry(&authorization_destination)?),
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

fn create_repository_keyring_package(
    cli: &Cli,
    authorization: &ReleaseAuthorization,
    staging: &Path,
) -> anyhow::Result<ArtifactRecord> {
    let fingerprint = repository_fingerprint(cli)?;
    let source_commit = authorization
        .source_git_commit
        .chars()
        .filter(|value| value.is_ascii_alphanumeric())
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let source_commit = if source_commit.is_empty() {
        "unknown".to_owned()
    } else {
        source_commit
    };
    let package_version = format!(
        "{}.{}.{}-1",
        authorization.issued_at.format("%Y%m%d.%H%M%S"),
        source_commit,
        authorization
            .release_id
            .simple()
            .to_string()
            .chars()
            .take(8)
            .collect::<String>()
    );
    let pkgver = package_version
        .strip_suffix("-1")
        .context("keyring package version 无效")?;
    let directory = tempfile::Builder::new()
        .prefix("aursmith-keyring-")
        .tempdir_in("/tmp")?;
    let root = directory.path();
    let public_key = root.join("aursmith.gpg");
    run_checked(
        "/usr/bin/gpg",
        &[
            "--homedir".into(),
            cli.gpg_home.as_os_str().into(),
            "--batch".into(),
            "--yes".into(),
            "--output".into(),
            public_key.as_os_str().into(),
            "--export".into(),
            fingerprint.clone().into(),
        ],
    )?;
    if fs::metadata(&public_key)?.len() == 0 {
        bail!("仓库 GPG 公钥导出为空");
    }
    fs::write(root.join("aursmith-trusted"), format!("{fingerprint}:4:\n"))?;
    fs::write(root.join("aursmith-revoked"), b"")?;
    fs::write(
        root.join("aursmith-keyring.install"),
        b"#!/bin/sh\n\npopulate_aursmith() {\n\tif usr/bin/pacman-key -l >/dev/null 2>&1; then\n\t\tusr/bin/pacman-key --populate aursmith\n\tfi\n}\n\npost_upgrade() {\n\tpopulate_aursmith\n}\n\npost_install() {\n\tif [ -x usr/bin/pacman-key ]; then\n\t\tpopulate_aursmith\n\tfi\n}\n",
    )?;
    let checksums = [
        digest_file(&public_key)?,
        digest_file(&root.join("aursmith-trusted"))?,
        digest_file(&root.join("aursmith-revoked"))?,
    ];
    fs::write(
        root.join("PKGBUILD"),
        format!(
            "pkgname=aursmith-keyring\npkgver={pkgver}\npkgrel=1\npkgdesc='AURsmith repository signing keys'\narch=('any')\nurl='https://desktop.shgao.top:8443'\nlicense=('Apache-2.0')\ndepends=('pacman')\ninstall=aursmith-keyring.install\noptions=('!strip' '!debug')\nsource=('aursmith.gpg' 'aursmith-trusted' 'aursmith-revoked')\nsha256sums=('{}' '{}' '{}')\n\npackage() {{\n  install -Dm644 aursmith.gpg \"$pkgdir/usr/share/pacman/keyrings/aursmith.gpg\"\n  install -Dm644 aursmith-trusted \"$pkgdir/usr/share/pacman/keyrings/aursmith-trusted\"\n  install -Dm644 aursmith-revoked \"$pkgdir/usr/share/pacman/keyrings/aursmith-revoked\"\n}}\n",
            checksums[0], checksums[1], checksums[2]
        ),
    )?;
    let package_output = root.join("packages");
    fs::create_dir_all(&package_output)?;
    let status = Command::new("/usr/bin/makepkg")
        .args(["--noconfirm", "--force", "--nodeps", "--cleanbuild"])
        .current_dir(root)
        .env("SOURCE_DATE_EPOCH", "946684800")
        .env("PKGDEST", &package_output)
        .env("SRCDEST", root)
        .env("BUILDDIR", root.join("build"))
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("aursmith-keyring makepkg 失败，状态 {status}");
    }
    let packages = fs::read_dir(&package_output)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry.file_name().to_string_lossy().contains(".pkg.tar.")
        })
        .collect::<Vec<_>>();
    if packages.len() != 1 {
        bail!("aursmith-keyring 产物数量不是 1");
    }
    let source = packages[0].path();
    let file_name = packages[0].file_name().to_string_lossy().into_owned();
    let destination = staging.join(&file_name);
    fs::copy(&source, &destination)?;
    let artifact = ArtifactRecord {
        path: file_name,
        sha256: digest_file(&destination)?,
        size: fs::metadata(&destination)?.len(),
        package_name: Some("aursmith-keyring".into()),
        package_version: Some(package_version),
        architecture: Some("any".into()),
    };
    validate_package_metadata(&destination, &artifact)?;
    Ok(artifact)
}

fn repository_fingerprint(cli: &Cli) -> anyhow::Result<String> {
    let output = Command::new("/usr/bin/gpg")
        .args(["--homedir"])
        .arg(&cli.gpg_home)
        .args(["--batch", "--with-colons", "--fingerprint"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("无法读取仓库 GPG 指纹");
    }
    String::from_utf8(output.stdout)?
        .lines()
        .filter_map(|line| line.split(':').nth(9))
        .find(|value| {
            value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .context("仓库私钥没有有效主指纹")
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
    if authorization.artifacts.is_empty() && authorization.removed_package_names.is_empty() {
        bail!("Release 既不包含软件包，也没有清除操作");
    }
    let mut artifact_paths = std::collections::BTreeSet::new();
    let mut package_names = std::collections::BTreeSet::new();
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
        package_names.insert(artifact.package_name.clone().unwrap_or_default());
    }
    if authorization.include_repository_keyring && package_names.contains("aursmith-keyring") {
        bail!("aursmith-keyring 是 Signer 生成的保留包名");
    }
    if authorization.evidence_files.len() > 4096 {
        bail!("Release 证据文件数量超过上限");
    }
    for evidence in &authorization.evidence_files {
        aursmith_protocol::validate_relative_path(&evidence.path)?;
        if !evidence.path.starts_with("evidence/") || !artifact_paths.insert(evidence.path.clone())
        {
            bail!("Release 证据文件路径无效：{}", evidence.path);
        }
        let path = root.join(&evidence.path);
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.len() != evidence.size
            || digest_file(&path)? != evidence.sha256
        {
            bail!("Release 证据文件 Manifest 不匹配：{}", evidence.path);
        }
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

fn create_empty_repository_database(path: &Path) -> anyhow::Result<()> {
    run_checked(
        "/usr/bin/bsdtar",
        &[
            "-czf".into(),
            path.as_os_str().into(),
            "--format=gnutar".into(),
            "-T".into(),
            "/dev/null".into(),
        ],
    )
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

fn file_entry_with_path(path: &Path, manifest_path: String) -> anyhow::Result<ManifestEntry> {
    Ok(ManifestEntry {
        path: manifest_path,
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
    fn bad_release_does_not_abort_the_inbox_scan() {
        let root = tempfile::tempdir().unwrap();
        let inbox = root.path().join("inbox");
        let output = root.path().join("output");
        fs::create_dir_all(inbox.join("bad-release")).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(
            inbox.join("bad-release/authorization.json"),
            b"not valid json",
        )
        .unwrap();
        let cli = Cli {
            inbox,
            output,
            controller_key_hex: "00".repeat(32),
            gpg_private_key: root.path().join("unused"),
            gpg_home: root.path().join("gnupg"),
        };

        assert!(process_pending(&cli, &[0; 32]).is_ok());
    }

    #[test]
    fn authorization_accepts_package_without_transferred_evidence() {
        let root = tempfile::tempdir().unwrap();
        let package_root = tempfile::tempdir().unwrap();
        fs::write(
            package_root.path().join(".PKGINFO"),
            "pkgname = fixture\npkgver = 1-1\narch = any\n",
        )
        .unwrap();
        let package = root.path().join("fixture-1-1-any.pkg.tar.zst");
        let status = Command::new("/usr/bin/bsdtar")
            .args(["-cf"])
            .arg(&package)
            .arg("-C")
            .arg(package_root.path())
            .arg(".PKGINFO")
            .status()
            .unwrap();
        assert!(status.success());
        let authorization = ReleaseAuthorization {
            release_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            writer_epoch: 1,
            repository_name: "aursmith".into(),
            source_git_commit: "a".repeat(40),
            revision_sha256s: vec!["b".repeat(64)],
            audit_report_sha256s: vec!["c".repeat(64)],
            artifacts: vec![ArtifactRecord {
                path: "fixture-1-1-any.pkg.tar.zst".into(),
                sha256: digest_file(&package).unwrap(),
                size: fs::metadata(&package).unwrap().len(),
                package_name: Some("fixture".into()),
                package_version: Some("1-1".into()),
                architecture: Some("any".into()),
            }],
            evidence_files: vec![],
            removed_package_names: vec![],
            include_repository_keyring: true,
            evidence: Default::default(),
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        };
        assert!(validate_authorization(&authorization, root.path()).is_ok());
    }

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
            evidence_files: vec![],
            removed_package_names: vec![],
            include_repository_keyring: true,
            evidence: Default::default(),
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        };
        assert!(validate_authorization(&authorization, root.path()).is_err());
    }

    #[test]
    fn empty_repository_database_is_a_readable_archive() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("aursmith.db.tar.gz");
        create_empty_repository_database(&database).unwrap();
        let status = Command::new("/usr/bin/bsdtar")
            .args(["-tf"])
            .arg(&database)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(fs::metadata(database).unwrap().len() > 0);
    }

    #[test]
    fn repository_keyring_is_a_real_arch_package() {
        let root = tempfile::tempdir().unwrap();
        let gpg_home = root.path().join("gnupg");
        let staging = root.path().join("staging");
        fs::create_dir_all(&gpg_home).unwrap();
        fs::create_dir_all(&staging).unwrap();
        let status = Command::new("/usr/bin/gpg")
            .args([
                "--homedir",
                gpg_home.to_str().unwrap(),
                "--batch",
                "--passphrase",
                "",
                "--quick-gen-key",
                "AURsmith Keyring Test <test@aursmith.invalid>",
                "ed25519",
                "sign",
                "0",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let cli = Cli {
            inbox: root.path().join("inbox"),
            output: root.path().join("output"),
            controller_key_hex: "00".repeat(32),
            gpg_private_key: root.path().join("unused"),
            gpg_home,
        };
        let authorization = ReleaseAuthorization {
            release_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            writer_epoch: 1,
            repository_name: "aursmith".into(),
            source_git_commit: "abcdef1234567890".into(),
            revision_sha256s: vec![],
            audit_report_sha256s: vec![],
            artifacts: vec![],
            evidence_files: vec![],
            removed_package_names: vec!["old-package".into()],
            include_repository_keyring: true,
            evidence: Default::default(),
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        };
        let artifact = create_repository_keyring_package(&cli, &authorization, &staging).unwrap();
        assert_eq!(artifact.package_name.as_deref(), Some("aursmith-keyring"));
        assert_eq!(artifact.architecture.as_deref(), Some("any"));
        let package = staging.join(&artifact.path);
        validate_package_metadata(&package, &artifact).unwrap();
        let entries = Command::new("/usr/bin/bsdtar")
            .args(["-tf"])
            .arg(package)
            .output()
            .unwrap();
        assert!(entries.status.success());
        let entries = String::from_utf8(entries.stdout).unwrap();
        for expected in [
            "usr/share/pacman/keyrings/aursmith.gpg",
            "usr/share/pacman/keyrings/aursmith-trusted",
            "usr/share/pacman/keyrings/aursmith-revoked",
        ] {
            assert!(entries.lines().any(|entry| entry == expected));
        }
    }
}
