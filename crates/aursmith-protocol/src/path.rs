use std::path::{Component, Path};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathPolicyError {
    #[error("路径不能为空")]
    Empty,
    #[error("只允许相对路径")]
    Absolute,
    #[error("路径不能包含父目录、根目录或平台前缀")]
    UnsafeComponent,
}

pub fn validate_relative_path(value: &str) -> Result<(), PathPolicyError> {
    if value.is_empty() {
        return Err(PathPolicyError::Empty);
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return Err(PathPolicyError::Absolute);
    }
    for component in path.components() {
        if matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        ) {
            return Err(PathPolicyError::UnsafeComponent);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_relative_paths_are_accepted() {
        assert!(validate_relative_path("jobs/123/result.pkg.tar.zst").is_ok());
    }

    #[test]
    fn traversal_and_absolute_paths_are_rejected() {
        assert!(validate_relative_path("../../etc/shadow").is_err());
        assert!(validate_relative_path("/etc/shadow").is_err());
    }
}
