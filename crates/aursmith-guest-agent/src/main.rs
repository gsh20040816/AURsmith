use anyhow::{Context, bail};
use aursmith_protocol::{ArtifactRecord, BuildResult, GuestResult, JobKind, JobSpec};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::symlink,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const INPUT: &str = "/mnt/aursmith-input";
const OUTPUT: &str = "/mnt/aursmith-output";
const BUILD: &str = "/build";

fn main() {
    if let Err(error) = run() {
        let _ = fs::create_dir_all(OUTPUT);
        let _ = fs::write(
            format!("{OUTPUT}/guest-error.json"),
            serde_json::to_vec(&serde_json::json!({
                "code": guest_error_code(&error),
                "error": error.to_string()
            }))
            .unwrap_or_default(),
        );
        eprintln!("AURsmith Build 容器失败：{error:#}");
        std::process::exit(1);
    }
}

fn guest_error_code(error: &anyhow::Error) -> &'static str {
    let message = error.to_string();
    for code in [
        "GUEST_CHECKSUM_FAILED",
        "GUEST_PGP_FAILED",
        "GUEST_CHECK_FAILED",
        "GUEST_PACKAGE_FAILED",
        "GUEST_OUTPUT_MISMATCH",
        "GUEST_BUILD_FAILED",
        "BUILD_NETWORK_TRANSIENT",
    ] {
        if message.contains(code) {
            return code;
        }
    }
    "GUEST_BUILD_FAILED"
}

fn run() -> anyhow::Result<()> {
    let spec_bytes =
        fs::read(format!("{INPUT}/.aursmith/job-spec.json")).context("缺少 Build JobSpec")?;
    let spec: JobSpec = serde_json::from_slice(&spec_bytes).context("Build JobSpec JSON 无效")?;
    if spec.is_expired_at(Utc::now()) {
        bail!("Build JobSpec 已过期");
    }
    if spec.kind != JobKind::Build {
        bail!("Build 镜像只接受 Build Job");
    }
    reset_build_directory()?;
    copy_tree(Path::new(INPUT), Path::new(BUILD), true)?;
    import_declared_pgp_keys(Path::new(BUILD))?;
    disable_debug_packages(Path::new(BUILD))?;
    run_checked("/usr/bin/chown", &["-R", "builder:builder", BUILD], None)?;
    let result = GuestResult::Build(build(&spec)?);
    fs::write(
        format!("{OUTPUT}/build-result.json"),
        serde_json::to_vec(&result)?,
    )?;
    run_checked("/usr/bin/sync", &[], None)?;
    Ok(())
}

fn disable_debug_packages(build: &Path) -> anyhow::Result<()> {
    let path = build.join("PKGBUILD");
    let mut file = OpenOptions::new()
        .append(true)
        .open(&path)
        .context("无法打开构建工作副本中的 PKGBUILD")?;
    file.write_all(
        b"\n# AURsmith build policy: do not create undeclared debug split packages.\noptions+=('!debug')\noptions_x86_64+=('!debug')\n",
    )?;
    file.flush()?;
    Ok(())
}

fn import_declared_pgp_keys(build: &Path) -> anyhow::Result<()> {
    let srcinfo =
        fs::read_to_string(build.join(".SRCINFO")).context("AUR snapshot 缺少 .SRCINFO")?;
    let fingerprints = declared_pgp_fingerprints(&srcinfo)?;
    if fingerprints.is_empty() {
        return Ok(());
    }
    let mut arguments = vec![
        "/usr/bin/gpg".to_owned(),
        "--batch".to_owned(),
        "--keyserver".to_owned(),
        "hkps://keyserver.ubuntu.com".to_owned(),
        "--recv-keys".to_owned(),
    ];
    arguments.extend(fingerprints.iter().cloned());
    let argument_refs = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    run_as_builder(&argument_refs, None)?;
    verify_declared_pgp_keys(build, &fingerprints)
}

fn verify_declared_pgp_keys(build: &Path, fingerprints: &[String]) -> anyhow::Result<()> {
    for fingerprint in fingerprints {
        let output = Command::new("/usr/bin/runuser")
            .args(builder_command_arguments(&[
                "/usr/bin/gpg",
                "--batch",
                "--with-colons",
                "--fingerprint",
                fingerprint,
            ]))
            .current_dir(build)
            .stdin(Stdio::null())
            .env_clear()
            .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/bin")
            .env("HOME", "/home/builder")
            .env("LANG", "C.UTF-8")
            .output()?;
        let exact_match = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("fpr:::::::::"))
            .filter_map(|line| line.strip_suffix(':'))
            .any(|received| received.eq_ignore_ascii_case(fingerprint));
        if !output.status.success() || !exact_match {
            bail!("GUEST_PGP_FAILED: 导入的密钥与声明指纹不一致");
        }
    }
    Ok(())
}

fn declared_pgp_fingerprints(srcinfo: &str) -> anyhow::Result<Vec<String>> {
    let mut fingerprints = BTreeSet::new();
    for line in srcinfo.lines() {
        let Some((key, value)) = line.trim().split_once('=') else {
            continue;
        };
        if key.trim() != "validpgpkeys" {
            continue;
        }
        let fingerprint = value.trim().to_ascii_uppercase();
        if !matches!(fingerprint.len(), 40 | 64)
            || !fingerprint.chars().all(|value| value.is_ascii_hexdigit())
        {
            bail!("GUEST_PGP_FAILED: .SRCINFO validpgpkeys 必须是完整指纹");
        }
        fingerprints.insert(fingerprint);
    }
    Ok(fingerprints.into_iter().collect())
}

fn build(spec: &JobSpec) -> anyhow::Result<BuildResult> {
    let log = Path::new(OUTPUT).join("build.log");
    install_batch_dependencies(Path::new(BUILD), &log)?;
    let status = run_as_builder_status(&makepkg_arguments(spec.allow_check), Some(&log))?;
    if !status.success() {
        bail!(
            "{}: makepkg 失败，详情见 build.log",
            classify_makepkg_failure(&log)
        );
    }
    let packages = collect_package_files(Path::new(BUILD))?;
    if packages.is_empty() {
        bail!("GUEST_OUTPUT_MISMATCH: makepkg 未生成软件包");
    }
    let mut artifacts = Vec::new();
    for package in packages {
        let name = package
            .file_name()
            .and_then(|value| value.to_str())
            .context("GUEST_OUTPUT_MISMATCH: 软件包文件名不是 UTF-8")?;
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
    Ok(BuildResult {
        job_id: spec.job_id,
        attempt: spec.attempt.clone(),
        revision_sha256: spec.revision_sha256.clone(),
        source_manifest_sha256: spec
            .source_manifest_sha256
            .clone()
            .context("Build Job 缺少输入摘要")?,
        dependency_snapshot_sha256: spec
            .dependency_snapshot_sha256
            .clone()
            .context("Build Job 缺少依赖选择摘要")?,
        artifacts,
        provenance: [
            ("guest_agent".into(), env!("CARGO_PKG_VERSION").into()),
            ("build_image".into(), "aursmith-build:latest".into()),
            ("network".into(), "docker_bridge".into()),
            (
                "check".into(),
                if spec.allow_check {
                    "enabled"
                } else {
                    "disabled"
                }
                .into(),
            ),
        ]
        .into_iter()
        .collect(),
        log_sha256: file_digest(&log)?,
        finished_at: Utc::now(),
    })
}

fn install_batch_dependencies(build: &Path, log: &Path) -> anyhow::Result<()> {
    let directory = build.join(".aursmith-batch-dependencies");
    if !directory.exists() {
        return Ok(());
    }
    let mut packages = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(".pkg.tar.") && !name.ends_with(".sig"))
        })
        .collect::<Vec<_>>();
    packages.sort();
    if packages.is_empty() {
        return Ok(());
    }
    let stdout = OpenOptions::new().create(true).append(true).open(log)?;
    let stderr = stdout.try_clone()?;
    let status = Command::new("/usr/bin/pacman")
        .args(["-U", "--noconfirm", "--needed"])
        .args(packages)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()?;
    if !status.success() {
        bail!(
            "{}: 同批 AUR 依赖安装失败，详情见 build.log",
            classify_makepkg_failure(log)
        );
    }
    Ok(())
}

fn makepkg_arguments(allow_check: bool) -> Vec<&'static str> {
    let mut arguments = vec![
        "/usr/bin/makepkg",
        "--config",
        "/etc/aursmith/makepkg.conf",
        "--syncdeps",
        "--noconfirm",
        "--cleanbuild",
        "--force",
    ];
    if !allow_check {
        arguments.push("--nocheck");
    }
    arguments
}

fn classify_makepkg_failure(log: &Path) -> &'static str {
    let text = String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).to_ascii_lowercase();
    classify_makepkg_failure_text(&text)
}

fn classify_makepkg_failure_text(text: &str) -> &'static str {
    if [
        "could not resolve host",
        "temporary failure in name resolution",
        "network is unreachable",
        "connection timed out",
        "connection reset",
        "failed retrieving file",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
        || contains_transient_http_status(text)
    {
        "BUILD_NETWORK_TRANSIENT"
    } else if text.contains("did not pass the validity check") || text.contains("checksum") {
        "GUEST_CHECKSUM_FAILED"
    } else if text.contains("unknown public key")
        || text.contains("signature") && text.contains("failed")
    {
        "GUEST_PGP_FAILED"
    } else if text.contains("a failure occurred in check()") {
        "GUEST_CHECK_FAILED"
    } else if text.contains("a failure occurred in package") {
        "GUEST_PACKAGE_FAILED"
    } else {
        "GUEST_BUILD_FAILED"
    }
}

fn contains_transient_http_status(text: &str) -> bool {
    [408_u16, 429].into_iter().chain(500..=599).any(|status| {
        [
            format!("returned error: {status}"),
            format!("http error {status}"),
            format!("http status {status}"),
            format!("http/1.1 {status}"),
            format!("http/2 {status}"),
        ]
        .iter()
        .any(|pattern| text.contains(pattern))
    })
}

fn builder_command_arguments<'a>(arguments: &'a [&'a str]) -> Vec<&'a str> {
    let mut command = vec!["-u", "builder", "--"];
    command.extend_from_slice(arguments);
    command
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
        bail!("GUEST_OUTPUT_MISMATCH: 预期 {expected:?}，实际 {actual:?}");
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
        bail!("GUEST_OUTPUT_MISMATCH: 无法读取构建产物 .PKGINFO");
    }
    let text = String::from_utf8(output.stdout)?;
    let value = |name: &str| {
        text.lines()
            .filter_map(|line| line.split_once(" = "))
            .find_map(|(key, value)| (key == name).then_some(value.to_owned()))
            .with_context(|| format!("GUEST_OUTPUT_MISMATCH: .PKGINFO 缺少 {name}"))
    };
    Ok((value("pkgname")?, value("pkgver")?, value("arch")?))
}

fn run_as_builder(arguments: &[&str], log: Option<&Path>) -> anyhow::Result<()> {
    let status = run_as_builder_status(arguments, log)?;
    if !status.success() {
        bail!("Builder 命令失败，状态 {status}");
    }
    Ok(())
}

fn run_as_builder_status(
    arguments: &[&str],
    log: Option<&Path>,
) -> anyhow::Result<std::process::ExitStatus> {
    let mut command = Command::new("/usr/bin/runuser");
    command.args(builder_command_arguments(arguments));
    command.current_dir(BUILD).stdin(Stdio::null());
    command
        .env("PATH", "/usr/local/sbin:/usr/local/bin:/usr/bin")
        .env("HOME", "/home/builder")
        .env("LANG", "C.UTF-8");
    if let Some(path) = log {
        let stdout = OpenOptions::new().create(true).append(true).open(path)?;
        let stderr = stdout.try_clone()?;
        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
    }
    let mut child = command.spawn()?;
    let mut last_size = log.and_then(|path| fs::metadata(path).ok().map(|value| value.len()));
    let mut last_progress = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_secs(1));
        let Some(path) = log else { continue };
        let current_size = fs::metadata(path).ok().map(|value| value.len());
        if current_size != last_size {
            last_size = current_size;
            last_progress = Instant::now();
        } else if last_progress.elapsed() >= Duration::from_secs(120) {
            let mut diagnostics = OpenOptions::new().append(true).open(path)?;
            diagnostics.write_all(b"\n==> AURsmith: 120 seconds without log progress\n")?;
            diagnostics.flush()?;
            last_progress = Instant::now();
        }
    }
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
        || link.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        bail!("输入符号链接越过构建目录");
    }
    Ok(())
}

fn collect_package_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut packages = Vec::new();
    for item in fs::read_dir(root)? {
        let path = item?.path();
        if path.is_file()
            && path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(|name| name.contains(".pkg.tar.") && !name.ends_with(".sig"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn makepkg_uses_standard_dependency_install_and_failure_classes() {
        assert!(makepkg_arguments(true).contains(&"--syncdeps"));
        assert!(!makepkg_arguments(true).contains(&"--nocheck"));
        assert!(makepkg_arguments(false).contains(&"--nocheck"));
        assert!(!builder_command_arguments(&["/usr/bin/makepkg"]).contains(&"/usr/bin/env"));
        assert_eq!(
            guest_error_code(&anyhow::anyhow!("GUEST_CHECKSUM_FAILED: bad source")),
            "GUEST_CHECKSUM_FAILED"
        );
    }

    #[test]
    fn transient_http_download_failures_are_retryable_but_not_not_found() {
        assert_eq!(
            classify_makepkg_failure_text(
                "curl: (22) the requested url returned error: 429\n==> error: failure while downloading"
            ),
            "BUILD_NETWORK_TRANSIENT"
        );
        assert_eq!(
            classify_makepkg_failure_text(
                "curl: (22) the requested url returned error: 404\n==> error: failure while downloading"
            ),
            "GUEST_BUILD_FAILED"
        );
    }

    #[test]
    fn declared_pgp_keys_require_full_fingerprints() {
        let fingerprint = "EF6E286DDA85EA2A4BA7DE684E2C6E8793298290";
        assert_eq!(
            declared_pgp_fingerprints(&format!("validpgpkeys = {fingerprint}")).unwrap(),
            [fingerprint]
        );
        assert!(declared_pgp_fingerprints("validpgpkeys = 93298290").is_err());
    }

    #[test]
    fn links_cannot_escape_build_directory() {
        assert!(validate_relative_link(Path::new("src/file")).is_ok());
        assert!(validate_relative_link(Path::new("../secret")).is_err());
        assert!(validate_relative_link(Path::new("/etc/shadow")).is_err());
    }

    #[test]
    fn split_outputs_are_exact() {
        let artifact = ArtifactRecord {
            path: "demo-1-1-any.pkg.tar.zst".into(),
            sha256: "a".repeat(64),
            size: 1,
            package_name: Some("demo".into()),
            package_version: Some("1-1".into()),
            architecture: Some("any".into()),
        };
        assert!(validate_expected_outputs(&[artifact], &["demo".into()]).is_ok());
    }
}
