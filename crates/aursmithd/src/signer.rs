//! 无网络签名器（`aursmithd signer`）。
//!
//! 运行在 `network_mode: none` 的独立容器中：没有网络、没有数据库、不解析任何 AUR 内容。
//! 它只读 `exchange/inbox/<plan_sha256>/`（期望软件包集合 + 新产物），自己校验每个产物的
//! SHA-256 与 `.PKGINFO`，签名、重建仓库数据库、写出签名清单，然后原子切换
//! `current`/`previous`，最后把结果写入 `exchange/outbox/<plan_sha256>.json`。

use anyhow::{Context, bail};
use aursmith_core::{
    is_sha256_hex,
    names::{validate_artifact_file_name, validate_repository_name},
    protocol::{
        ArtifactRecord, FileEntry, PUBLISH_PLAN_FORMAT, PublishManifest, PublishPlan, SignerResult,
        SignerState,
    },
};
use chrono::Utc;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, File},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const KEYRING_NAME: &str = "aursmith-keyring";
const MAXIMUM_PLAN_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Signer {
    pub exchange: PathBuf,
    /// 仓库根目录（其下是 `x86_64/`）。
    pub repository: PathBuf,
    pub gpg_home: PathBuf,
    pub fingerprint: String,
}

impl Signer {
    /// 导入私钥并发布公钥。`gpg_home` 应该是容器内 tmpfs。
    pub fn initialize(
        exchange: PathBuf,
        repository: PathBuf,
        gpg_key: &Path,
        gpg_home: PathBuf,
    ) -> anyhow::Result<Self> {
        fs::create_dir_all(&gpg_home)?;
        let metadata = fs::symlink_metadata(gpg_key).context("无法读取 GPG 私钥文件")?;
        if !metadata.file_type().is_file() || metadata.len() == 0 || metadata.len() > 1024 * 1024 {
            bail!("GPG 私钥文件类型或大小无效");
        }
        run_checked(
            "/usr/bin/gpg",
            &[
                "--homedir".into(),
                gpg_home.as_os_str().into(),
                "--batch".into(),
                "--import".into(),
                gpg_key.as_os_str().into(),
            ],
        )?;
        Self::with_home(exchange, repository, gpg_home)
    }

    pub fn with_home(
        exchange: PathBuf,
        repository: PathBuf,
        gpg_home: PathBuf,
    ) -> anyhow::Result<Self> {
        let fingerprint = repository_fingerprint(&gpg_home)?;
        let signer = Self {
            exchange,
            repository,
            gpg_home,
            fingerprint,
        };
        for directory in [
            signer.inbox(),
            signer.outbox(),
            signer.arch(),
            signer.releases(),
        ] {
            fs::create_dir_all(directory)?;
        }
        signer.publish_public_key()?;
        Ok(signer)
    }

    fn inbox(&self) -> PathBuf {
        self.exchange.join("inbox")
    }
    fn outbox(&self) -> PathBuf {
        self.exchange.join("outbox")
    }
    fn arch(&self) -> PathBuf {
        self.repository.join("x86_64")
    }
    fn releases(&self) -> PathBuf {
        self.arch().join("releases")
    }

    fn publish_public_key(&self) -> anyhow::Result<()> {
        let output = Command::new("/usr/bin/gpg")
            .arg("--homedir")
            .arg(&self.gpg_home)
            .args(["--batch", "--armor", "--export", &self.fingerprint])
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() || output.stdout.is_empty() {
            bail!("无法导出仓库公钥");
        }
        let target = self.arch().join("aursmith-repository-key.asc");
        if fs::read(&target).ok().as_deref() != Some(output.stdout.as_slice()) {
            write_atomic(&target, &output.stdout)?;
        }
        Ok(())
    }

    /// 处理 inbox 中所有还没有结果的计划。返回处理数量。
    pub fn process_inbox(&self) -> anyhow::Result<usize> {
        let mut processed = 0;
        let mut entries: Vec<_> = fs::read_dir(self.inbox())?.filter_map(Result::ok).collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !is_sha256_hex(&name) || !entry.file_type()?.is_dir() {
                continue;
            }
            let outbox = self.outbox().join(format!("{name}.json"));
            if outbox.exists() {
                continue;
            }
            let result = match self.publish(&name) {
                Ok(manifest_sha256) => SignerResult {
                    plan_sha256: name.clone(),
                    state: SignerState::Published,
                    error: None,
                    manifest_sha256: Some(manifest_sha256),
                    keyring_fingerprint: Some(self.fingerprint.clone()),
                    finished_at: Utc::now(),
                },
                Err(error) => {
                    tracing::error!(plan = %name, error = %format!("{error:#}"), "发布失败");
                    SignerResult {
                        plan_sha256: name.clone(),
                        state: SignerState::Failed,
                        error: Some(format!("{error:#}").chars().take(4000).collect()),
                        manifest_sha256: None,
                        keyring_fingerprint: Some(self.fingerprint.clone()),
                        finished_at: Utc::now(),
                    }
                }
            };
            write_atomic(&outbox, &serde_json::to_vec_pretty(&result)?)?;
            processed += 1;
        }
        Ok(processed)
    }

    fn read_plan(&self, plan_sha256: &str) -> anyhow::Result<PublishPlan> {
        let path = self.inbox().join(plan_sha256).join("plan.json");
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.len() > MAXIMUM_PLAN_BYTES {
            bail!("plan.json 类型或大小无效");
        }
        let bytes = fs::read(&path)?;
        if aursmith_core::sha256_hex(&bytes) != plan_sha256 {
            bail!("plan.json 摘要与目录名不一致");
        }
        let plan: PublishPlan = serde_json::from_slice(&bytes)?;
        if plan.canonical_bytes() != bytes {
            bail!("plan.json 不是规范序列化");
        }
        validate_plan(&plan)?;
        Ok(plan)
    }

    /// 发布一个计划；同一计划重复执行是幂等的。返回清单摘要。
    pub fn publish(&self, plan_sha256: &str) -> anyhow::Result<String> {
        let plan = self.read_plan(plan_sha256)?;
        let committed = self.releases().join(plan_sha256);
        if committed.join("manifest.json").is_file() {
            self.activate(plan_sha256)?;
            return file_digest(&committed.join("manifest.json"));
        }
        let staging = self.releases().join(format!(".staging-{plan_sha256}"));
        if staging.exists() {
            fs::remove_dir_all(&staging)?;
        }
        fs::create_dir_all(&staging)?;
        let inbox = self.inbox().join(plan_sha256);
        let mut package_paths = Vec::new();
        let mut new_files = Vec::new();
        for artifact in &plan.artifacts {
            let hot = self.arch().join(&artifact.file);
            let hot_signature = signature_path(&hot);
            let reusable = hot.is_file()
                && hot_signature.is_file()
                && fs::metadata(&hot)?.len() == artifact.size
                && file_digest(&hot)? == artifact.sha256;
            if reusable {
                self.verify(&hot, &hot_signature)?;
                package_paths.push(hot);
                continue;
            }
            let source = inbox.join(&artifact.file);
            let metadata = fs::symlink_metadata(&source)
                .with_context(|| format!("计划要求的新产物不存在：{}", artifact.file))?;
            if !metadata.file_type().is_file()
                || metadata.len() != artifact.size
                || file_digest(&source)? != artifact.sha256
            {
                bail!("产物与计划不一致：{}", artifact.file);
            }
            validate_package_metadata(&source, artifact)?;
            let destination = staging.join(&artifact.file);
            fs::copy(&source, &destination)?;
            self.sign(&destination)?;
            package_paths.push(destination);
            new_files.push(artifact.file.clone());
        }
        let keyring = match self.find_keyring()? {
            Some((artifact, path)) => {
                package_paths.push(path);
                artifact
            }
            None => {
                let artifact = self.build_keyring(&staging)?;
                self.sign(&staging.join(&artifact.file))?;
                package_paths.push(staging.join(&artifact.file));
                new_files.push(artifact.file.clone());
                artifact
            }
        };
        let database = staging.join(format!("{}.db.tar.gz", plan.repository_name));
        let files_database = staging.join(format!("{}.files.tar.gz", plan.repository_name));
        let mut arguments: Vec<OsString> = vec!["--quiet".into(), database.as_os_str().into()];
        arguments.extend(package_paths.iter().map(|path| path.as_os_str().to_owned()));
        run_checked("/usr/bin/repo-add", &arguments)?;
        for link in [
            format!("{}.db", plan.repository_name),
            format!("{}.files", plan.repository_name),
        ] {
            let path = staging.join(link);
            if fs::symlink_metadata(&path).is_ok() {
                fs::remove_file(path)?;
            }
        }
        let mut expected: BTreeMap<String, (String, String)> = plan
            .artifacts
            .iter()
            .map(|artifact| {
                (
                    artifact.package_name.clone(),
                    (artifact.package_version.clone(), artifact.file.clone()),
                )
            })
            .collect();
        expected.insert(
            KEYRING_NAME.into(),
            (keyring.package_version.clone(), keyring.file.clone()),
        );
        let actual = read_repository_packages(&database)?;
        if actual != expected {
            bail!("仓库数据库内容与计划不一致");
        }
        self.sign(&database)?;
        self.sign(&files_database)?;
        let manifest = PublishManifest {
            format: PUBLISH_PLAN_FORMAT,
            plan_sha256: plan_sha256.to_owned(),
            repository_name: plan.repository_name.clone(),
            artifacts: plan.artifacts.clone(),
            repository_keyring: keyring,
            keyring_fingerprint: self.fingerprint.clone(),
            database: file_entry(&database)?,
            files_database: file_entry(&files_database)?,
            committed_at: Utc::now(),
        };
        let manifest_path = staging.join("manifest.json");
        fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
        self.sign(&manifest_path)?;
        for file in &new_files {
            for name in [file.clone(), format!("{file}.sig")] {
                let source = staging.join(&name);
                File::open(&source)?.sync_all()?;
                fs::rename(&source, self.arch().join(&name))?;
            }
        }
        sync_directory(&self.arch())?;
        sync_directory(&staging)?;
        fs::rename(&staging, &committed)?;
        sync_directory(&self.releases())?;
        self.activate(plan_sha256)?;
        if let Err(error) = self.prune() {
            tracing::warn!(error = %format!("{error:#}"), "清理旧 release 失败，下次重试");
        }
        file_digest(&committed.join("manifest.json"))
    }

    fn read_manifest(&self, release: &str) -> anyhow::Result<PublishManifest> {
        let directory = self.releases().join(release);
        let path = directory.join("manifest.json");
        self.verify(&path, &signature_path(&path))?;
        let manifest: PublishManifest = serde_json::from_slice(&fs::read(&path)?)?;
        if manifest.plan_sha256 != release {
            bail!("release 清单与目录名不一致");
        }
        Ok(manifest)
    }

    /// 校验某个已提交 release 并原子切换为 current（旧 current 成为 previous）。
    pub fn activate(&self, release: &str) -> anyhow::Result<()> {
        let manifest = self.read_manifest(release)?;
        let directory = self.releases().join(release);
        for entry in [&manifest.database, &manifest.files_database] {
            let path = directory.join(&entry.file);
            if file_digest(&path)? != entry.sha256 {
                bail!("release 数据库摘要不一致：{}", entry.file);
            }
            self.verify(&path, &signature_path(&path))?;
        }
        for artifact in manifest
            .artifacts
            .iter()
            .chain([&manifest.repository_keyring])
        {
            let path = self.arch().join(&artifact.file);
            if fs::metadata(&path).map(|metadata| metadata.len()).ok() != Some(artifact.size) {
                bail!("release 引用的软件包缺失或大小不符：{}", artifact.file);
            }
            self.verify(&path, &signature_path(&path))?;
        }
        let current = self.current_release();
        let name = &manifest.repository_name;
        let arch = self.arch();
        atomic_link(
            &arch,
            &format!("{name}.db.sig"),
            &format!("releases/{release}/{}.sig", manifest.database.file),
        )?;
        atomic_link(
            &arch,
            &format!("{name}.files.sig"),
            &format!("releases/{release}/{}.sig", manifest.files_database.file),
        )?;
        atomic_link(
            &arch,
            &format!("{name}.files"),
            &format!("releases/{release}/{}", manifest.files_database.file),
        )?;
        atomic_link(
            &arch,
            &format!("{name}.db"),
            &format!("releases/{release}/{}", manifest.database.file),
        )?;
        if let Some(previous) = current.filter(|current| current != release) {
            atomic_link(&self.releases(), "previous", &previous)?;
        }
        atomic_link(&self.releases(), "current", release)?;
        sync_directory(&arch)?;
        sync_directory(&self.releases())?;
        tracing::info!(%release, "仓库已切换");
        Ok(())
    }

    /// 当前 release：优先 `releases/current`，兼容旧布局时从 `<repo>.db` 链接推断。
    fn current_release(&self) -> Option<String> {
        if let Ok(target) = fs::read_link(self.releases().join("current")) {
            return Some(target.to_string_lossy().into_owned());
        }
        fs::read_dir(self.arch())
            .ok()?
            .filter_map(Result::ok)
            .find_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                if !name.ends_with(".db") {
                    return None;
                }
                let target = fs::read_link(entry.path()).ok()?;
                let mut components = target.components();
                (components.next()?.as_os_str() == "releases")
                    .then(|| {
                        components
                            .next()
                            .map(|part| part.as_os_str().to_string_lossy().into_owned())
                    })
                    .flatten()
            })
    }

    /// 紧急回滚：切回 previous。下一次期望状态变化会重新收敛。
    pub fn rollback(&self) -> anyhow::Result<String> {
        let previous = fs::read_link(self.releases().join("previous"))
            .context("没有 previous release，无法回滚")?
            .to_string_lossy()
            .into_owned();
        self.activate(&previous)?;
        Ok(previous)
    }

    /// 只保留 current 与 previous 两个 release 及其引用的软件包。
    pub fn prune(&self) -> anyhow::Result<()> {
        let mut keep_releases = BTreeSet::new();
        for link in ["current", "previous"] {
            if let Ok(target) = fs::read_link(self.releases().join(link)) {
                keep_releases.insert(target.to_string_lossy().into_owned());
            }
        }
        if keep_releases.is_empty() {
            return Ok(());
        }
        let mut keep_files = BTreeSet::new();
        for release in &keep_releases {
            keep_files.extend(referenced_files(&self.releases().join(release))?);
        }
        for entry in fs::read_dir(self.releases())?.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir()
                && !name.starts_with('.')
                && !keep_releases.contains(&name)
            {
                fs::remove_dir_all(entry.path())?;
            }
        }
        for entry in fs::read_dir(self.arch())?.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let package = name.strip_suffix(".sig").unwrap_or(&name);
            if entry.file_type()?.is_file()
                && package.contains(".pkg.tar.")
                && !keep_files.contains(package)
            {
                fs::remove_file(entry.path())?;
            }
        }
        sync_directory(&self.arch())?;
        Ok(())
    }

    fn find_keyring(&self) -> anyhow::Result<Option<(ArtifactRecord, PathBuf)>> {
        let version = self.keyring_version()?;
        let prefix = format!("{KEYRING_NAME}-{version}-any.pkg.tar.");
        for entry in fs::read_dir(self.arch())?.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) || name.ends_with(".sig") {
                continue;
            }
            let path = entry.path();
            if !signature_path(&path).is_file() || !self.keyring_matches(&path)? {
                continue;
            }
            self.verify(&path, &signature_path(&path))?;
            let artifact = ArtifactRecord {
                file: name,
                sha256: file_digest(&path)?,
                size: fs::metadata(&path)?.len(),
                package_name: KEYRING_NAME.into(),
                package_version: version.clone(),
                architecture: "any".into(),
            };
            return Ok(Some((artifact, path)));
        }
        Ok(None)
    }

    /// keyring 版本只随签名密钥变化：`1:<主密钥创建时间戳>-1`。
    fn keyring_version(&self) -> anyhow::Result<String> {
        let output = Command::new("/usr/bin/gpg")
            .arg("--homedir")
            .arg(&self.gpg_home)
            .args(["--batch", "--with-colons", "--list-keys", &self.fingerprint])
            .stdin(Stdio::null())
            .output()?;
        let created = String::from_utf8(output.stdout)?
            .lines()
            .find(|line| line.starts_with("pub:"))
            .and_then(|line| line.split(':').nth(5).map(str::to_owned))
            .filter(|value| {
                !value.is_empty() && value.chars().all(|character| character.is_ascii_digit())
            })
            .context("无法读取仓库密钥创建时间")?;
        Ok(format!("1:{created}-1"))
    }

    fn keyring_contents(&self) -> anyhow::Result<[(&'static str, Vec<u8>); 3]> {
        let public_key = Command::new("/usr/bin/gpg")
            .arg("--homedir")
            .arg(&self.gpg_home)
            .args(["--batch", "--export", &self.fingerprint])
            .stdin(Stdio::null())
            .output()?;
        if !public_key.status.success() || public_key.stdout.is_empty() {
            bail!("无法导出仓库 GPG 公钥");
        }
        Ok([
            ("aursmith.gpg", public_key.stdout),
            (
                "aursmith-trusted",
                format!("{}:4:\n", self.fingerprint).into_bytes(),
            ),
            ("aursmith-revoked", Vec::new()),
        ])
    }

    fn keyring_matches(&self, package: &Path) -> anyhow::Result<bool> {
        for (name, expected) in self.keyring_contents()? {
            let actual = Command::new("/usr/bin/bsdtar")
                .arg("-xOf")
                .arg(package)
                .arg(format!("usr/share/pacman/keyrings/{name}"))
                .stdin(Stdio::null())
                .output()?;
            if !actual.status.success() || actual.stdout != expected {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn build_keyring(&self, staging: &Path) -> anyhow::Result<ArtifactRecord> {
        let version = self.keyring_version()?;
        let pkgver = version
            .strip_prefix("1:")
            .and_then(|rest| rest.strip_suffix("-1"))
            .context("keyring 版本格式无效")?;
        let directory = tempfile::Builder::new()
            .prefix("aursmith-keyring-")
            .tempdir()?;
        let root = directory.path();
        let mut checksums = Vec::new();
        for (name, content) in self.keyring_contents()? {
            fs::write(root.join(name), &content)?;
            checksums.push(aursmith_core::sha256_hex(&content));
        }
        fs::write(
            root.join("aursmith-keyring.install"),
            "#!/bin/sh\n\npopulate_aursmith() {\n\tif usr/bin/pacman-key -l >/dev/null 2>&1; then\n\t\tusr/bin/pacman-key --populate aursmith\n\tfi\n}\n\npost_upgrade() {\n\tpopulate_aursmith\n}\n\npost_install() {\n\tif [ -x usr/bin/pacman-key ]; then\n\t\tpopulate_aursmith\n\tfi\n}\n",
        )?;
        fs::write(
            root.join("PKGBUILD"),
            format!(
                "pkgname={KEYRING_NAME}\nepoch=1\npkgver={pkgver}\npkgrel=1\npkgdesc='AURsmith repository signing keys'\narch=('any')\nurl='https://github.com/gsh20040816/AURsmith'\nlicense=('Apache-2.0')\ndepends=('pacman')\ninstall=aursmith-keyring.install\noptions=('!strip' '!debug')\nsource=('aursmith.gpg' 'aursmith-trusted' 'aursmith-revoked')\nsha256sums=('{}' '{}' '{}')\n\npackage() {{\n  install -Dm644 aursmith.gpg \"$pkgdir/usr/share/pacman/keyrings/aursmith.gpg\"\n  install -Dm644 aursmith-trusted \"$pkgdir/usr/share/pacman/keyrings/aursmith-trusted\"\n  install -Dm644 aursmith-revoked \"$pkgdir/usr/share/pacman/keyrings/aursmith-revoked\"\n}}\n",
                checksums[0], checksums[1], checksums[2]
            ),
        )?;
        let output = root.join("packages");
        fs::create_dir_all(&output)?;
        let status = Command::new("/usr/bin/makepkg")
            .args(["--noconfirm", "--force", "--nodeps", "--cleanbuild"])
            .current_dir(root)
            .env("SOURCE_DATE_EPOCH", "946684800")
            .env("PKGDEST", &output)
            .env("SRCDEST", root)
            .env("BUILDDIR", root.join("build"))
            .env("PKGEXT", ".pkg.tar.zst")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()?;
        if !status.success() {
            bail!("{KEYRING_NAME} makepkg 失败：{status}");
        }
        let packages: Vec<_> = fs::read_dir(&output)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".pkg.tar."))
            .collect();
        if packages.len() != 1 {
            bail!("{KEYRING_NAME} 产物数量不是 1");
        }
        let file = packages[0].file_name().to_string_lossy().into_owned();
        let destination = staging.join(&file);
        fs::copy(packages[0].path(), &destination)?;
        let artifact = ArtifactRecord {
            sha256: file_digest(&destination)?,
            size: fs::metadata(&destination)?.len(),
            file,
            package_name: KEYRING_NAME.into(),
            package_version: version,
            architecture: "any".into(),
        };
        validate_package_metadata(&destination, &artifact)?;
        Ok(artifact)
    }

    fn sign(&self, path: &Path) -> anyhow::Result<()> {
        run_checked(
            "/usr/bin/gpg",
            &[
                "--homedir".into(),
                self.gpg_home.as_os_str().into(),
                "--batch".into(),
                "--yes".into(),
                "--local-user".into(),
                self.fingerprint.clone().into(),
                "--detach-sign".into(),
                path.as_os_str().into(),
            ],
        )
    }

    fn verify(&self, path: &Path, signature: &Path) -> anyhow::Result<()> {
        run_checked(
            "/usr/bin/gpg",
            &[
                "--homedir".into(),
                self.gpg_home.as_os_str().into(),
                "--batch".into(),
                "--quiet".into(),
                "--verify".into(),
                signature.as_os_str().into(),
                path.as_os_str().into(),
            ],
        )
        .with_context(|| format!("签名校验失败：{}", path.display()))
    }
}

pub fn validate_plan(plan: &PublishPlan) -> anyhow::Result<()> {
    if plan.format != PUBLISH_PLAN_FORMAT {
        bail!("不支持的计划格式 {}", plan.format);
    }
    validate_repository_name(&plan.repository_name)?;
    let mut files = BTreeSet::new();
    let mut names = BTreeSet::new();
    for artifact in &plan.artifacts {
        validate_artifact_file_name(&artifact.file)?;
        aursmith_core::names::validate_package_name(&artifact.package_name)?;
        if !is_sha256_hex(&artifact.sha256) || artifact.size == 0 {
            bail!("产物摘要或大小无效：{}", artifact.file);
        }
        if artifact.package_name == KEYRING_NAME {
            bail!("{KEYRING_NAME} 是签名器保留的包名");
        }
        if !files.insert(&artifact.file) || !names.insert(&artifact.package_name) {
            bail!("计划包含重复产物：{}", artifact.file);
        }
    }
    Ok(())
}

/// 读取 release 目录清单引用的软件包文件（兼容旧 `release-manifest.json` 的 `path` 字段）。
fn referenced_files(directory: &Path) -> anyhow::Result<BTreeSet<String>> {
    let path = ["manifest.json", "release-manifest.json"]
        .iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
        .with_context(|| format!("release 缺少清单：{}", directory.display()))?;
    let value: Value = serde_json::from_slice(&fs::read(path)?)?;
    let mut files = BTreeSet::new();
    let entries = value["artifacts"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain([value["repository_keyring"].clone()]);
    for entry in entries {
        if let Some(file) = entry["file"].as_str().or_else(|| entry["path"].as_str()) {
            files.insert(file.to_owned());
        }
    }
    Ok(files)
}

fn read_repository_packages(database: &Path) -> anyhow::Result<BTreeMap<String, (String, String)>> {
    let output = Command::new("/usr/bin/bsdtar")
        .arg("-xOf")
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
        let file = record.lines().next().context("仓库条目缺少文件名")?;
        let field = |name: &str| {
            record
                .split_once(&format!("{name}\n"))
                .and_then(|(_, value)| value.lines().next())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .with_context(|| format!("仓库条目缺少 {name}"))
        };
        if packages
            .insert(field("%NAME%")?, (field("%VERSION%")?, file.to_owned()))
            .is_some()
        {
            bail!("仓库数据库包含重复包名");
        }
    }
    Ok(packages)
}

/// 产物自身的 `.PKGINFO` 必须与计划声明的 pkgname/pkgver/arch 一致。
fn validate_package_metadata(path: &Path, artifact: &ArtifactRecord) -> anyhow::Result<()> {
    let output = Command::new("/usr/bin/bsdtar")
        .arg("-xOf")
        .arg(path)
        .arg(".PKGINFO")
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() || output.stdout.len() > 1024 * 1024 {
        bail!("软件包缺少有效 .PKGINFO：{}", artifact.file);
    }
    let text = String::from_utf8(output.stdout)?;
    for (field, expected) in [
        ("pkgname", &artifact.package_name),
        ("pkgver", &artifact.package_version),
        ("arch", &artifact.architecture),
    ] {
        let actual = text
            .lines()
            .filter_map(|line| line.split_once(" = "))
            .find_map(|(name, value)| (name == field).then_some(value));
        if actual != Some(expected.as_str()) {
            bail!("软件包元数据与计划不一致：{} 的 {field}", artifact.file);
        }
    }
    Ok(())
}

fn repository_fingerprint(gpg_home: &Path) -> anyhow::Result<String> {
    let output = Command::new("/usr/bin/gpg")
        .arg("--homedir")
        .arg(gpg_home)
        .args([
            "--batch",
            "--with-colons",
            "--fingerprint",
            "--list-secret-keys",
        ])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        bail!("无法读取仓库 GPG 指纹");
    }
    String::from_utf8(output.stdout)?
        .lines()
        .filter(|line| line.starts_with("fpr:"))
        .filter_map(|line| line.split(':').nth(9))
        .find(|value| {
            value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
        })
        .map(str::to_owned)
        .context("仓库私钥没有有效主指纹")
}

fn signature_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

fn atomic_link(root: &Path, name: &str, target: &str) -> anyhow::Result<()> {
    let temporary = root.join(format!(".{name}.new"));
    if fs::symlink_metadata(&temporary).is_ok() {
        fs::remove_file(&temporary)?;
    }
    std::os::unix::fs::symlink(target, &temporary)?;
    fs::rename(temporary, root.join(name))?;
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, bytes)?;
    File::open(&temporary)?.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn run_checked(program: &str, arguments: &[OsString]) -> anyhow::Result<()> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()?;
    if !output.status.success() {
        bail!(
            "子进程失败：{program}，状态 {}：{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(500)
                .collect::<String>()
        );
    }
    Ok(())
}

fn file_entry(path: &Path) -> anyhow::Result<FileEntry> {
    Ok(FileEntry {
        file: path
            .file_name()
            .context("文件缺少名称")?
            .to_string_lossy()
            .into_owned(),
        sha256: file_digest(path)?,
        size: fs::metadata(path)?.len(),
    })
}

fn file_digest(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// 签名器主循环。
pub fn run(signer: &Signer, interval: std::time::Duration) -> ! {
    tracing::info!(fingerprint = %signer.fingerprint, "签名器已启动（无网络）");
    loop {
        if let Err(error) = signer.process_inbox() {
            tracing::error!(error = %format!("{error:#}"), "处理 inbox 失败");
        }
        std::thread::sleep(interval);
    }
}
