//! 静态解析 `.SRCINFO`。绝不 source PKGBUILD：公网主机上不执行任何 AUR 内容。

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    pub name: String,
    /// `runtime`、`build` 或 `check`。
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrcInfo {
    pub package_base: String,
    pub version: String,
    pub outputs: Vec<String>,
    pub dependencies: Vec<Dependency>,
    pub optional_dependencies: Vec<String>,
    pub provides: Vec<String>,
    pub architectures: Vec<String>,
    pub sources: Vec<String>,
    pub valid_pgp_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SrcInfoError {
    #[error(".SRCINFO 的 pkgbase 与期望不一致或缺少 pkgname")]
    Identity,
    #[error(".SRCINFO 缺少 {0}")]
    Missing(&'static str),
}

pub fn parse_srcinfo(expected_base: &str, text: &str) -> Result<SrcInfo, SrcInfoError> {
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
    let mut valid_pgp_keys = BTreeSet::new();
    for line in text.lines() {
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
            "validpgpkeys" => {
                valid_pgp_keys.insert(value.to_ascii_uppercase());
            }
            key if key == "source" || key == "source_x86_64" => {
                sources.insert(value.to_owned());
            }
            _ => {}
        }
    }
    if declared_base.as_deref() != Some(expected_base) || outputs.is_empty() {
        return Err(SrcInfoError::Identity);
    }
    let version = format!(
        "{}{}-{}",
        epoch
            .filter(|value| value != "0")
            .map(|value| format!("{value}:"))
            .unwrap_or_default(),
        pkgver.ok_or(SrcInfoError::Missing("pkgver"))?,
        pkgrel.ok_or(SrcInfoError::Missing("pkgrel"))?
    );
    Ok(SrcInfo {
        package_base: expected_base.to_owned(),
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
        valid_pgp_keys: valid_pgp_keys.into_iter().collect(),
    })
}

/// 去掉版本约束与 optdepends 描述：`foo>=1`、`foo: 说明` 都返回 `foo`。
pub fn dependency_name(value: &str) -> String {
    value
        .split(['<', '>', '=', ':'])
        .next()
        .unwrap_or(value)
        .trim()
        .to_owned()
}

/// 第一个 `git+https://` source（可带 `name::` 前缀）。
pub fn git_vcs_source(sources: &[String]) -> Option<&str> {
    sources.iter().find_map(|source| {
        let value = source
            .split_once("::")
            .map_or(source.as_str(), |(_, value)| value);
        value.starts_with("git+https://").then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "pkgbase = demo\n\tpkgver = 1.2\n\tpkgrel = 3\n\tepoch = 1\n\tarch = x86_64\n\tdepends = glibc>=2\n\tmakedepends = rust\n\toptdepends = foo: 说明\n\tsource = git+https://example.org/demo.git#tag=v1.2\n\tvalidpgpkeys = abcdef\n\npkgname = demo\n\tprovides = demo-bin=1.2\n\npkgname = demo-docs\n";

    #[test]
    fn parses_identity_dependencies_and_keys() {
        let info = parse_srcinfo("demo", SAMPLE).unwrap();
        assert_eq!(info.version, "1:1.2-3");
        assert_eq!(info.outputs, vec!["demo", "demo-docs"]);
        assert!(info.dependencies.contains(&Dependency {
            name: "glibc".into(),
            kind: "runtime".into()
        }));
        assert!(info.dependencies.contains(&Dependency {
            name: "rust".into(),
            kind: "build".into()
        }));
        assert_eq!(info.optional_dependencies, vec!["foo"]);
        assert_eq!(info.provides, vec!["demo-bin"]);
        assert_eq!(info.valid_pgp_keys, vec!["ABCDEF"]);
        assert_eq!(
            git_vcs_source(&info.sources),
            Some("git+https://example.org/demo.git#tag=v1.2")
        );
    }

    #[test]
    fn mismatched_pkgbase_is_rejected() {
        assert_eq!(parse_srcinfo("other", SAMPLE), Err(SrcInfoError::Identity));
        assert!(parse_srcinfo("demo", "pkgbase = demo\npkgname = demo\n").is_err());
    }
}
