use anyhow::{Context, bail};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use reqwest::{Client, StatusCode, Url};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr},
    path::Path,
    process::Stdio,
    time::Duration,
};
use tempfile::TempDir;
use tokio::{process::Command, time::timeout};

const MAXIMUM_RPC_RESULTS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct AurPackage {
    pub name: String,
    pub package_base: String,
    pub version: String,
    pub description: Option<String>,
    pub maintainer: Option<String>,
    pub out_of_date: Option<i64>,
    pub last_modified: i64,
    #[serde(default)]
    pub depends: Vec<String>,
    #[serde(default)]
    pub make_depends: Vec<String>,
    #[serde(default)]
    pub check_depends: Vec<String>,
    #[serde(default)]
    pub opt_depends: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OfficialPackage {
    pub pkgname: String,
    pub pkgver: String,
    pub pkgrel: String,
    #[serde(default)]
    pub epoch: u64,
    pub repo: String,
    pub arch: String,
    #[serde(default)]
    pub provides: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OfficialSearchResponse {
    results: Vec<OfficialPackage>,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    version: u8,
    #[serde(rename = "type")]
    response_type: String,
    results: Vec<AurPackage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AurSnapshot {
    pub package_base: String,
    pub aur_commit: String,
    pub vcs_commit: Option<String>,
    #[serde(default)]
    pub vcs_ancestor_of_current: Option<bool>,
    pub version: String,
    pub outputs: Vec<String>,
    pub dependencies: Vec<Dependency>,
    pub optional_dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub architectures: Vec<String>,
    pub sources: Vec<String>,
    pub srcinfo: String,
    pub files: Vec<SnapshotFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub binary: bool,
    pub text: Option<String>,
    pub content_base64: String,
}

#[derive(Clone)]
pub struct AurClient {
    http: Client,
    base: Url,
}

impl AurClient {
    pub fn new(base: &str) -> anyhow::Result<Self> {
        let base = Url::parse(base).context("AUR base URL 无效")?;
        if base.scheme() != "https" || base.host_str().is_none() {
            bail!("AUR base URL 必须是 HTTPS URL");
        }
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
            .user_agent(concat!("AURsmith/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http, base })
    }

    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<AurPackage>> {
        self.search_by(query, "name-desc").await
    }

    pub async fn providers(&self, dependency: &str) -> anyhow::Result<Vec<AurPackage>> {
        self.search_by(dependency, "provides").await
    }

    async fn search_by(&self, query: &str, field: &str) -> anyhow::Result<Vec<AurPackage>> {
        let query = validate_query(query)?;
        let mut url = self.base.join("rpc/v5/search/")?;
        url.path_segments_mut()
            .map_err(|_| anyhow::anyhow!("AUR base URL 不能作为路径基址"))?
            .push(query);
        url.query_pairs_mut().append_pair("by", field);
        self.rpc(url, "search").await
    }

    pub async fn info(&self, names: &[String]) -> anyhow::Result<Vec<AurPackage>> {
        if names.is_empty() || names.len() > MAXIMUM_RPC_RESULTS {
            bail!("AUR info 每次需要 1 至 {MAXIMUM_RPC_RESULTS} 个包名");
        }
        let mut url = self.base.join("rpc/v5/info")?;
        {
            let mut pairs = url.query_pairs_mut();
            for name in names {
                pairs.append_pair("arg[]", validate_package_base(name)?);
            }
        }
        self.rpc(url, "multiinfo").await
    }

    pub async fn official(&self, name: &str) -> anyhow::Result<Vec<OfficialPackage>> {
        let name = validate_package_base(name)?;
        let mut url = Url::parse("https://archlinux.org/packages/search/json/")?;
        url.query_pairs_mut().append_pair("q", name);
        let mut delay = Duration::from_millis(500);
        let payload = loop {
            let response = self.http.get(url.clone()).send().await?;
            if response.status().is_success() {
                break response
                    .json::<OfficialSearchResponse>()
                    .await
                    .context("Arch 官方仓库接口返回无效 JSON")?;
            }
            if !matches!(
                response.status(),
                StatusCode::TOO_MANY_REQUESTS
                    | StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            ) || delay > Duration::from_secs(2)
            {
                response.error_for_status()?;
            }
            tokio::time::sleep(delay).await;
            delay *= 2;
        };
        Ok(payload
            .results
            .into_iter()
            .filter(|package| official_package_matches(package, name))
            .collect())
    }

    async fn rpc(&self, url: Url, expected_type: &str) -> anyhow::Result<Vec<AurPackage>> {
        let response = self.http.get(url).send().await?.error_for_status()?;
        let payload: RpcResponse = response.json().await.context("AUR RPC 返回无效 JSON")?;
        if payload.version != 5 || payload.response_type != expected_type {
            bail!("AUR RPC 返回类型不符合预期");
        }
        if payload.results.len() > MAXIMUM_RPC_RESULTS {
            bail!("AUR RPC 返回结果超过本地安全上限");
        }
        Ok(payload.results)
    }

    pub async fn snapshot(
        &self,
        package_base: &str,
        previous_vcs_commit: Option<&str>,
    ) -> anyhow::Result<AurSnapshot> {
        let package_base = validate_package_base(package_base)?;
        let repository = self.base.join(&format!("{package_base}.git"))?;
        let temporary = TempDir::new().context("无法创建 AUR 快照临时目录")?;
        run_git_clone(repository.as_str(), temporary.path()).await?;
        let commit = run_git_output(temporary.path(), &["rev-parse", "HEAD"]).await?;
        if commit.len() != 40
            || !commit
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            bail!("AUR Git 返回了无效 commit");
        }
        let srcinfo = run_git_output(temporary.path(), &["show", "HEAD:.SRCINFO"]).await?;
        let mut snapshot = parse_srcinfo(package_base, commit, srcinfo)?;
        snapshot.files = collect_snapshot_files(temporary.path()).await?;
        if package_base.ends_with("-git") {
            let (commit, ancestor) =
                resolve_git_vcs_commit(&snapshot.sources, previous_vcs_commit).await?;
            snapshot.vcs_commit = commit;
            snapshot.vcs_ancestor_of_current = ancestor;
        }
        Ok(snapshot)
    }
}

fn official_package_matches(package: &OfficialPackage, name: &str) -> bool {
    package.pkgname == name
        || package
            .provides
            .iter()
            .filter_map(|provided| provided.split(['=', '<', '>']).next())
            .any(|provided| provided == name)
}

async fn collect_snapshot_files(directory: &Path) -> anyhow::Result<Vec<SnapshotFile>> {
    const MAXIMUM_FILES: usize = 128;
    const MAXIMUM_TOTAL_BYTES: usize = 2 * 1024 * 1024;
    let names = run_git_bytes(directory, &["ls-tree", "-r", "-z", "--name-only", "HEAD"]).await?;
    let paths: Vec<String> = names
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()).context("AUR Git 路径不是 UTF-8"))
        .collect::<Result<_, _>>()?;
    if paths.is_empty() || paths.len() > MAXIMUM_FILES {
        bail!("AUR Git 文件数量超出 1 至 {MAXIMUM_FILES} 的限制");
    }
    let mut total = 0_usize;
    let mut files = Vec::with_capacity(paths.len());
    for path in paths {
        validate_snapshot_path(&path)?;
        let object = format!("HEAD:{path}");
        let bytes = run_git_bytes(directory, &["show", &object]).await?;
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| anyhow::anyhow!("AUR Git 文件总大小溢出"))?;
        if total > MAXIMUM_TOTAL_BYTES {
            bail!("AUR Git 文件总大小超过 2 MiB");
        }
        let text = String::from_utf8(bytes.clone()).ok();
        files.push(SnapshotFile {
            path,
            sha256: hex::encode(Sha256::digest(&bytes)),
            size: u64::try_from(bytes.len())?,
            binary: text.is_none(),
            text,
            content_base64: BASE64.encode(&bytes),
        });
    }
    Ok(files)
}

async fn run_git_bytes(directory: &Path, arguments: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = timeout(
        Duration::from_secs(15),
        Command::new("/usr/bin/git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("读取 AUR Git 对象超时")??;
    if !output.status.success() {
        bail!(
            "读取 AUR Git 对象失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

fn validate_snapshot_path(path: &str) -> anyhow::Result<()> {
    let candidate = Path::new(path);
    if path.starts_with('-')
        || candidate.is_absolute()
        || candidate.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || path.chars().any(char::is_control)
    {
        bail!("AUR Git 包含不安全路径");
    }
    Ok(())
}

async fn run_git_clone(repository: &str, directory: &Path) -> anyhow::Result<()> {
    let output = timeout(
        Duration::from_secs(60),
        Command::new("/usr/bin/git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "clone",
                "--ipv4",
                "--depth",
                "1",
                "--no-tags",
                "--quiet",
            ])
            .arg(repository)
            .arg(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("AUR Git clone 超时")??;
    if !output.status.success() {
        bail!(
            "AUR Git clone 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

async fn run_git_output(directory: &Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = timeout(
        Duration::from_secs(15),
        Command::new("/usr/bin/git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .stdin(Stdio::null())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("读取 AUR Git 快照超时")??;
    if !output.status.success() {
        bail!(
            "读取 AUR Git 快照失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout)
        .context("AUR Git 输出不是 UTF-8")
        .map(|value| value.trim().to_owned())
}

async fn resolve_git_vcs_commit(
    sources: &[String],
    previous_vcs_commit: Option<&str>,
) -> anyhow::Result<(Option<String>, Option<bool>)> {
    let Some(source) = sources.iter().find(|source| {
        source
            .split_once("::")
            .map(|(_, value)| value)
            .unwrap_or(source)
            .starts_with("git+https://")
    }) else {
        return Ok((None, None));
    };
    let source = source
        .split_once("::")
        .map(|(_, value)| value)
        .unwrap_or(source)
        .trim_start_matches("git+");
    let url = Url::parse(source).context("Git VCS source URL 无效")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("Git VCS source URL 不允许内嵌凭据");
    }
    let host = url.host_str().context("Git VCS source 缺少主机")?;
    let port = url.port_or_known_default().unwrap_or(443);
    let addresses = public_addresses(host, port).await?;
    let fragment = url.fragment().unwrap_or_default().to_owned();
    let mut repository = url.clone();
    repository.set_fragment(None);
    if let Some(commit) = fragment.strip_prefix("commit=") {
        if commit.len() == 40
            && commit
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Ok((Some(commit.to_owned()), None));
        }
    }
    let reference = fragment
        .strip_prefix("branch=")
        .map(|value| format!("refs/heads/{value}"))
        .or_else(|| {
            fragment
                .strip_prefix("tag=")
                .map(|value| format!("refs/tags/{value}"))
        })
        .unwrap_or_else(|| "HEAD".to_owned());
    let fetch_repository = repository.clone();
    repository
        .path_segments_mut()
        .map_err(|_| anyhow::anyhow!("Git VCS URL 不能作为路径基址"))?
        .push("info")
        .push("refs");
    repository
        .query_pairs_mut()
        .append_pair("service", "git-upload-pack");
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .resolve(
            host,
            addresses
                .iter()
                .find(|address| address.is_ipv4())
                .copied()
                .unwrap_or(addresses[0]),
        )
        .user_agent(concat!("AURsmith/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let response = client
        .get(repository)
        .header("Accept", "application/x-git-upload-pack-advertisement")
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|size| size > 8 * 1024 * 1024)
    {
        bail!("Git VCS refs 响应超过 8 MiB 上限");
    }
    let body = response.bytes().await?;
    if body.len() > 8 * 1024 * 1024 {
        bail!("Git VCS refs 响应超过 8 MiB 上限");
    }
    let refs = parse_git_advertisement(&body);
    let commit = refs
        .iter()
        .find(|(_, advertised_ref)| advertised_ref == &reference)
        .or_else(|| {
            refs.iter()
                .find(|(_, advertised_ref)| advertised_ref == &format!("{reference}^{{}}"))
        })
        .map(|(commit, _)| commit.clone())
        .ok_or_else(|| anyhow::anyhow!("Git VCS 上游没有返回目标 ref"))?;
    let ancestor = match previous_vcs_commit {
        Some(previous) if previous != commit => Some(
            git_commit_is_ancestor(
                &fetch_repository,
                &reference,
                previous,
                &commit,
                host,
                port,
                addresses[0].ip(),
            )
            .await?,
        ),
        Some(_) => Some(true),
        None => None,
    };
    Ok((Some(commit), ancestor))
}

async fn git_commit_is_ancestor(
    repository: &Url,
    reference: &str,
    previous: &str,
    current: &str,
    host: &str,
    port: u16,
    address: std::net::IpAddr,
) -> anyhow::Result<bool> {
    if !valid_commit(previous) || !valid_commit(current) {
        bail!("Git VCS ancestry commit 无效");
    }
    let temporary = TempDir::new().context("无法创建 Git VCS ancestry 临时目录")?;
    run_git_ancestry_command(temporary.path(), &["init", "--bare", "."]).await?;
    let address = match address {
        std::net::IpAddr::V4(value) => value.to_string(),
        std::net::IpAddr::V6(value) => format!("[{value}]"),
    };
    let curl_resolve = format!("http.curloptResolve={host}:{port}:{address}");
    run_git_ancestry_command(
        temporary.path(),
        &[
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "http.followRedirects=false",
            "-c",
            &curl_resolve,
            "fetch",
            "--no-tags",
            "--filter=blob:none",
            repository.as_str(),
            reference,
        ],
    )
    .await?;
    let fetched =
        run_git_ancestry_command(temporary.path(), &["rev-parse", "FETCH_HEAD^{commit}"]).await?;
    if fetched.trim() != current {
        bail!("Git VCS ancestry 获取的 commit 与 refs 广告不一致");
    }
    if !git_object_exists(
        temporary.path(),
        &["cat-file", "-e", &format!("{previous}^{{commit}}")],
    )
    .await?
    {
        return Ok(false);
    }
    git_condition(
        temporary.path(),
        &["merge-base", "--is-ancestor", previous, current],
    )
    .await
}

fn valid_commit(value: &str) -> bool {
    value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
}

async fn run_git_ancestry_command(directory: &Path, arguments: &[&str]) -> anyhow::Result<String> {
    let output = git_ancestry_output(directory, arguments).await?;
    if !output.status.success() {
        bail!(
            "检查 Git VCS ancestry 失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("Git VCS ancestry 输出不是 UTF-8")
}

async fn git_condition(directory: &Path, arguments: &[&str]) -> anyhow::Result<bool> {
    let output = git_ancestry_output(directory, arguments).await?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "检查 Git VCS ancestry 条件失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

async fn git_object_exists(directory: &Path, arguments: &[&str]) -> anyhow::Result<bool> {
    let output = git_ancestry_output(directory, arguments).await?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(_) => Ok(false),
        None => bail!("检查 Git VCS 对象时进程被信号终止"),
    }
}

async fn git_ancestry_output(
    directory: &Path,
    arguments: &[&str],
) -> anyhow::Result<std::process::Output> {
    timeout(
        Duration::from_secs(120),
        Command::new("/usr/bin/git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output(),
    )
    .await
    .context("检查 Git VCS ancestry 超时")?
    .context("无法启动 Git VCS ancestry 检查")
}

fn parse_git_advertisement(body: &[u8]) -> Vec<(String, String)> {
    body.split(|byte| *byte == b'\n' || *byte == b'\0')
        .filter_map(|segment| {
            (0..segment.len().saturating_sub(41)).find_map(|start| {
                let commit = &segment[start..start + 40];
                if commit.iter().all(u8::is_ascii_hexdigit)
                    && segment.get(start + 40) == Some(&b' ')
                {
                    let reference = segment[start + 41..]
                        .split(|byte| byte.is_ascii_whitespace())
                        .next()
                        .unwrap_or_default();
                    let commit = std::str::from_utf8(commit).ok()?.to_owned();
                    let reference = std::str::from_utf8(reference).ok()?.to_owned();
                    (!reference.is_empty()).then_some((commit, reference))
                } else {
                    None
                }
            })
        })
        .collect()
}

async fn public_addresses(host: &str, port: u16) -> anyhow::Result<Vec<std::net::SocketAddr>> {
    let addresses: Vec<_> = tokio::net::lookup_host((host, port))
        .await
        .context("无法解析 Git VCS source 主机")?
        .collect();
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) {
        bail!("Git VCS source 主机解析到私网、链路本地或保留地址");
    }
    Ok(addresses)
}

fn is_public_ip(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(value) => {
            !(value.is_private()
                || value.is_loopback()
                || value.is_link_local()
                || value.is_broadcast()
                || value.is_documentation()
                || value.is_unspecified()
                || value.octets()[0] == 0)
        }
        std::net::IpAddr::V6(value) => {
            !(value.is_loopback()
                || value.is_unspecified()
                || value.is_unique_local()
                || value.is_unicast_link_local())
        }
    }
}

fn parse_srcinfo(
    package_base: &str,
    commit: String,
    srcinfo: String,
) -> anyhow::Result<AurSnapshot> {
    let mut declared_base = None;
    let mut pkgver = None;
    let mut pkgrel = None;
    let mut epoch = None;
    let mut outputs = BTreeSet::new();
    let mut dependencies = BTreeSet::new();
    let mut optional_dependencies = BTreeSet::new();
    let mut provides = BTreeSet::new();
    let mut architectures = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for line in srcinfo.lines() {
        let Some((raw_key, raw_value)) = line.trim().split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        let value = raw_value.trim();
        if value.is_empty() {
            continue;
        }
        match key {
            "pkgbase" => declared_base = Some(value.to_owned()),
            "pkgver" => pkgver = Some(value.to_owned()),
            "pkgrel" => pkgrel = Some(value.to_owned()),
            "epoch" => epoch = Some(value.to_owned()),
            "pkgname" => {
                outputs.insert(value.to_owned());
            }
            "depends" | "depends_x86_64" => {
                dependencies.insert((dependency_name(value), "runtime".to_owned()));
            }
            "makedepends" | "makedepends_x86_64" => {
                dependencies.insert((dependency_name(value), "build".to_owned()));
            }
            "checkdepends" | "checkdepends_x86_64" => {
                dependencies.insert((dependency_name(value), "check".to_owned()));
            }
            "optdepends" | "optdepends_x86_64" => {
                optional_dependencies.insert(dependency_name(value));
            }
            "provides" | "provides_x86_64" => {
                provides.insert(dependency_name(value));
            }
            "arch" => {
                architectures.insert(value.to_owned());
            }
            key if key == "source" || key.starts_with("source_") => {
                sources.insert(value.to_owned());
            }
            _ => {}
        }
    }
    if declared_base.as_deref() != Some(package_base) || outputs.is_empty() {
        bail!(".SRCINFO 的 pkgbase 或 split outputs 不完整");
    }
    let version = format!(
        "{}{}-{}",
        epoch
            .filter(|value| value != "0")
            .map(|value| format!("{value}:"))
            .unwrap_or_default(),
        pkgver.context(".SRCINFO 缺少 pkgver")?,
        pkgrel.context(".SRCINFO 缺少 pkgrel")?
    );
    Ok(AurSnapshot {
        package_base: package_base.to_owned(),
        aur_commit: commit,
        vcs_commit: None,
        vcs_ancestor_of_current: None,
        version,
        outputs: outputs.into_iter().collect(),
        dependencies: dependencies
            .into_iter()
            .map(|(name, kind)| Dependency { name, kind })
            .collect(),
        optional_dependencies: optional_dependencies.into_iter().collect(),
        provides: provides.into_iter().collect(),
        architectures: architectures.into_iter().collect(),
        sources: sources.into_iter().collect(),
        srcinfo,
        files: Vec::new(),
    })
}

fn dependency_name(value: &str) -> String {
    value
        .split(['<', '>', '='])
        .next()
        .unwrap_or(value)
        .trim()
        .to_owned()
}

fn validate_query(value: &str) -> anyhow::Result<&str> {
    let value = value.trim();
    if !(2..=100).contains(&value.chars().count()) || value.chars().any(char::is_control) {
        bail!("AUR 搜索词长度必须为 2 至 100 个字符且不能包含控制字符");
    }
    Ok(value)
}

#[cfg(test)]
mod official_tests {
    use super::OfficialPackage;

    #[test]
    fn official_provider_names_ignore_version_constraints() {
        let package = OfficialPackage {
            pkgname: "pkgconf".into(),
            pkgver: "2.5.1".into(),
            pkgrel: "1".into(),
            epoch: 0,
            repo: "core".into(),
            arch: "x86_64".into(),
            provides: vec!["libpkgconf.so=8-64".into(), "pkg-config".into()],
        };
        assert!(super::official_package_matches(&package, "pkg-config"));
        assert!(!super::official_package_matches(&package, "pkg-config-git"));
    }
}

fn validate_package_base(value: &str) -> anyhow::Result<&str> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._+-".contains(character))
    {
        bail!("AUR 包名包含非法字符");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srcinfo_folds_split_outputs_and_dependency_kinds() {
        let parsed = parse_srcinfo(
            "demo",
            "a".repeat(40),
            "pkgbase = demo\npkgver = 2.0\npkgrel = 3\narch = x86_64\nmakedepends = cmake>=3\ncheckdepends = pytest\npkgname = demo-cli\ndepends = glibc\noptdepends = docs: manual\npkgname = demo-lib\nprovides = demo-api=2\n".into(),
        ).unwrap();
        assert_eq!(parsed.outputs, ["demo-cli", "demo-lib"]);
        assert!(parsed.dependencies.contains(&Dependency {
            name: "cmake".into(),
            kind: "build".into()
        }));
        assert!(parsed.dependencies.contains(&Dependency {
            name: "glibc".into(),
            kind: "runtime".into()
        }));
        assert_eq!(parsed.version, "2.0-3");
    }

    #[test]
    fn srcinfo_includes_x86_64_dependencies_and_ignores_other_architectures() {
        let parsed = parse_srcinfo(
            "demo",
            "a".repeat(40),
            "pkgbase = demo\npkgver = 1\npkgrel = 1\narch = x86_64\ndepends = common\ndepends_x86_64 = lib32-glibc>=2.11\nmakedepends_x86_64 = nasm\ncheckdepends_x86_64 = test-tool\noptdepends_x86_64 = optional-tool: feature\nprovides_x86_64 = demo-abi=1\ndepends_i686 = glibc\ndepends_aarch64 = aarch64-only\npkgname = demo\n".into(),
        )
        .unwrap();

        assert!(parsed.dependencies.contains(&Dependency {
            name: "lib32-glibc".into(),
            kind: "runtime".into()
        }));
        assert!(parsed.dependencies.contains(&Dependency {
            name: "nasm".into(),
            kind: "build".into()
        }));
        assert!(parsed.dependencies.contains(&Dependency {
            name: "test-tool".into(),
            kind: "check".into()
        }));
        assert!(
            !parsed
                .dependencies
                .iter()
                .any(|dependency| dependency.name == "glibc" || dependency.name == "aarch64-only")
        );
        assert_eq!(parsed.optional_dependencies, ["optional-tool: feature"]);
        assert_eq!(parsed.provides, ["demo-abi"]);
    }

    #[test]
    fn package_base_rejects_paths_and_shell_text() {
        assert!(validate_package_base("../evil").is_err());
        assert!(validate_package_base("demo;touch").is_err());
        assert!(validate_package_base("valid-bin").is_ok());
    }

    #[test]
    fn snapshot_paths_reject_traversal_options_and_controls() {
        assert!(validate_snapshot_path("PKGBUILD").is_ok());
        assert!(validate_snapshot_path("src/helper.sh").is_ok());
        assert!(validate_snapshot_path("../secret").is_err());
        assert!(validate_snapshot_path("-option").is_err());
        assert!(validate_snapshot_path("bad\npath").is_err());
    }

    #[test]
    fn private_and_special_addresses_are_rejected_for_vcs_tracking() {
        assert!(!is_public_ip("127.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("10.0.0.1".parse().unwrap()));
        assert!(!is_public_ip("169.254.1.1".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
        assert!(is_public_ip("1.1.1.1".parse().unwrap()));
    }

    #[test]
    fn git_advertisement_parser_extracts_refs_without_capabilities() {
        let body = b"001e# service=git-upload-pack\n0000003f0123456789012345678901234567890123456789 HEAD\0multi_ack\n00440123456789012345678901234567890123456789 refs/heads/main\n0000";
        let refs = parse_git_advertisement(body);
        assert!(refs.contains(&(
            "0123456789012345678901234567890123456789".into(),
            "HEAD".into()
        )));
        assert!(refs.contains(&(
            "0123456789012345678901234567890123456789".into(),
            "refs/heads/main".into()
        )));
    }

    #[tokio::test]
    async fn git_ancestry_distinguishes_fast_forward_and_rewritten_history() {
        let repository = TempDir::new().unwrap();
        run_git_ancestry_command(repository.path(), &["init", "."])
            .await
            .unwrap();
        run_git_ancestry_command(repository.path(), &["config", "user.name", "AURsmith Test"])
            .await
            .unwrap();
        run_git_ancestry_command(
            repository.path(),
            &["config", "user.email", "test@aursmith.invalid"],
        )
        .await
        .unwrap();
        std::fs::write(repository.path().join("fixture"), "first").unwrap();
        run_git_ancestry_command(repository.path(), &["add", "fixture"])
            .await
            .unwrap();
        run_git_ancestry_command(repository.path(), &["commit", "-m", "first"])
            .await
            .unwrap();
        let first = run_git_ancestry_command(repository.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap();
        std::fs::write(repository.path().join("fixture"), "second").unwrap();
        run_git_ancestry_command(repository.path(), &["commit", "-am", "second"])
            .await
            .unwrap();
        let second = run_git_ancestry_command(repository.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap();
        assert!(
            git_condition(
                repository.path(),
                &["merge-base", "--is-ancestor", first.trim(), second.trim()]
            )
            .await
            .unwrap()
        );

        run_git_ancestry_command(repository.path(), &["checkout", "--orphan", "rewritten"])
            .await
            .unwrap();
        std::fs::remove_file(repository.path().join("fixture")).unwrap();
        std::fs::write(repository.path().join("replacement"), "rewritten").unwrap();
        run_git_ancestry_command(repository.path(), &["add", "-A"])
            .await
            .unwrap();
        run_git_ancestry_command(repository.path(), &["commit", "-m", "rewritten"])
            .await
            .unwrap();
        let rewritten = run_git_ancestry_command(repository.path(), &["rev-parse", "HEAD"])
            .await
            .unwrap();
        assert!(
            !git_condition(
                repository.path(),
                &[
                    "merge-base",
                    "--is-ancestor",
                    first.trim(),
                    rewritten.trim()
                ]
            )
            .await
            .unwrap()
        );
    }
}
