use anyhow::{Context, bail};
use aursmith_protocol::ArtifactRecord;
use serde::Serialize;
use std::{collections::BTreeSet, path::Path, process::Command};

const MAXIMUM_ARCHIVE_ENTRIES: usize = 500_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PackageInspection {
    pub artifact_sha256: String,
    pub entry_count: usize,
    pub install_scripts: Vec<String>,
    pub pacman_hooks: Vec<String>,
    pub systemd_units: Vec<String>,
    pub setuid_or_setgid: Vec<String>,
    pub kernel_modules: Vec<String>,
}

pub fn inspect_package(
    path: &Path,
    artifact: &ArtifactRecord,
) -> anyhow::Result<PackageInspection> {
    let paths = run_bsdtar(path, &["-tf"])?;
    let verbose = run_bsdtar(path, &["-tvf"])?;
    let modes = verbose
        .lines()
        .map(|line| {
            line.split_whitespace()
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect::<Vec<_>>();
    let paths = paths.lines().map(str::to_owned).collect::<Vec<_>>();
    let inspection = inspect_listing(&paths, &modes, &artifact.sha256)?;
    validate_pkginfo(path, artifact)?;
    Ok(inspection)
}

fn run_bsdtar(path: &Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("/usr/bin/bsdtar")
        .args(arguments)
        .arg(path)
        .output()?;
    if !output.status.success() {
        bail!("无法读取 Arch 软件包归档，状态 {}", output.status);
    }
    String::from_utf8(output.stdout).context("软件包归档清单不是 UTF-8")
}

fn validate_pkginfo(path: &Path, artifact: &ArtifactRecord) -> anyhow::Result<()> {
    let output = Command::new("/usr/bin/bsdtar")
        .args(["-xOf"])
        .arg(path)
        .arg(".PKGINFO")
        .output()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        bail!("软件包缺少有效或有界的 .PKGINFO");
    }
    let text = String::from_utf8(output.stdout)?;
    for (field, expected) in [
        ("pkgname", artifact.package_name.as_deref()),
        ("pkgver", artifact.package_version.as_deref()),
        ("arch", artifact.architecture.as_deref()),
    ] {
        let values = text
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .filter_map(|(name, value)| (name == field).then_some(value))
            .collect::<Vec<_>>();
        if values.len() != 1 || expected != values.first().copied() {
            bail!("Publisher 复验软件包元数据失败：{field}");
        }
    }
    Ok(())
}

fn inspect_listing(
    paths: &[String],
    modes: &[String],
    artifact_sha256: &str,
) -> anyhow::Result<PackageInspection> {
    if paths.is_empty() || paths.len() != modes.len() || paths.len() > MAXIMUM_ARCHIVE_ENTRIES {
        bail!("软件包归档清单为空、过大或与详细清单不一致");
    }
    let mut unique = BTreeSet::new();
    let mut metadata = std::collections::BTreeMap::<&str, usize>::new();
    let mut inspection = PackageInspection {
        artifact_sha256: artifact_sha256.to_owned(),
        entry_count: paths.len(),
        install_scripts: Vec::new(),
        pacman_hooks: Vec::new(),
        systemd_units: Vec::new(),
        setuid_or_setgid: Vec::new(),
        kernel_modules: Vec::new(),
    };
    for (raw_path, mode) in paths.iter().zip(modes) {
        let normalized = raw_path
            .strip_prefix("./")
            .unwrap_or(raw_path)
            .trim_end_matches('/');
        if normalized.is_empty()
            || !unique.insert(normalized.to_owned())
            || aursmith_protocol::validate_relative_path(normalized).is_err()
        {
            bail!("软件包包含重复或不安全路径：{raw_path}");
        }
        let kind = mode.chars().next().context("软件包详细清单缺少文件类型")?;
        if !matches!(kind, '-' | 'd' | 'l' | 'h') {
            bail!("软件包包含设备、FIFO 或 Socket 等特殊文件：{normalized}");
        }
        for name in [".PKGINFO", ".BUILDINFO", ".MTREE"] {
            if normalized == name {
                *metadata.entry(name).or_default() += 1;
            }
        }
        if normalized == ".INSTALL" {
            inspection.install_scripts.push(normalized.to_owned());
        }
        if normalized.starts_with("usr/share/libalpm/hooks/") {
            inspection.pacman_hooks.push(normalized.to_owned());
        }
        if normalized.starts_with("usr/lib/systemd/system/")
            || normalized.starts_with("usr/lib/systemd/user/")
        {
            inspection.systemd_units.push(normalized.to_owned());
        }
        if mode
            .chars()
            .nth(3)
            .is_some_and(|value| matches!(value, 's' | 'S'))
            || mode
                .chars()
                .nth(6)
                .is_some_and(|value| matches!(value, 's' | 'S'))
        {
            inspection.setuid_or_setgid.push(normalized.to_owned());
        }
        if normalized.ends_with(".ko")
            || normalized.ends_with(".ko.gz")
            || normalized.ends_with(".ko.xz")
            || normalized.ends_with(".ko.zst")
        {
            inspection.kernel_modules.push(normalized.to_owned());
        }
    }
    if [".PKGINFO", ".BUILDINFO", ".MTREE"]
        .iter()
        .any(|name| metadata.get(name).copied() != Some(1))
    {
        bail!("软件包必须各包含一个 .PKGINFO、.BUILDINFO 和 .MTREE");
    }
    Ok(inspection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_reads_a_real_tar_package_fixture() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(".PKGINFO"),
            "pkgname = demo\npkgver = 1-1\narch = any\n",
        )
        .unwrap();
        std::fs::write(root.path().join(".BUILDINFO"), "format = 2\n").unwrap();
        std::fs::write(root.path().join(".MTREE"), "#mtree\n").unwrap();
        let package = root.path().join("demo-1-1-any.pkg.tar");
        let status = Command::new("/usr/bin/bsdtar")
            .current_dir(root.path())
            .args(["-cf"])
            .arg(&package)
            .args([".PKGINFO", ".BUILDINFO", ".MTREE"])
            .status()
            .unwrap();
        assert!(status.success());
        let artifact = ArtifactRecord {
            path: "demo-1-1-any.pkg.tar".into(),
            sha256: "a".repeat(64),
            size: std::fs::metadata(&package).unwrap().len(),
            package_name: Some("demo".into()),
            package_version: Some("1-1".into()),
            architecture: Some("any".into()),
        };
        assert_eq!(inspect_package(&package, &artifact).unwrap().entry_count, 3);
    }

    #[test]
    fn inspection_records_risky_but_legitimate_package_content() {
        let paths = vec![
            ".PKGINFO",
            ".BUILDINFO",
            ".MTREE",
            ".INSTALL",
            "usr/share/libalpm/hooks/demo.hook",
            "usr/lib/systemd/system/demo.service",
            "usr/bin/demo",
            "usr/lib/modules/demo.ko.zst",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let modes = vec![
            "-rw-r--r--",
            "-rw-r--r--",
            "-rw-r--r--",
            "-rw-r--r--",
            "-rw-r--r--",
            "-rw-r--r--",
            "-rwsr-xr-x",
            "-rw-r--r--",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let result = inspect_listing(&paths, &modes, "digest").unwrap();
        assert_eq!(result.install_scripts, [".INSTALL"]);
        assert_eq!(result.pacman_hooks.len(), 1);
        assert_eq!(result.systemd_units.len(), 1);
        assert_eq!(result.setuid_or_setgid, ["usr/bin/demo"]);
        assert_eq!(result.kernel_modules.len(), 1);
    }

    #[test]
    fn inspection_rejects_traversal_devices_and_duplicate_metadata() {
        let required = vec![".PKGINFO".into(), ".BUILDINFO".into(), ".MTREE".into()];
        assert!(
            inspect_listing(
                &[required.clone(), vec!["../escape".into()]].concat(),
                &[vec!["-rw-r--r--".into(); 3], vec!["-rw-r--r--".into()]].concat(),
                "digest"
            )
            .is_err()
        );
        assert!(
            inspect_listing(
                &[required, vec!["dev/node".into()]].concat(),
                &[vec!["-rw-r--r--".into(); 3], vec!["crw-------".into()]].concat(),
                "digest"
            )
            .is_err()
        );
    }
}
