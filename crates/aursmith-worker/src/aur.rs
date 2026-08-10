use anyhow::{Context, bail};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path, process::Stdio, time::Duration};
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
    pub repo: String,
    pub arch: String,
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
    pub version: String,
    pub outputs: Vec<String>,
    pub dependencies: Vec<Dependency>,
    pub optional_dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub architectures: Vec<String>,
    pub sources: Vec<String>,
    pub srcinfo: String,
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
        url.query_pairs_mut().append_pair("name", name);
        let payload: OfficialSearchResponse = self
            .http
            .get(url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await
            .context("Arch 官方仓库接口返回无效 JSON")?;
        Ok(payload
            .results
            .into_iter()
            .filter(|package| package.pkgname == name)
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

    pub async fn snapshot(&self, package_base: &str) -> anyhow::Result<AurSnapshot> {
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
        if package_base.ends_with("-git") {
            snapshot.vcs_commit = resolve_git_vcs_commit(&snapshot.sources).await?;
        }
        Ok(snapshot)
    }
}

async fn run_git_clone(repository: &str, directory: &Path) -> anyhow::Result<()> {
    let output = timeout(
        Duration::from_secs(60),
        Command::new("/usr/bin/git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "clone",
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

async fn resolve_git_vcs_commit(sources: &[String]) -> anyhow::Result<Option<String>> {
    let Some(source) = sources.iter().find(|source| {
        source
            .split_once("::")
            .map(|(_, value)| value)
            .unwrap_or(source)
            .starts_with("git+https://")
    }) else {
        return Ok(None);
    };
    let source = source
        .split_once("::")
        .map(|(_, value)| value)
        .unwrap_or(source)
        .trim_start_matches("git+");
    let url = Url::parse(source).context("Git VCS source URL 无效")?;
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
            return Ok(Some(commit.to_owned()));
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
        .resolve(host, addresses[0])
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
    Ok(Some(commit))
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
            "depends" => {
                dependencies.insert((dependency_name(value), "runtime".to_owned()));
            }
            "makedepends" => {
                dependencies.insert((dependency_name(value), "build".to_owned()));
            }
            "checkdepends" => {
                dependencies.insert((dependency_name(value), "check".to_owned()));
            }
            "optdepends" => {
                optional_dependencies.insert(dependency_name(value));
            }
            "provides" => {
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
    fn package_base_rejects_paths_and_shell_text() {
        assert!(validate_package_base("../evil").is_err());
        assert!(validate_package_base("demo;touch").is_err());
        assert!(validate_package_base("valid-bin").is_ok());
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
}
