use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditFile {
    pub path: String,
    pub declared_sha256: String,
    pub binary: bool,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingSeverity {
    Information,
    Suspicious,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeterministicFinding {
    pub rule_id: String,
    pub severity: FindingSeverity,
    pub path: String,
    pub summary: String,
}

pub fn scan_aur_wrapper(files: &[AuditFile]) -> Vec<DeterministicFinding> {
    let mut findings = Vec::new();
    for file in files {
        let path = Path::new(&file.path);
        if path.is_absolute()
            || file.path.starts_with('-')
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            findings.push(finding(
                "AUR_PATH_ESCAPE",
                FindingSeverity::Block,
                file,
                "AUR 文件路径可能逃逸快照根目录",
            ));
        }
        let actual = hex::encode(Sha256::digest(&file.bytes));
        if actual != file.declared_sha256.to_ascii_lowercase() {
            findings.push(finding(
                "AUR_DIGEST_MISMATCH",
                FindingSeverity::Block,
                file,
                "AUR 文件内容与声明摘要不一致",
            ));
        }
        if file.binary {
            findings.push(finding(
                "AUR_BINARY_WRAPPER",
                FindingSeverity::Suspicious,
                file,
                "AUR 包装仓库包含二进制文件，需要人工或 Agent 解释",
            ));
            continue;
        }
        let text = String::from_utf8_lossy(&file.bytes).to_ascii_lowercase();
        for marker in [
            "://127.0.0.1",
            "://localhost",
            "://169.254.",
            "://0.0.0.0",
            "://[::1]",
            "://10.",
            "://192.168.",
        ] {
            if text.contains(marker) {
                findings.push(finding(
                    "PRIVATE_NETWORK_TARGET",
                    FindingSeverity::Block,
                    file,
                    "包装文件引用私网、回环或链路本地目标",
                ));
                break;
            }
        }
        for (rule, marker, summary) in [
            ("DYNAMIC_NETWORK", "curl ", "构建逻辑中出现动态网络工具"),
            ("DYNAMIC_NETWORK", "wget ", "构建逻辑中出现动态网络工具"),
            ("SHELL_EVAL", "eval ", "包装逻辑使用动态 Shell 求值"),
            ("OBFUSCATED_PAYLOAD", "base64 -d", "包装逻辑解码内嵌载荷"),
            (
                "PERSISTENCE_HOOK",
                ".install",
                "软件包声明安装脚本或持久化钩子",
            ),
            ("PRIVILEGE_CHANGE", "setcap ", "包装逻辑修改文件 capability"),
        ] {
            if text.contains(marker) {
                findings.push(finding(rule, FindingSeverity::Suspicious, file, summary));
            }
        }
    }
    findings.sort_by(|left, right| {
        (&left.path, &left.rule_id, &left.summary).cmp(&(
            &right.path,
            &right.rule_id,
            &right.summary,
        ))
    });
    findings.dedup();
    findings
}

fn finding(
    rule_id: &str,
    severity: FindingSeverity,
    file: &AuditFile,
    summary: &str,
) -> DeterministicFinding {
    DeterministicFinding {
        rule_id: rule_id.to_owned(),
        severity,
        path: file.path.clone(),
        summary: summary.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, text: &str) -> AuditFile {
        AuditFile {
            path: path.into(),
            declared_sha256: hex::encode(Sha256::digest(text.as_bytes())),
            binary: false,
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn digest_and_private_target_are_absolute_blocks() {
        let mut input = file("PKGBUILD", "source=(http://127.0.0.1/payload)");
        input.declared_sha256 = "0".repeat(64);
        let findings = scan_aur_wrapper(&[input]);
        assert_eq!(
            findings
                .iter()
                .filter(|finding| finding.severity == FindingSeverity::Block)
                .count(),
            2
        );
    }

    #[test]
    fn suspicious_shell_constructs_do_not_claim_malware() {
        let findings =
            scan_aur_wrapper(&[file("PKGBUILD", "prepare() { curl example.org; eval x; }")]);
        assert!(
            findings
                .iter()
                .all(|finding| finding.severity != FindingSeverity::Block)
        );
        assert!(
            findings
                .iter()
                .any(|finding| finding.rule_id == "DYNAMIC_NETWORK")
        );
    }
}
