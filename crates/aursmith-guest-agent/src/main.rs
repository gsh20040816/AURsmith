use anyhow::{Context, bail};
use aursmith_protocol::{
    ArtifactRecord, AuditSourceFile, BuildResult, DependencySource, FetchResult, GuestResult,
    JobKind, JobSpec, ManifestEntry, ResolvedDependency, SignedEnvelope, SourceEntryKind,
    SourceManifestEntry,
};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::{
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
        "virtiofs",
        &["-o", "ro,nodev,nosuid"],
    )?;
    mount(
        "aursmith-output",
        OUTPUT,
        "virtiofs",
        &["-o", "nodev,nosuid"],
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
                    .is_some_and(|name| name.contains(".pkg.tar."))
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
                .is_some_and(|name| name.contains(".pkg.tar."))
        {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        let output = Command::new("/usr/bin/pacman")
            .args(["-Qp", "--print-format", "%n\t%v"])
            .arg(&path)
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() {
            bail!("无法读取官方依赖包元数据");
        }
        let metadata_line = String::from_utf8(output.stdout)?;
        let (name, version) = metadata_line
            .trim()
            .split_once('\t')
            .context("官方依赖包元数据格式无效")?;
        resolved.push(ResolvedDependency {
            name: name.to_owned(),
            version: version.to_owned(),
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

fn build(spec: &JobSpec) -> anyhow::Result<BuildResult> {
    let log = Path::new(OUTPUT).join("build.log");
    run_as_builder(
        &["/usr/bin/makepkg", "--noconfirm", "--cleanbuild", "--force"],
        Some(&log),
        false,
    )?;
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
            ("check".into(), "enabled".into()),
        ]
        .into_iter()
        .collect(),
        log_sha256: file_digest(&log)?,
        finished_at: Utc::now(),
    })
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
                .is_some_and(|name| name.contains(".pkg.tar."))
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
}
