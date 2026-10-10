//! 构建容器入口：`aursmith-builder guest`。
//!
//! 运行在一次性 `aursmith-build` 容器内，没有任何 AURsmith 凭据。输入只读挂载在
//! `/mnt/aursmith-input`，输出写到 `/mnt/aursmith-output`：
//!
//! - 成功：软件包文件 + `guest-result.json`（[`GuestResult`]）+ `build.log`
//! - 失败：`guest-error.json`（[`GuestError`]）+ `build.log`，进程退出码 1

use anyhow::{Context, bail};
use aursmith_core::protocol::{ArtifactRecord, GuestError, GuestResult, GuestSpec};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::symlink,
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const INPUT: &str = "/mnt/aursmith-input";
const OUTPUT: &str = "/mnt/aursmith-output";
const BUILD: &str = "/build";

pub fn main() {
    if let Err(error) = run() {
        let _ = fs::create_dir_all(OUTPUT);
        let report = GuestError {
            code: guest_error_code(&error).to_owned(),
            error: format!("{error:#}"),
        };
        let _ = fs::write(
            format!("{OUTPUT}/guest-error.json"),
            serde_json::to_vec(&report).unwrap_or_default(),
        );
        eprintln!("AURsmith 构建容器失败：{error:#}");
        std::process::exit(1);
    }
}

const GUEST_CODES: [&str; 7] = [
    "GUEST_CHECKSUM_FAILED",
    "GUEST_PGP_FAILED",
    "GUEST_CHECK_FAILED",
    "GUEST_PACKAGE_FAILED",
    "GUEST_OUTPUT_MISMATCH",
    "GUEST_DEPENDENCY_FAILED",
    "BUILD_NETWORK_TRANSIENT",
];

fn guest_error_code(error: &anyhow::Error) -> &'static str {
    let message = format!("{error:#}");
    GUEST_CODES
        .into_iter()
        .find(|code| message.contains(code))
        .unwrap_or("GUEST_BUILD_FAILED")
}

fn run() -> anyhow::Result<()> {
    let spec: GuestSpec = serde_json::from_slice(
        &fs::read(format!("{INPUT}/.aursmith/guest.json")).context("缺少 .aursmith/guest.json")?,
    )
    .context(".aursmith/guest.json 无效")?;
    reset_build_directory()?;
    copy_tree(Path::new(INPUT), Path::new(BUILD), true)?;
    import_declared_pgp_keys(Path::new(BUILD))?;
    disable_debug_packages(Path::new(BUILD))?;
    run_checked("/usr/bin/chown", &["-R", "builder:builder", BUILD])?;
    let artifacts = build(&spec)?;
    fs::write(
        format!("{OUTPUT}/guest-result.json"),
        serde_json::to_vec(&GuestResult {
            build_id: spec.build_id,
            artifacts,
        })?,
    )?;
    run_checked("/usr/bin/sync", &[])?;
    Ok(())
}

fn disable_debug_packages(build: &Path) -> anyhow::Result<()> {
    let mut file = OpenOptions::new()
        .append(true)
        .open(build.join("PKGBUILD"))
        .context("无法打开构建工作副本中的 PKGBUILD")?;
    file.write_all(
        b"\n# AURsmith build policy: do not create undeclared debug split packages.\noptions+=('!debug')\noptions_x86_64+=('!debug')\n",
    )?;
    Ok(())
}

fn import_declared_pgp_keys(build: &Path) -> anyhow::Result<()> {
    let srcinfo = fs::read_to_string(build.join(".SRCINFO")).context("AUR 快照缺少 .SRCINFO")?;
    let fingerprints = declared_pgp_fingerprints(&srcinfo)?;
    if fingerprints.is_empty() {
        return Ok(());
    }
    let log = Path::new(OUTPUT).join("build.log");
    for fingerprint in &fingerprints {
        let mut imported = false;
        for arguments in pgp_key_commands(fingerprint) {
            let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
            if run_as_builder(&references, Some(&log))?.success() {
                imported = true;
                break;
            }
        }
        if !imported {
            let code = if classify_makepkg_failure(&log) == "BUILD_NETWORK_TRANSIENT" {
                "BUILD_NETWORK_TRANSIENT"
            } else {
                "GUEST_PGP_FAILED"
            };
            bail!("{code}: 无法获取声明的公钥 {fingerprint}，详情见 build.log");
        }
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
        let exact = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.strip_prefix("fpr:::::::::"))
            .filter_map(|line| line.strip_suffix(':'))
            .any(|received| received.eq_ignore_ascii_case(fingerprint));
        if !output.status.success() || !exact {
            bail!("GUEST_PGP_FAILED: 导入的密钥与声明指纹不一致");
        }
    }
    Ok(())
}

/// 固定的可信发现地址；取回后仍严格比对 .SRCINFO 的完整指纹。
fn pgp_key_commands(fingerprint: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    if fingerprint == "EF6E286DDA85EA2A4BA7DE684E2C6E8793298290" {
        commands.push(vec![
            "/usr/bin/gpg".into(),
            "--batch".into(),
            "--auto-key-locate".into(),
            "clear,wkd".into(),
            "--locate-external-keys".into(),
            "torbrowser@torproject.org".into(),
        ]);
    }
    for server in ["hkps://keyserver.ubuntu.com", "hkps://keys.openpgp.org"] {
        commands.push(vec![
            "/usr/bin/gpg".into(),
            "--batch".into(),
            "--keyserver-options".into(),
            "timeout=30".into(),
            "--keyserver".into(),
            server.into(),
            "--recv-keys".into(),
            fingerprint.into(),
        ]);
    }
    commands
}

pub fn declared_pgp_fingerprints(srcinfo: &str) -> anyhow::Result<Vec<String>> {
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

fn build(spec: &GuestSpec) -> anyhow::Result<Vec<ArtifactRecord>> {
    let log = Path::new(OUTPUT).join("build.log");
    run_logged("/usr/bin/pacman", &["-Syu", "--noconfirm"], &log).map_err(|_| {
        anyhow::anyhow!(
            "{}: 系统升级失败，详情见 build.log",
            classify_makepkg_failure(&log)
        )
    })?;
    install_dependencies(spec, &log)?;
    let status = run_as_builder(&makepkg_arguments(spec.allow_check), Some(&log))?;
    if !status.success() {
        bail!(
            "{}: makepkg 失败，详情见 build.log",
            classify_makepkg_failure(&log)
        );
    }
    let packages = collect_package_files(Path::new(BUILD))?;
    let mut artifacts = Vec::new();
    for package in packages {
        let name = package
            .file_name()
            .and_then(|value| value.to_str())
            .context("GUEST_OUTPUT_MISMATCH: 软件包文件名不是 UTF-8")?
            .to_owned();
        let destination = Path::new(OUTPUT).join(&name);
        fs::copy(&package, &destination)?;
        let (package_name, package_version, architecture) = read_package_metadata(&destination)?;
        artifacts.push(ArtifactRecord {
            sha256: file_digest(&destination)?,
            size: fs::metadata(&destination)?.len(),
            file: name,
            package_name,
            package_version,
            architecture,
        });
    }
    validate_expected_outputs(&artifacts, &spec.expected_outputs)?;
    Ok(artifacts)
}

/// 同一期望状态中已构建的 AUR 依赖（Builder 已校验摘要）以 `--asdeps` 安装。
fn install_dependencies(spec: &GuestSpec, log: &Path) -> anyhow::Result<()> {
    if spec.dependency_files.is_empty() {
        return Ok(());
    }
    let mut arguments = vec![
        "-U".to_owned(),
        "--noconfirm".into(),
        "--needed".into(),
        "--asdeps".into(),
    ];
    for file in &spec.dependency_files {
        aursmith_core::names::validate_artifact_file_name(file)
            .map_err(|error| anyhow::anyhow!("GUEST_DEPENDENCY_FAILED: {error}"))?;
        arguments.push(format!("{INPUT}/.aursmith/deps/{file}"));
    }
    let references = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    run_logged("/usr/bin/pacman", &references, log)
        .map_err(|_| anyhow::anyhow!("GUEST_DEPENDENCY_FAILED: AUR 依赖安装失败，详情见 build.log"))
}

pub fn makepkg_arguments(allow_check: bool) -> Vec<&'static str> {
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
    classify_makepkg_failure_text(
        &String::from_utf8_lossy(&fs::read(log).unwrap_or_default()).to_ascii_lowercase(),
    )
}

pub fn classify_makepkg_failure_text(text: &str) -> &'static str {
    let text = text.to_ascii_lowercase();
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
        || contains_transient_http_status(&text)
    {
        "BUILD_NETWORK_TRANSIENT"
    } else if text.contains("did not pass the validity check") || text.contains("checksum") {
        "GUEST_CHECKSUM_FAILED"
    } else if [
        "unknown public key",
        "pgp signatures could not be verified",
        "signature verification failed",
        "bad signature",
        "signature is unknown trust",
        "invalid or corrupted package (pgp signature)",
    ]
    .iter()
    .any(|pattern| text.contains(pattern))
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

fn builder_command_arguments<'a>(arguments: &[&'a str]) -> Vec<&'a str> {
    let mut command = vec!["-u", "builder", "--"];
    command.extend_from_slice(arguments);
    command
}

pub fn validate_expected_outputs(
    artifacts: &[ArtifactRecord],
    expected_outputs: &[String],
) -> anyhow::Result<()> {
    let actual = artifacts
        .iter()
        .map(|artifact| artifact.package_name.clone())
        .collect::<BTreeSet<_>>();
    let expected = expected_outputs.iter().cloned().collect::<BTreeSet<_>>();
    if artifacts.is_empty() || actual != expected {
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
    parse_pkginfo(&String::from_utf8(output.stdout)?)
}

pub fn parse_pkginfo(text: &str) -> anyhow::Result<(String, String, String)> {
    let value = |name: &str| {
        text.lines()
            .filter_map(|line| line.split_once(" = "))
            .find_map(|(key, value)| (key == name).then(|| value.to_owned()))
            .with_context(|| format!("GUEST_OUTPUT_MISMATCH: .PKGINFO 缺少 {name}"))
    };
    Ok((value("pkgname")?, value("pkgver")?, value("arch")?))
}

fn run_logged(executable: &str, arguments: &[&str], log: &Path) -> anyhow::Result<()> {
    let stdout = OpenOptions::new().create(true).append(true).open(log)?;
    let stderr = stdout.try_clone()?;
    let status = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()?;
    if !status.success() {
        bail!("{executable} 失败，状态 {status}");
    }
    Ok(())
}

/// 以 builder 用户运行；长时间无日志输出时写入心跳行，便于排查卡死。
fn run_as_builder(
    arguments: &[&str],
    log: Option<&Path>,
) -> anyhow::Result<std::process::ExitStatus> {
    let mut command = Command::new("/usr/bin/runuser");
    command
        .args(builder_command_arguments(arguments))
        .current_dir(BUILD)
        .stdin(Stdio::null())
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
        let size = fs::metadata(path).ok().map(|value| value.len());
        if size != last_size {
            last_size = size;
            last_progress = Instant::now();
        } else if last_progress.elapsed() >= Duration::from_secs(120) {
            let mut diagnostics = OpenOptions::new().append(true).open(path)?;
            diagnostics.write_all(b"\n==> AURsmith: 120 seconds without log progress\n")?;
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

/// 复制输入树（跳过 `.aursmith` 控制目录）；拒绝越界符号链接与特殊文件。
pub fn copy_tree(source: &Path, destination: &Path, skip_control: bool) -> anyhow::Result<()> {
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

pub fn validate_relative_link(link: &Path) -> anyhow::Result<()> {
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
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

fn run_checked(executable: &str, arguments: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(executable)
        .args(arguments)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        bail!("{executable} 失败，状态 {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tor_uses_wkd_before_keyservers_and_other_keys_use_fallback() {
        let tor = pgp_key_commands("EF6E286DDA85EA2A4BA7DE684E2C6E8793298290");
        assert_eq!(tor.len(), 3);
        assert!(tor[0].iter().any(|arg| arg == "torbrowser@torproject.org"));
        let other = pgp_key_commands("20EE325B86A81BCBD3E56798F04367096FBA95E8");
        assert_eq!(other.len(), 2);
        assert!(other[1].iter().any(|arg| arg == "hkps://keys.openpgp.org"));
        assert!(
            other
                .iter()
                .all(|cmd| cmd.last().unwrap() == "20EE325B86A81BCBD3E56798F04367096FBA95E8")
        );
    }

    #[test]
    fn makepkg_arguments_respect_check_policy() {
        assert!(makepkg_arguments(true).contains(&"--syncdeps"));
        assert!(!makepkg_arguments(true).contains(&"--nocheck"));
        assert!(makepkg_arguments(false).contains(&"--nocheck"));
        assert_eq!(
            builder_command_arguments(&["/usr/bin/makepkg"])[..3],
            ["-u", "builder", "--"]
        );
    }

    #[test]
    fn failure_codes_are_extracted_from_errors() {
        assert_eq!(
            guest_error_code(&anyhow::anyhow!("GUEST_CHECKSUM_FAILED: bad")),
            "GUEST_CHECKSUM_FAILED"
        );
        assert_eq!(
            guest_error_code(&anyhow::anyhow!("something else")),
            "GUEST_BUILD_FAILED"
        );
    }

    #[test]
    fn transient_http_failures_are_retryable_but_404_is_not() {
        assert_eq!(
            classify_makepkg_failure_text("curl: (22) The requested URL returned error: 429"),
            "BUILD_NETWORK_TRANSIENT"
        );
        assert_eq!(
            classify_makepkg_failure_text("curl: (22) The requested URL returned error: 404"),
            "GUEST_BUILD_FAILED"
        );
        assert_eq!(
            classify_makepkg_failure_text(
                "==> ERROR: One or more PGP signatures could not be verified!"
            ),
            "GUEST_PGP_FAILED"
        );
        assert_eq!(
            classify_makepkg_failure_text(
                "gpg: public key ABC is newer than the signature\n-- Performing Test FEATURE - Failed"
            ),
            "GUEST_BUILD_FAILED"
        );
        assert_eq!(
            classify_makepkg_failure_text("==> ERROR: A failure occurred in check()."),
            "GUEST_CHECK_FAILED"
        );
    }

    #[test]
    fn declared_pgp_keys_require_full_fingerprints() {
        let fingerprint = "EF6E286DDA85EA2A4BA7DE684E2C6E8793298290";
        assert_eq!(
            declared_pgp_fingerprints(&format!("\tvalidpgpkeys = {fingerprint}")).unwrap(),
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
    fn copy_tree_skips_control_directory() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::create_dir_all(source.path().join(".aursmith/deps")).unwrap();
        fs::write(source.path().join(".aursmith/guest.json"), b"{}").unwrap();
        fs::write(source.path().join("PKGBUILD"), b"pkgname=demo").unwrap();
        symlink("PKGBUILD", source.path().join("link")).unwrap();
        copy_tree(source.path(), destination.path(), true).unwrap();
        assert!(destination.path().join("PKGBUILD").is_file());
        assert!(destination.path().join("link").is_symlink());
        assert!(!destination.path().join(".aursmith").exists());
        symlink("/etc/passwd", source.path().join("escape")).unwrap();
        assert!(copy_tree(source.path(), tempfile::tempdir().unwrap().path(), true).is_err());
    }

    #[test]
    fn outputs_and_pkginfo_are_exact() {
        let (name, version, arch) =
            parse_pkginfo("pkgname = demo\npkgver = 1:2.0-1\narch = any\n").unwrap();
        assert_eq!(
            (name.as_str(), version.as_str(), arch.as_str()),
            ("demo", "1:2.0-1", "any")
        );
        let artifact = ArtifactRecord {
            file: "demo-1:2.0-1-any.pkg.tar.zst".into(),
            sha256: "a".repeat(64),
            size: 1,
            package_name: name,
            package_version: version,
            architecture: arch,
        };
        assert!(
            validate_expected_outputs(std::slice::from_ref(&artifact), &["demo".into()]).is_ok()
        );
        assert!(
            validate_expected_outputs(&[artifact], &["demo".into(), "demo-docs".into()]).is_err()
        );
        assert!(validate_expected_outputs(&[], &[]).is_err());
    }
}
