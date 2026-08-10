use anyhow::{Context, bail};
use aursmith_protocol::{
    ArtifactRecord, AuditSourceFile, BuildResult, DependencySource, FetchResult, GuestResult,
    JobKind, JobSpec, ManifestEntry, ResolvedDependency, SignedEnvelope, SourceEntryKind,
    SourceManifestEntry,
};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    os::unix::fs::symlink,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

const INPUT: &str = "/mnt/aursmith-input";
const OUTPUT: &str = "/mnt/aursmith-output";
const BUILD: &str = "/build";

fn main() {
    if let Err(error) = run() {
        let _ = fs::create_dir_all(OUTPUT);
        let _ = fs::write(
            format!("{OUTPUT}/guest-error.json"),
            serde_json::to_vec(&serde_json::json!({"error": error.to_string()}))
                .unwrap_or_default(),
        );
        eprintln!("AURsmith Guest 失败：{error:#}");
        shutdown();
        std::process::exit(1);
    }
    shutdown();
}

fn run() -> anyhow::Result<()> {
    mount("proc", "/proc", "proc", &[])?;
    mount("sysfs", "/sys", "sysfs", &[])?;
    fs::create_dir_all(INPUT)?;
    fs::create_dir_all(OUTPUT)?;
    mount(
        "aursmith-input",
        INPUT,
        "9p",
        &["-o", "trans=virtio,version=9p2000.L,ro,nodev,nosuid"],
    )?;
    mount(
        "aursmith-output",
        OUTPUT,
        "9p",
        &["-o", "trans=virtio,version=9p2000.L,nodev,nosuid"],
    )?;

    let controller_key = controller_key()?;
    let envelope_bytes =
        fs::read(format!("{INPUT}/.aursmith/job-envelope.json")).context("缺少 Guest JobSpec")?;
    let envelope: SignedEnvelope =
        serde_json::from_slice(&envelope_bytes).context("Guest JobSpec JSON 无效")?;
    if envelope.verifying_key != controller_key {
        bail!("Guest JobSpec Controller 公钥不匹配");
    }
    let spec: JobSpec = envelope.verify("aursmith.job_spec")?;
    if spec.is_expired_at(Utc::now()) {
        bail!("Guest JobSpec 已过期");
    }
    if spec.kind == JobKind::Fetch {
        configure_fetch_network()?;
    }
    reset_build_directory()?;
    copy_tree(Path::new(INPUT), Path::new(BUILD), true)?;
    if spec.kind == JobKind::ProfileFixture {
        fs::write(
            Path::new(BUILD).join("PKGBUILD"),
            b"pkgname=aursmith-profile-fixture\npkgver=1\npkgrel=1\narch=('any')\npackage() { install -Dm644 /usr/lib/os-release \"$pkgdir/usr/share/aursmith-profile-fixture/os-release\"; }\n",
        )?;
    }
    if spec.kind == JobKind::Build {
        install_offline_dependencies(Path::new(BUILD))?;
        apply_published_pkgrel(
            Path::new(BUILD),
            spec.upstream_pkgrel.as_deref(),
            spec.published_pkgrel.as_deref(),
        )?;
    }
    run_checked("/usr/bin/chown", &["-R", "builder:builder", BUILD], None)?;
    let result = match spec.kind {
        JobKind::Fetch => GuestResult::Fetch(fetch(&spec)?),
        JobKind::Build => GuestResult::Build(build(&spec)?),
        JobKind::ProfileFixture => GuestResult::ProfileFixture(build(&spec)?),
    };
    let result_path = format!("{OUTPUT}/build-result.json");
    fs::write(&result_path, serde_json::to_vec(&result)?)?;
    sync_filesystem()?;
    Ok(())
}

fn configure_fetch_network() -> anyhow::Result<()> {
    let interface = fs::read_dir("/sys/class/net")?
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .find(|name| name != "lo")
        .context("Fetch VM 未发现网络接口")?;
    run_checked("/usr/bin/ip", &["link", "set", &interface, "up"], None)?;
    run_checked(
        "/usr/bin/ip",
        &["address", "add", "10.0.2.15/24", "dev", &interface],
        None,
    )?;
    run_checked(
        "/usr/bin/ip",
        &["route", "add", "default", "via", "10.0.2.2"],
        None,
    )?;
    Ok(())
}

fn install_offline_dependencies(build: &Path) -> anyhow::Result<()> {
    let mut packages = Vec::new();
    for directory in [
        build.join(".aursmith-official-dependencies"),
        build.join(".aursmith-batch-dependencies"),
    ] {
        if !directory.exists() {
            continue;
        }
        for item in fs::read_dir(directory)? {
            let path = item?.path();
            if path.is_file()
                && path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(is_package_archive_name)
            {
                packages.push(path);
            }
        }
    }
    if packages.is_empty() {
        return Ok(());
    }
    packages.sort();
    let status = Command::new("/usr/bin/pacman")
        .args(["-U", "--noconfirm", "--needed"])
        .args(packages)
        .stdin(Stdio::null())
        .env_clear()
        .env("PATH", "/usr/bin")
        .status()?;
    if !status.success() {
        bail!("离线依赖安装失败，状态 {status}");
    }
    Ok(())
}

fn apply_published_pkgrel(
    build: &Path,
    upstream_pkgrel: Option<&str>,
    published_pkgrel: Option<&str>,
) -> anyhow::Result<()> {
    let (Some(upstream_pkgrel), Some(published_pkgrel)) = (upstream_pkgrel, published_pkgrel)
    else {
        return Ok(());
    };
    if [upstream_pkgrel, published_pkgrel].iter().any(|pkgrel| {
        pkgrel.is_empty()
            || !pkgrel
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || ".+_".contains(value))
    }) {
        bail!("签名 JobSpec 中的发布 pkgrel 非法");
    }
    let path = build.join("PKGBUILD");
    let source = fs::read_to_string(&path).context("PKGBUILD 不是有效 UTF-8")?;
    let mut replacements = 0_u8;
    let mut rewritten = String::with_capacity(source.len() + published_pkgrel.len());
    for line in source.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = content.trim_start();
        if !content.starts_with(char::is_whitespace)
            && trimmed.starts_with("pkgrel=")
            && !trimmed.starts_with("pkgrel+=")
        {
            replacements = replacements.saturating_add(1);
            let original = trimmed.strip_prefix("pkgrel=").unwrap_or_default().trim();
            let original = original
                .strip_prefix('\'')
                .and_then(|value| value.strip_suffix('\''))
                .or_else(|| {
                    original
                        .strip_prefix('"')
                        .and_then(|value| value.strip_suffix('"'))
                })
                .unwrap_or(original);
            if original != upstream_pkgrel {
                bail!("PKGBUILD pkgrel 与签名 JobSpec 的上游值不一致");
            }
            rewritten.push_str("pkgrel=");
            rewritten.push_str(published_pkgrel);
            if line.ends_with('\n') {
                rewritten.push('\n');
            }
        } else {
            rewritten.push_str(line);
        }
    }
    if replacements != 1 {
        bail!("PKGBUILD 必须恰好包含一个顶层 pkgrel 赋值，实际为 {replacements}");
    }
    fs::write(path, rewritten)?;
    Ok(())
}

fn fetch(spec: &JobSpec) -> anyhow::Result<FetchResult> {
    let log = Path::new(OUTPUT).join("fetch.log");
    run_as_builder(
        &["/usr/bin/makepkg", "--verifysource", "--noconfirm"],
        Some(&log),
        true,
    )?;
    let prepared = Path::new(OUTPUT).join("prepared");
    fs::create_dir_all(&prepared)?;
    copy_tree(Path::new(BUILD), &prepared, false)?;
    let dependency_download_started = std::time::Instant::now();
    let resolved_dependencies = download_official_dependencies(spec, &prepared)?;
    let dependency_download_milliseconds =
        u64::try_from(dependency_download_started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let sources = manifest(&prepared, Path::new(OUTPUT))?;
    let source_manifest_sha256 = manifest_digest(&sources)?;
    let audit_files = select_audit_files(&prepared, &sources)?;
    Ok(FetchResult {
        job_id: spec.job_id,
        attempt: spec.attempt.clone(),
        revision_sha256: spec.revision_sha256.clone(),
        source_manifest_sha256,
        sources,
        audit_files,
        resolved_dependencies: resolved_dependencies.clone(),
        dependency_download_milliseconds,
        resolved_pkgver: None,
        dependency_snapshot_sha256: hex::encode(Sha256::digest(serde_json::to_vec(
            &serde_json::json!({"requested": spec.dependencies, "resolved": resolved_dependencies}),
        )?)),
        log_sha256: file_digest(&log)?,
        finished_at: Utc::now(),
    })
}

fn download_official_dependencies(
    spec: &JobSpec,
    prepared: &Path,
) -> anyhow::Result<Vec<ResolvedDependency>> {
    let names = spec
        .dependencies
        .iter()
        .filter(|dependency| dependency.source == DependencySource::Official)
        .map(|dependency| dependency.name.as_str())
        .collect::<Vec<_>>();
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if names.iter().any(|name| {
        name.is_empty()
            || name.starts_with('-')
            || !name
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || "@._+:-".contains(value))
    }) {
        bail!("官方依赖包名包含非法字符");
    }
    let cache = prepared.join(".aursmith-official-dependencies");
    fs::create_dir(&cache)?;
    let mut command = Command::new("/usr/bin/pacman");
    command
        .args(["-Sw", "--noconfirm", "--needed", "--cachedir"])
        .arg(&cache)
        .args(&names)
        .stdin(Stdio::null())
        .env_clear()
        .env("PATH", "/usr/bin")
        .env("http_proxy", "http://10.0.2.100:8080")
        .env("https_proxy", "http://10.0.2.100:8080");
    let status = command.status()?;
    if !status.success() {
        bail!("官方依赖下载失败，状态 {status}");
    }
    let mut resolved = Vec::new();
    for item in fs::read_dir(&cache)? {
        let path = item?.path();
        if !path.is_file()
            || !path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(is_package_archive_name)
        {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let output = Command::new("/usr/bin/bsdtar")
            .args(["-xOf"])
            .arg(&path)
            .arg(".PKGINFO")
            .stdin(Stdio::null())
            .env_clear()
            .env("PATH", "/usr/bin")
            .output()?;
        if !output.status.success() {
            bail!("无法读取官方依赖包元数据");
        }
        let pkginfo = String::from_utf8(output.stdout)?;
        let (name, version) = parse_pkginfo_identity(&pkginfo)?;
        resolved.push(ResolvedDependency {
            name,
            version,
            source: DependencySource::Official,
            package: ManifestEntry {
                path: path
                    .strip_prefix(Path::new(OUTPUT))?
                    .to_string_lossy()
                    .into_owned(),
                sha256: file_digest(&path)?,
                size: metadata.len(),
            },
        });
    }
    resolved.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(resolved)
}

fn is_package_archive_name(name: &str) -> bool {
    name.contains(".pkg.tar.") && !name.ends_with(".sig")
}

fn parse_pkginfo_identity(pkginfo: &str) -> anyhow::Result<(String, String)> {
    fn values<'a>(pkginfo: &'a str, key: &str) -> BTreeSet<&'a str> {
        pkginfo
            .lines()
            .filter_map(|line| {
                let (field, value) = line.split_once('=')?;
                (field.trim() == key)
                    .then(|| value.trim())
                    .filter(|value| !value.is_empty())
            })
            .collect()
    }
    let names = values(pkginfo, "pkgname");
    let versions = values(pkginfo, "pkgver");
    if names.len() != 1 || versions.len() != 1 {
        bail!("官方依赖包 .PKGINFO 缺少唯一 pkgname/pkgver");
    }
    Ok((
        names.into_iter().next().unwrap().to_owned(),
        versions.into_iter().next().unwrap().to_owned(),
    ))
}

fn build(spec: &JobSpec) -> anyhow::Result<BuildResult> {
    let log = Path::new(OUTPUT).join("build.log");
    let makepkg_arguments = makepkg_arguments(spec.allow_check);
    run_as_builder(&makepkg_arguments, Some(&log), false)?;
    let packages = collect_package_files(Path::new(BUILD))?;
    if packages.is_empty() {
        bail!("makepkg 未生成软件包");
    }
    let mut artifacts = Vec::new();
    for package in packages {
        let name = package
            .file_name()
            .and_then(|value| value.to_str())
            .context("软件包文件名不是 UTF-8")?;
        let destination = Path::new(OUTPUT).join(name);
        fs::copy(&package, &destination)?;
        let metadata = fs::metadata(&destination)?;
        let package_metadata = read_package_metadata(&destination)?;
        artifacts.push(ArtifactRecord {
            path: name.to_owned(),
            sha256: file_digest(&destination)?,
            size: metadata.len(),
            package_name: Some(package_metadata.0),
            package_version: Some(package_metadata.1),
            architecture: Some(package_metadata.2),
        });
    }
    validate_expected_outputs(&artifacts, &spec.expected_outputs)?;
    let namcap_log = Path::new(OUTPUT).join("namcap.log");
    let mut namcap_arguments = vec!["/usr/bin/namcap"];
    let artifact_paths = artifacts
        .iter()
        .map(|artifact| format!("{OUTPUT}/{}", artifact.path))
        .collect::<Vec<_>>();
    namcap_arguments.extend(artifact_paths.iter().map(String::as_str));
    run_as_builder(&namcap_arguments, Some(&namcap_log), false)?;
    Ok(BuildResult {
        job_id: spec.job_id,
        attempt: spec.attempt.clone(),
        revision_sha256: spec.revision_sha256.clone(),
        source_manifest_sha256: spec
            .source_manifest_sha256
            .clone()
            .context("Build Job 缺少 Source Manifest")?,
        dependency_snapshot_sha256: spec
            .dependency_snapshot_sha256
            .clone()
            .context("Build Job 缺少依赖快照")?,
        profile_sha256: spec
            .profile_sha256
            .clone()
            .context("Build Job 缺少 Profile")?,
        artifacts,
        provenance: [
            ("guest_agent".into(), env!("CARGO_PKG_VERSION").into()),
            ("network".into(), "none".into()),
            (
                "check".into(),
                if spec.allow_check {
                    "enabled"
                } else {
                    "disabled"
                }
                .into(),
            ),
            ("namcap_sha256".into(), file_digest(&namcap_log)?),
            (
                "published_pkgrel".into(),
                spec.published_pkgrel
                    .clone()
                    .unwrap_or_else(|| "upstream".into()),
            ),
        ]
        .into_iter()
        .collect(),
        log_sha256: file_digest(&log)?,
        finished_at: Utc::now(),
    })
}

fn makepkg_arguments(allow_check: bool) -> Vec<&'static str> {
    let mut arguments = vec!["/usr/bin/makepkg", "--noconfirm", "--cleanbuild", "--force"];
    if !allow_check {
        arguments.push("--nocheck");
    }
    arguments
}

fn validate_expected_outputs(
    artifacts: &[ArtifactRecord],
    expected_outputs: &[String],
) -> anyhow::Result<()> {
    let actual = artifacts
        .iter()
        .filter_map(|artifact| artifact.package_name.clone())
        .collect::<BTreeSet<_>>();
    let expected = expected_outputs.iter().cloned().collect::<BTreeSet<_>>();
    if !expected.is_empty() && actual != expected {
        bail!(
            "构建产物与签名 JobSpec 的 split outputs 不一致：预期 {:?}，实际 {:?}",
            expected,
            actual
        );
    }
    Ok(())
}

fn read_package_metadata(path: &Path) -> anyhow::Result<(String, String, String)> {
    let output = Command::new("/usr/bin/bsdtar")
        .args(["-xOf"])
        .arg(path)
        .arg(".PKGINFO")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("无法读取构建产物 .PKGINFO");
    }
    let text = String::from_utf8(output.stdout)?;
    let value = |name: &str| {
        text.lines()
            .filter_map(|line| line.split_once(" = "))
            .find_map(|(key, value)| (key == name).then_some(value.to_owned()))
            .with_context(|| format!(".PKGINFO 缺少 {name}"))
    };
    Ok((value("pkgname")?, value("pkgver")?, value("arch")?))
}

fn run_as_builder(arguments: &[&str], log: Option<&Path>, network: bool) -> anyhow::Result<()> {
    let mut command = Command::new("/usr/bin/runuser");
    command.args(["-u", "builder", "--"]).args(arguments);
    command.current_dir(BUILD).stdin(Stdio::null());
    command
        .env_clear()
        .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/bin")
        .env("HOME", "/home/builder")
        .env("LANG", "C.UTF-8");
    if network {
        command
            .env("http_proxy", "http://10.0.2.100:8080")
            .env("https_proxy", "http://10.0.2.100:8080");
    }
    if let Some(path) = log {
        let stdout = File::create(path)?;
        let stderr = stdout.try_clone()?;
        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
    }
    let status = command.status()?;
    if !status.success() {
        bail!("Guest 命令失败，状态 {status}");
    }
    Ok(())
}

fn controller_key() -> anyhow::Result<Vec<u8>> {
    let command_line = fs::read_to_string("/proc/cmdline")?;
    let value = command_line
        .split_whitespace()
        .find_map(|part| part.strip_prefix("aursmith.controller_key="))
        .context("内核参数缺少 Controller 公钥")?;
    let bytes = hex::decode(value)?;
    if bytes.len() != 32 {
        bail!("Controller 公钥长度无效");
    }
    Ok(bytes)
}

fn reset_build_directory() -> anyhow::Result<()> {
    if Path::new(BUILD).exists() {
        fs::remove_dir_all(BUILD)?;
    }
    fs::create_dir(BUILD)?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path, skip_control: bool) -> anyhow::Result<()> {
    for item in fs::read_dir(source)? {
        let item = item?;
        let name = item.file_name();
        if skip_control && name == ".aursmith" {
            continue;
        }
        let target = destination.join(&name);
        let metadata = fs::symlink_metadata(item.path())?;
        if metadata.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&item.path(), &target, false)?;
        } else if metadata.is_file() {
            fs::copy(item.path(), target)?;
        } else if metadata.file_type().is_symlink() {
            let link = fs::read_link(item.path())?;
            validate_relative_link(&link)?;
            symlink(link, target)?;
        } else {
            bail!("输入包含不支持的特殊文件");
        }
    }
    Ok(())
}

fn validate_relative_link(link: &Path) -> anyhow::Result<()> {
    if link.is_absolute()
        || link.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("输入符号链接越界");
    }
    Ok(())
}

fn manifest(root: &Path, output_root: &Path) -> anyhow::Result<Vec<SourceManifestEntry>> {
    let mut files = Vec::new();
    collect_manifest(root, output_root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn collect_manifest(
    root: &Path,
    output_root: &Path,
    files: &mut Vec<SourceManifestEntry>,
) -> anyhow::Result<()> {
    for item in fs::read_dir(root)? {
        let item = item?;
        let metadata = fs::symlink_metadata(item.path())?;
        let path = item
            .path()
            .strip_prefix(output_root)?
            .to_string_lossy()
            .into_owned();
        if metadata.is_dir() {
            files.push(SourceManifestEntry {
                path,
                kind: SourceEntryKind::Directory,
                sha256: None,
                size: 0,
                link_target: None,
            });
            collect_manifest(&item.path(), output_root, files)?;
        } else if metadata.is_file() {
            files.push(SourceManifestEntry {
                path,
                kind: SourceEntryKind::File,
                sha256: Some(file_digest(&item.path())?),
                size: metadata.len(),
                link_target: None,
            });
        } else if metadata.file_type().is_symlink() {
            files.push(SourceManifestEntry {
                path,
                kind: SourceEntryKind::Symlink,
                sha256: None,
                size: 0,
                link_target: Some(fs::read_link(item.path())?.to_string_lossy().into_owned()),
            });
        } else {
            bail!("源码树包含设备文件或其他特殊文件");
        }
    }
    Ok(())
}

fn manifest_digest(entries: &[SourceManifestEntry]) -> anyhow::Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(entries)?)))
}

fn select_audit_files(
    prepared: &Path,
    entries: &[SourceManifestEntry],
) -> anyhow::Result<Vec<AuditSourceFile>> {
    const MAX_TOTAL: usize = 2 * 1024 * 1024;
    const MAX_FILE: u64 = 256 * 1024;
    let mut remaining = MAX_TOTAL;
    let mut selected = Vec::new();
    for entry in entries {
        if entry.kind != SourceEntryKind::File || entry.size > MAX_FILE {
            continue;
        }
        let lower = entry.path.to_ascii_lowercase();
        let reason = if lower.ends_with("/pkgbuild") || lower == "prepared/pkgbuild" {
            Some("AUR 构建入口")
        } else if lower.ends_with(".sh")
            || lower.ends_with("/makefile")
            || lower.ends_with("/cmakelists.txt")
            || lower.contains("install")
            || lower.contains("systemd")
            || lower.contains("service")
            || lower.contains("hook")
            || lower.contains("network")
            || lower.contains("permission")
            || lower.contains("persist")
        {
            Some("风险相关构建、安装、网络、权限或持久化文件")
        } else {
            None
        };
        let Some(reason) = reason else { continue };
        let relative = Path::new(&entry.path).strip_prefix("prepared")?;
        let path = prepared.join(relative);
        let bytes = fs::read(&path)?;
        if bytes.len() > remaining {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        remaining -= text.len();
        selected.push(AuditSourceFile {
            path: entry.path.clone(),
            sha256: entry.sha256.clone().context("源码文件缺少摘要")?,
            size: entry.size,
            selection_reason: reason.to_owned(),
            text,
        });
    }
    Ok(selected)
}

fn collect_package_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut packages = Vec::new();
    for item in fs::read_dir(root)? {
        let path = item?.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(is_package_archive_name)
        {
            packages.push(path);
        }
    }
    packages.sort();
    Ok(packages)
}

fn file_digest(path: &Path) -> anyhow::Result<String> {
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

fn mount(source: &str, target: &str, kind: &str, extra: &[&str]) -> anyhow::Result<()> {
    let mut arguments = vec!["-t", kind];
    arguments.extend_from_slice(extra);
    arguments.extend([source, target]);
    run_checked("/usr/bin/mount", &arguments, None)
}

fn run_checked(executable: &str, arguments: &[&str], cwd: Option<&Path>) -> anyhow::Result<()> {
    let mut command = Command::new(executable);
    command.args(arguments).stdin(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let status = command.status()?;
    if !status.success() {
        bail!("{executable} 失败，状态 {status}");
    }
    Ok(())
}

fn sync_filesystem() -> anyhow::Result<()> {
    run_checked("/usr/bin/sync", &[], None)
}

fn shutdown() {
    let _ = Command::new("/usr/bin/poweroff").arg("-f").status();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMPORARY_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn temporary_build_directory() -> PathBuf {
        let sequence = TEMPORARY_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "aursmith-guest-test-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn links_cannot_escape_guest_build_directory() {
        assert!(validate_relative_link(Path::new("src/file")).is_ok());
        assert!(validate_relative_link(Path::new("../secret")).is_err());
        assert!(validate_relative_link(Path::new("/etc/shadow")).is_err());
    }

    #[test]
    fn manifest_digest_is_order_sensitive_after_canonical_sorting() {
        let entries = vec![SourceManifestEntry {
            path: "prepared/PKGBUILD".into(),
            kind: SourceEntryKind::File,
            sha256: Some("a".repeat(64)),
            size: 1,
            link_target: None,
        }];
        assert_eq!(manifest_digest(&entries).unwrap().len(), 64);
    }

    #[test]
    fn package_identity_comes_from_unique_pkginfo_fields() {
        let pkginfo = "pkgname = tree\npkgbase = tree\npkgver = 2.3.2-1\n";
        assert_eq!(
            parse_pkginfo_identity(pkginfo).unwrap(),
            ("tree".into(), "2.3.2-1".into())
        );
        assert!(parse_pkginfo_identity("pkgname = tree\n").is_err());
        assert!(parse_pkginfo_identity("pkgname = one\npkgname = two\npkgver = 1\n").is_err());
        assert!(is_package_archive_name("tree-2.3.2-1-x86_64.pkg.tar.zst"));
        assert!(!is_package_archive_name(
            "tree-2.3.2-1-x86_64.pkg.tar.zst.sig"
        ));
    }

    #[test]
    fn published_pkgrel_only_rewrites_the_vm_working_copy() {
        let directory = temporary_build_directory();
        fs::write(
            directory.join("PKGBUILD"),
            "pkgname=demo\npkgver=1.0\npkgrel=1\npackage() { :; }\n",
        )
        .unwrap();
        apply_published_pkgrel(&directory, Some("1"), Some("1.2")).unwrap();
        assert!(
            fs::read_to_string(directory.join("PKGBUILD"))
                .unwrap()
                .contains("pkgrel=1.2\n")
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn ambiguous_or_dynamic_pkgrel_is_rejected() {
        let directory = temporary_build_directory();
        fs::write(
            directory.join("PKGBUILD"),
            "pkgname=demo\npkgver=1.0\npkgrel=$BUILD_NUMBER\npkgrel=2\n",
        )
        .unwrap();
        assert!(apply_published_pkgrel(&directory, Some("1"), Some("1.1")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn single_dynamic_pkgrel_is_rejected() {
        let directory = temporary_build_directory();
        fs::write(
            directory.join("PKGBUILD"),
            "pkgname=demo\npkgver=1.0\npkgrel=$BUILD_NUMBER\n",
        )
        .unwrap();
        assert!(apply_published_pkgrel(&directory, Some("1"), Some("1.1")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn check_policy_is_explicit_and_split_outputs_must_match() {
        assert!(!makepkg_arguments(true).contains(&"--nocheck"));
        assert!(makepkg_arguments(false).contains(&"--nocheck"));
        let artifact = ArtifactRecord {
            path: "demo-1-1-any.pkg.tar.zst".into(),
            sha256: "a".repeat(64),
            size: 1,
            package_name: Some("demo".into()),
            package_version: Some("1-1".into()),
            architecture: Some("any".into()),
        };
        assert!(
            validate_expected_outputs(std::slice::from_ref(&artifact), &["demo".into()]).is_ok()
        );
        assert!(validate_expected_outputs(&[artifact], &["missing-split-output".into()]).is_err());
    }
}
