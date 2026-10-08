//! 名称与路径校验。所有来自 AUR、Builder 或浏览器的名字都必须先经过这里。

use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum NameError {
    #[error("软件包名称无效：{0}")]
    Package(String),
    #[error("路径无效：{0}")]
    Path(String),
    #[error("软件包文件名无效：{0}")]
    Artifact(String),
    #[error("仓库名称无效：{0}")]
    Repository(String),
}

/// Arch 软件包名：字母数字与 `@._+-`，不能以 `-` 或 `.` 开头，最长 128。
pub fn validate_package_name(value: &str) -> Result<&str, NameError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && !value.starts_with(['-', '.'])
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._+-".contains(character));
    if valid {
        Ok(value)
    } else {
        Err(NameError::Package(value.chars().take(64).collect()))
    }
}

/// 只允许不逃逸根目录的相对路径，且不能以 `-` 开头或包含控制字符。
pub fn validate_relative_path(value: &str) -> Result<&str, NameError> {
    let path = Path::new(value);
    let valid = !value.is_empty()
        && value.len() <= 512
        && !value.starts_with('-')
        && !path.is_absolute()
        && !value.chars().any(char::is_control)
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if valid {
        Ok(value)
    } else {
        Err(NameError::Path(value.chars().take(64).collect()))
    }
}

/// 软件包产物必须是纯文件名、包含 `.pkg.tar.`、不是签名文件。
pub fn validate_artifact_file_name(value: &str) -> Result<&str, NameError> {
    let valid = !value.is_empty()
        && value.len() <= 255
        && !value.starts_with(['-', '.'])
        && value.contains(".pkg.tar.")
        && !value.ends_with(".sig")
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "@._+-:".contains(character));
    if valid {
        Ok(value)
    } else {
        Err(NameError::Artifact(value.chars().take(64).collect()))
    }
}

/// pacman 仓库名：字母数字、`-`、`_`。
pub fn validate_repository_name(value: &str) -> Result<&str, NameError> {
    let valid = !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character));
    if valid {
        Ok(value)
    } else {
        Err(NameError::Repository(value.chars().take(64).collect()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names_follow_arch_rules() {
        assert!(validate_package_name("python-foo_bar+1@x").is_ok());
        assert!(validate_package_name("-rf").is_err());
        assert!(validate_package_name("a/b").is_err());
        assert!(validate_package_name("").is_err());
    }

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        assert!(validate_relative_path("src/patch.diff").is_ok());
        assert!(validate_relative_path("../../etc/shadow").is_err());
        assert!(validate_relative_path("/etc/shadow").is_err());
        assert!(validate_relative_path("-oops").is_err());
    }

    #[test]
    fn artifact_names_are_plain_package_files() {
        assert!(validate_artifact_file_name("foo-1:2.0-1-x86_64.pkg.tar.zst").is_ok());
        assert!(validate_artifact_file_name("foo-1-1-x86_64.pkg.tar.zst.sig").is_err());
        assert!(validate_artifact_file_name("dir/foo.pkg.tar.zst").is_err());
        assert!(validate_artifact_file_name("foo.tar.zst").is_err());
    }
}
