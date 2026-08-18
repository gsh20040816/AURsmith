//! Publisher 内部的仓库组装、签名和 keyring 生成实现。

use anyhow::{Context, bail};
use aursmith_protocol::{ArtifactRecord, ManifestEntry, ReleaseManifest, ReleasePlan};
use chrono::{Duration as ChronoDuration, Utc};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

#[derive(Debug, Clone)]
struct Cli {
    inbox: PathBuf,
    output: PathBuf,
    repository: PathBuf,
    gpg_private_key: PathBuf,
    gpg_home: PathBuf,
    keyring_refresh_days: i64,
}

fn prepare(cli: &Cli) -> anyhow::Result<()> {
    if !(1..=365).contains(&cli.keyring_refresh_days) {
        bail!("AURSMITH_KEYRING_REFRESH_DAYS 必须在 1 到 365 之间");
    }
    initialize_gpg(cli)
}

#[derive(Clone)]
pub struct PublisherSigning {
    cli: Cli,
}

impl PublisherSigning {
    pub fn new(
        repository: PathBuf,
        gpg_private_key: PathBuf,
        gpg_home: PathBuf,
        keyring_refresh_days: i64,
    ) -> anyhow::Result<Self> {
        let cli = Cli {
            inbox: PathBuf::new(),
            output: PathBuf::new(),
            repository,
            gpg_private_key,
            gpg_home,
            keyring_refresh_days,
        };
        prepare(&cli)?;
        Ok(Self { cli })
    }

    pub fn sign(&self, input: &Path, output: &Path) -> anyhow::Result<()> {
        let mut cli = self.cli.clone();
        cli.inbox = input
            .parent()
            .context("Publisher 签名输入缺少父目录")?
            .to_owned();
        cli.output = output
            .parent()
            .context("Publisher 签名输出缺少父目录")?
            .to_owned();
        if input.file_name() != output.file_name() {
            bail!("Publisher 签名输入与输出 Release ID 不一致");
        }
        process_release_path(&cli, input)
    }

    pub fn keyring_refresh_days(&self) -> i64 {
        self.cli.keyring_refresh_days
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

fn process_release_path(cli: &Cli, input: &Path) -> anyhow::Result<()> {
    let directory_name = input
        .file_name()
        .context("Publisher 签名输入缺少 Release ID")?
        .to_string_lossy();
    if cli.output.join(directory_name.as_ref()).is_dir() {
        return Ok(());
    }
    let plan_path = input.join("release-plan.json");
    let authorization: ReleasePlan = serde_json::from_slice(&fs::read(&plan_path)?)?;
    validate_authorization(cli, &authorization, input)?;
    let release_id = authorization.release_id.to_string();
    if directory_name != release_id {
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
    let mut changed_package_paths = Vec::new();
    for artifact in &authorization.artifacts {
        let source = input.join(&artifact.path);
        if source.is_file() {
            let destination = staging.join(
                Path::new(&artifact.path)
                    .file_name()
                    .context("Artifact 缺少文件名")?,
            );
            fs::copy(source, &destination)?;
            gpg_sign(cli, &destination)?;
            changed_package_paths.push(destination);
        } else {
            find_reusable_package(cli, artifact)?;
        }
    }
    let (repository_keyring, keyring_generation, keyring_fingerprint, keyring_published_at) =
        if authorization.include_repository_keyring {
            let current = current_repository_keyring(cli, &authorization.repository_name)?;
            let fingerprint = repository_fingerprint(cli)?;
            let reusable = current.as_ref().is_some_and(|current| {
                keyring_is_reusable(current, &fingerprint, Utc::now(), cli.keyring_refresh_days)
            });
            let (artifact, generation, published_at, changed) = if reusable {
                let current = current.expect("已经检查当前 keyring");
                reuse_repository_keyring(cli, &staging, &current)?;
                (
                    current.artifact,
                    current.generation,
                    current.published_at,
                    false,
                )
            } else {
                let generation = current
                    .as_ref()
                    .map_or(1, |current| current.generation.saturating_add(1));
                let artifact = create_repository_keyring_package(cli, generation, &staging)?;
                gpg_sign(cli, &staging.join(&artifact.path))?;
                (artifact, generation, Utc::now(), true)
            };
            if changed {
                changed_package_paths.push(staging.join(&artifact.path));
            }
            (
                Some(artifact),
                Some(generation),
                Some(fingerprint),
                Some(published_at),
            )
        } else {
            (None, None, None, None)
        };
    changed_package_paths.sort();
    let database = staging.join(format!("{}.db.tar.gz", authorization.repository_name));
    let files_database = staging.join(format!("{}.files.tar.gz", authorization.repository_name));
    let had_baseline = copy_current_repository_databases(
        cli,
        &authorization.repository_name,
        &database,
        &files_database,
    )?;
    let expected_packages =
        expected_repository_packages(&authorization, repository_keyring.as_ref())?;
    update_repository_databases(
        &database,
        &files_database,
        had_baseline,
        &changed_package_paths,
        &expected_packages,
    )?;
    gpg_sign(cli, &database)?;
    gpg_sign(cli, &files_database)?;
    let manifest = ReleaseManifest {
        release_id: authorization.release_id,
        batch_id: authorization.batch_id,
        source_git_commit: authorization.source_git_commit,
        repository_name: authorization.repository_name,
        artifacts: authorization.artifacts,
        removed_package_names: authorization.removed_package_names,
        repository_keyring,
        keyring_generation,
        keyring_fingerprint,
        keyring_published_at,
        repository_database: file_entry(&database)?,
        repository_files: file_entry(&files_database)?,
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

struct CurrentKeyring {
    artifact: ArtifactRecord,
    generation: u64,
    fingerprint: String,
    published_at: chrono::DateTime<Utc>,
}

fn keyring_is_reusable(
    current: &CurrentKeyring,
    fingerprint: &str,
    now: chrono::DateTime<Utc>,
    refresh_days: i64,
) -> bool {
    current.fingerprint == fingerprint
        && current.published_at >= now - ChronoDuration::days(refresh_days)
}

fn current_repository_keyring(
    cli: &Cli,
    repository_name: &str,
) -> anyhow::Result<Option<CurrentKeyring>> {
    let database_link = cli
        .repository
        .join("x86_64")
        .join(format!("{repository_name}.db"));
    let Ok(target) = fs::read_link(&database_link) else {
        return Ok(None);
    };
    let manifest_path = cli
        .repository
        .join("x86_64")
        .join(target)
        .parent()
        .context("当前仓库数据库链接缺少父目录")?
        .join("release-manifest.json");
    let manifest: ReleaseManifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    let Some(artifact) = manifest.repository_keyring else {
        return Ok(None);
    };
    let (Some(generation), Some(fingerprint), Some(published_at)) = (
        manifest.keyring_generation,
        manifest.keyring_fingerprint,
        manifest.keyring_published_at,
    ) else {
        return Ok(None);
    };
    Ok(Some(CurrentKeyring {
        artifact,
        generation,
        fingerprint,
        published_at,
    }))
}

fn reuse_repository_keyring(
    cli: &Cli,
    staging: &Path,
    current: &CurrentKeyring,
) -> anyhow::Result<()> {
    let package = cli.repository.join("x86_64").join(&current.artifact.path);
    let signature = package.with_file_name(format!("{}.sig", current.artifact.path));
    if !package.is_file()
        || !signature.is_file()
        || digest_file(&package)? != current.artifact.sha256
        || !repository_keyring_matches(cli, &package, &current.fingerprint)?
    {
        bail!("当前 keyring 与 Release Manifest 不一致");
    }
    fs::copy(&package, staging.join(&current.artifact.path))?;
    fs::copy(
        &signature,
        staging.join(format!("{}.sig", current.artifact.path)),
    )?;
    Ok(())
}

fn repository_keyring_matches(
    cli: &Cli,
    package: &Path,
    fingerprint: &str,
) -> anyhow::Result<bool> {
    let public_key = Command::new("/usr/bin/gpg")
        .args(["--homedir"])
        .arg(&cli.gpg_home)
        .args(["--batch", "--export", fingerprint])
        .stdin(Stdio::null())
        .output()?;
    if !public_key.status.success() || public_key.stdout.is_empty() {
        bail!("无法导出仓库 GPG 公钥");
    }
    let expected = [
        ("usr/share/pacman/keyrings/aursmith.gpg", public_key.stdout),
        (
            "usr/share/pacman/keyrings/aursmith-trusted",
            format!("{fingerprint}:4:\n").into_bytes(),
        ),
        ("usr/share/pacman/keyrings/aursmith-revoked", Vec::new()),
    ];
    for (path, expected_content) in expected {
        let actual = Command::new("/usr/bin/bsdtar")
            .args(["-xOf"])
            .arg(package)
            .arg(path)
            .stdin(Stdio::null())
            .output()?;
        if !actual.status.success()
            || actual.stdout.len() > 1024 * 1024
            || actual.stdout != expected_content
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn find_reusable_package(cli: &Cli, artifact: &ArtifactRecord) -> anyhow::Result<PathBuf> {
    let hot = cli.repository.join("x86_64").join(&artifact.path);
    if !hot.is_file()
        || !hot
            .with_file_name(format!("{}.sig", artifact.path))
            .is_file()
    {
        bail!("复用 Artifact 不存在于已提交 hot set：{}", artifact.path);
    }
    let releases = cli.repository.join("x86_64/releases");
    for entry in fs::read_dir(releases)?.filter_map(Result::ok) {
        let manifest_path = entry.path().join("release-manifest.json");
        if !manifest_path.is_file() {
            continue;
        }
        let manifest: ReleaseManifest = serde_json::from_slice(&fs::read(&manifest_path)?)?;
        if manifest
            .artifacts
            .iter()
            .any(|previous| previous == artifact)
        {
            return Ok(hot);
        }
    }
    bail!("复用 Artifact 没有已提交 Release 记录：{}", artifact.path)
}

fn create_repository_keyring_package(
    cli: &Cli,
    generation: u64,
    staging: &Path,
) -> anyhow::Result<ArtifactRecord> {
    let fingerprint = repository_fingerprint(cli)?;
    if generation == 0 {
        bail!("keyring generation 必须大于零");
    }
    let package_version = format!("1:{generation}-1");
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
            "pkgname=aursmith-keyring\nepoch=1\npkgver={generation}\npkgrel=1\npkgdesc='AURsmith repository signing keys'\narch=('any')\nurl='https://desktop.shgao.top:8443'\nlicense=('Apache-2.0')\ndepends=('pacman')\ninstall=aursmith-keyring.install\noptions=('!strip' '!debug')\nsource=('aursmith.gpg' 'aursmith-trusted' 'aursmith-revoked')\nsha256sums=('{}' '{}' '{}')\n\npackage() {{\n  install -Dm644 aursmith.gpg \"$pkgdir/usr/share/pacman/keyrings/aursmith.gpg\"\n  install -Dm644 aursmith-trusted \"$pkgdir/usr/share/pacman/keyrings/aursmith-trusted\"\n  install -Dm644 aursmith-revoked \"$pkgdir/usr/share/pacman/keyrings/aursmith-revoked\"\n}}\n",
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

fn validate_authorization(
    cli: &Cli,
    authorization: &ReleasePlan,
    root: &Path,
) -> anyhow::Result<()> {
    if authorization.expires_at < Utc::now() {
        bail!("ReleasePlan 已过期");
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
        if !path.is_file() {
            find_reusable_package(cli, artifact)?;
            package_names.insert(artifact.package_name.clone().unwrap_or_default());
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file()
            || metadata.len() != artifact.size
            || digest_file(&path)? != artifact.sha256
        {
            bail!("Artifact Manifest 不匹配：{}", artifact.path);
        }
        package_names.insert(artifact.package_name.clone().unwrap_or_default());
    }
    if authorization.include_repository_keyring && package_names.contains("aursmith-keyring") {
        bail!("aursmith-keyring 是 Publisher 生成的保留包名");
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

fn copy_current_repository_databases(
    cli: &Cli,
    repository_name: &str,
    database: &Path,
    files_database: &Path,
) -> anyhow::Result<bool> {
    let root = cli.repository.join("x86_64");
    let current_database = root.join(format!("{repository_name}.db"));
    let current_database_signature = root.join(format!("{repository_name}.db.sig"));
    let current_files = root.join(format!("{repository_name}.files"));
    let current_files_signature = root.join(format!("{repository_name}.files.sig"));
    let present = [
        &current_database,
        &current_database_signature,
        &current_files,
        &current_files_signature,
    ]
    .map(|path| path.is_file());
    if present.iter().all(|value| !value) {
        return Ok(false);
    }
    if !present.iter().all(|value| *value) {
        bail!("当前仓库数据库或签名不完整，拒绝增量发布");
    }
    gpg_verify(cli, &current_database, &current_database_signature)?;
    gpg_verify(cli, &current_files, &current_files_signature)?;
    fs::copy(current_database, database)?;
    fs::copy(current_files, files_database)?;
    Ok(true)
}

fn expected_repository_packages(
    authorization: &ReleasePlan,
    repository_keyring: Option<&ArtifactRecord>,
) -> anyhow::Result<BTreeMap<String, (String, String)>> {
    let mut expected = BTreeMap::new();
    for artifact in authorization.artifacts.iter().chain(repository_keyring) {
        let name = artifact
            .package_name
            .as_ref()
            .context("Release Artifact 缺少包名")?;
        let version = artifact
            .package_version
            .as_ref()
            .context("Release Artifact 缺少版本")?;
        if expected
            .insert(name.clone(), (version.clone(), artifact.path.clone()))
            .is_some()
        {
            bail!("Release 包含重复包名：{name}");
        }
    }
    Ok(expected)
}

fn update_repository_databases(
    database: &Path,
    files_database: &Path,
    had_baseline: bool,
    changed_package_paths: &[PathBuf],
    expected_packages: &BTreeMap<String, (String, String)>,
) -> anyhow::Result<()> {
    if had_baseline {
        let current_packages = read_repository_packages(database)?;
        let removals = current_packages
            .keys()
            .filter(|name| !expected_packages.contains_key(*name))
            .cloned()
            .collect::<Vec<_>>();
        if !removals.is_empty() {
            let mut arguments = vec![database.as_os_str().to_owned()];
            arguments.extend(removals.into_iter().map(Into::into));
            run_checked("/usr/bin/repo-remove", &arguments)?;
        }
    } else if expected_packages.is_empty() {
        create_empty_repository_database(database)?;
        create_empty_repository_database(files_database)?;
    } else if changed_package_paths.is_empty() {
        bail!("仓库没有当前数据库，且 Release 没有可用于初始化的新增软件包");
    }
    if !changed_package_paths.is_empty() {
        let mut arguments = vec![database.as_os_str().to_owned()];
        arguments.extend(
            changed_package_paths
                .iter()
                .map(|path| path.as_os_str().to_owned()),
        );
        run_checked("/usr/bin/repo-add", &arguments)?;
    }
    validate_repository_packages(database, expected_packages)
}

fn read_repository_packages(database: &Path) -> anyhow::Result<BTreeMap<String, (String, String)>> {
    let output = Command::new("/usr/bin/bsdtar")
        .args(["-xOf"])
        .arg(database)
        .args(["--include", "*/desc"])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("无法读取仓库数据库：{}", database.display());
    }
    let text = String::from_utf8(output.stdout)?;
    let mut packages = BTreeMap::new();
    for record in text.split("%FILENAME%\n").skip(1) {
        let file_name = record.lines().next().context("仓库条目缺少文件名")?;
        let name = repository_field(record, "%NAME%")?;
        let version = repository_field(record, "%VERSION%")?;
        if packages
            .insert(name.to_owned(), (version.to_owned(), file_name.to_owned()))
            .is_some()
        {
            bail!("仓库数据库包含重复包名：{name}");
        }
    }
    Ok(packages)
}

fn repository_field<'a>(record: &'a str, field: &str) -> anyhow::Result<&'a str> {
    record
        .split_once(&format!("{field}\n"))
        .and_then(|(_, value)| value.lines().next())
        .filter(|value| !value.is_empty())
        .with_context(|| format!("仓库条目缺少 {field}"))
}

fn validate_repository_packages(
    database: &Path,
    expected: &BTreeMap<String, (String, String)>,
) -> anyhow::Result<()> {
    let actual = read_repository_packages(database)?;
    if &actual != expected {
        let expected_names = expected.keys().cloned().collect::<BTreeSet<_>>();
        let actual_names = actual.keys().cloned().collect::<BTreeSet<_>>();
        bail!(
            "增量仓库数据库与授权清单不一致：缺少 {:?}，多出 {:?}",
            expected_names.difference(&actual_names).collect::<Vec<_>>(),
            actual_names.difference(&expected_names).collect::<Vec<_>>()
        );
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

fn gpg_verify(cli: &Cli, path: &Path, signature: &Path) -> anyhow::Result<()> {
    run_checked(
        "/usr/bin/gpg",
        &[
            "--homedir".into(),
            cli.gpg_home.as_os_str().into(),
            "--batch".into(),
            "--verify".into(),
            signature.as_os_str().into(),
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

    fn test_cli(root: &Path) -> Cli {
        Cli {
            inbox: root.join("inbox"),
            output: root.join("output"),
            repository: root.join("repository"),
            gpg_private_key: root.join("unused"),
            gpg_home: root.join("gnupg"),
            keyring_refresh_days: 30,
        }
    }

    fn fixture_package(root: &Path, name: &str, version: &str) -> PathBuf {
        let source = root.join(format!("source-{name}-{version}"));
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join(".PKGINFO"),
            format!("pkgname = {name}\npkgver = {version}\narch = any\n"),
        )
        .unwrap();
        let package = root.join(format!("{name}-{version}-any.pkg.tar.zst"));
        let status = Command::new("/usr/bin/bsdtar")
            .args(["-cf"])
            .arg(&package)
            .arg("-C")
            .arg(source)
            .arg(".PKGINFO")
            .status()
            .unwrap();
        assert!(status.success());
        package
    }

    #[test]
    fn committed_release_is_skipped_before_reading_stale_plan() {
        let root = tempfile::tempdir().unwrap();
        let release_id = Uuid::new_v4().to_string();
        let inbox = root.path().join("inbox");
        let output = root.path().join("output");
        fs::create_dir_all(inbox.join(&release_id)).unwrap();
        fs::create_dir_all(output.join(&release_id)).unwrap();
        fs::write(
            inbox.join(&release_id).join("release-plan.json"),
            b"stale invalid authorization",
        )
        .unwrap();
        let entry = fs::read_dir(&inbox).unwrap().next().unwrap().unwrap();
        let cli = Cli {
            inbox,
            output,
            repository: root.path().join("repository"),
            gpg_private_key: root.path().join("unused"),
            gpg_home: root.path().join("gnupg"),
            keyring_refresh_days: 30,
        };

        assert!(process_release_path(&cli, &entry.path()).is_ok());
    }

    #[test]
    fn release_plan_accepts_package() {
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
        let authorization = ReleasePlan {
            release_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            repository_name: "aursmith".into(),
            source_git_commit: "a".repeat(40),
            artifacts: vec![ArtifactRecord {
                path: "fixture-1-1-any.pkg.tar.zst".into(),
                sha256: digest_file(&package).unwrap(),
                size: fs::metadata(&package).unwrap().len(),
                package_name: Some("fixture".into()),
                package_version: Some("1-1".into()),
                architecture: Some("any".into()),
            }],
            removed_package_names: vec![],
            include_repository_keyring: true,
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        };
        assert!(
            validate_authorization(&test_cli(root.path()), &authorization, root.path()).is_ok()
        );
    }

    #[test]
    fn release_plan_rejects_traversal_before_running_tools() {
        let root = tempfile::tempdir().unwrap();
        let authorization = ReleasePlan {
            release_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            repository_name: "aursmith".into(),
            source_git_commit: "a".repeat(40),
            artifacts: vec![ArtifactRecord {
                path: "../evil.pkg.tar.zst".into(),
                sha256: "d".repeat(64),
                size: 1,
                package_name: Some("evil".into()),
                package_version: Some("1-1".into()),
                architecture: Some("any".into()),
            }],
            removed_package_names: vec![],
            include_repository_keyring: true,
            issued_at: Utc::now(),
            expires_at: Utc::now() + Duration::minutes(5),
        };
        assert!(
            validate_authorization(&test_cli(root.path()), &authorization, root.path()).is_err()
        );
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
    fn incremental_repository_update_only_adds_changes_and_removes_absent_packages() {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("aursmith.db.tar.gz");
        let files_database = root.path().join("aursmith.files.tar.gz");
        let old = fixture_package(root.path(), "old", "1-1");
        let keep_v1 = fixture_package(root.path(), "keep", "1-1");
        run_checked(
            "/usr/bin/repo-add",
            &[
                database.as_os_str().to_owned(),
                old.as_os_str().to_owned(),
                keep_v1.as_os_str().to_owned(),
            ],
        )
        .unwrap();

        let keep_v2 = fixture_package(root.path(), "keep", "2-1");
        let added = fixture_package(root.path(), "added", "1-1");
        let expected = BTreeMap::from([
            (
                "added".to_owned(),
                (
                    "1-1".to_owned(),
                    added.file_name().unwrap().to_string_lossy().into_owned(),
                ),
            ),
            (
                "keep".to_owned(),
                (
                    "2-1".to_owned(),
                    keep_v2.file_name().unwrap().to_string_lossy().into_owned(),
                ),
            ),
        ]);
        update_repository_databases(
            &database,
            &files_database,
            true,
            &[keep_v2, added],
            &expected,
        )
        .unwrap();

        assert_eq!(read_repository_packages(&database).unwrap(), expected);
        assert!(files_database.is_file());
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
            repository: root.path().join("repository"),
            gpg_private_key: root.path().join("unused"),
            gpg_home,
            keyring_refresh_days: 30,
        };
        let artifact = create_repository_keyring_package(&cli, 7, &staging).unwrap();
        assert_eq!(artifact.package_name.as_deref(), Some("aursmith-keyring"));
        assert_eq!(artifact.package_version.as_deref(), Some("1:7-1"));
        assert_eq!(artifact.architecture.as_deref(), Some("any"));
        let package = staging.join(&artifact.path);
        validate_package_metadata(&package, &artifact).unwrap();
        let fingerprint = repository_fingerprint(&cli).unwrap();
        assert!(repository_keyring_matches(&cli, &package, &fingerprint).unwrap());
        let entries = Command::new("/usr/bin/bsdtar")
            .args(["-tf"])
            .arg(&package)
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

        let repository_root = cli.repository.join("x86_64");
        let release_root = repository_root.join("releases/fixture");
        let reuse_staging = root.path().join("reuse-staging");
        fs::create_dir_all(&release_root).unwrap();
        fs::create_dir_all(&reuse_staging).unwrap();
        fs::copy(&package, repository_root.join(&artifact.path)).unwrap();
        fs::write(
            repository_root.join(format!("{}.sig", artifact.path)),
            b"fixture-signature",
        )
        .unwrap();
        let placeholder = ManifestEntry {
            path: "placeholder".into(),
            sha256: "0".repeat(64),
            size: 0,
        };
        let mut manifest = ReleaseManifest {
            release_id: Uuid::new_v4(),
            batch_id: Uuid::new_v4(),
            source_git_commit: "a".repeat(40),
            repository_name: "aursmith".into(),
            artifacts: vec![],
            removed_package_names: vec![],
            repository_keyring: Some(artifact.clone()),
            keyring_generation: Some(7),
            keyring_fingerprint: Some(fingerprint.clone()),
            keyring_published_at: Some(Utc::now()),
            repository_database: placeholder.clone(),
            repository_files: placeholder,
            committed_at: Utc::now(),
        };
        let manifest_path = release_root.join("release-manifest.json");
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::os::unix::fs::symlink(
            "releases/fixture/aursmith.db.tar.gz",
            repository_root.join("aursmith.db"),
        )
        .unwrap();
        let current = current_repository_keyring(&cli, "aursmith")
            .unwrap()
            .unwrap();
        assert_eq!(current.generation, 7);
        assert!(keyring_is_reusable(&current, &fingerprint, Utc::now(), 30));
        reuse_repository_keyring(&cli, &reuse_staging, &current).unwrap();
        assert!(reuse_staging.join(&artifact.path).is_file());

        manifest.keyring_published_at = Some(Utc::now() - Duration::days(31));
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let expired = current_repository_keyring(&cli, "aursmith")
            .unwrap()
            .unwrap();
        assert!(!keyring_is_reusable(&expired, &fingerprint, Utc::now(), 30));
    }
}
