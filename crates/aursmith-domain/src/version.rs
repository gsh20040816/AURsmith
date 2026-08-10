use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedVersion {
    pub upstream_version: String,
    pub upstream_pkgrel: String,
    pub local_rebuild: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VersionError {
    #[error("upstream version and pkgrel must not be empty")]
    EmptyComponent,
}

impl PublishedVersion {
    pub fn new(
        upstream_version: impl Into<String>,
        upstream_pkgrel: impl Into<String>,
        local_rebuild: u32,
    ) -> Result<Self, VersionError> {
        let value = Self {
            upstream_version: upstream_version.into(),
            upstream_pkgrel: upstream_pkgrel.into(),
            local_rebuild,
        };
        if value.upstream_version.trim().is_empty() || value.upstream_pkgrel.trim().is_empty() {
            return Err(VersionError::EmptyComponent);
        }
        Ok(value)
    }

    pub fn published_pkgrel(&self) -> String {
        if self.local_rebuild == 0 {
            self.upstream_pkgrel.clone()
        } else {
            format!("{}.{}", self.upstream_pkgrel, self.local_rebuild)
        }
    }

    pub fn display(&self) -> String {
        format!("{}-{}", self.upstream_version, self.published_pkgrel())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_build_preserves_upstream_pkgrel() {
        assert_eq!(
            PublishedVersion::new("2.0", "1", 0).unwrap().display(),
            "2.0-1"
        );
    }

    #[test]
    fn rebuild_adds_monotonic_local_suffix() {
        assert_eq!(
            PublishedVersion::new("2.0", "1", 2).unwrap().display(),
            "2.0-1.2"
        );
    }
}
