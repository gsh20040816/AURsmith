//! AURsmith 共享领域逻辑。
//!
//! 这里只放纯逻辑：不访问网络、数据库或文件系统，便于 `aursmithd`、签名器和
//! 家用 Builder 共用，也便于单元测试。

pub mod credentials;
pub mod graph;
pub mod names;
pub mod protocol;
pub mod review;
pub mod scan;
pub mod srcinfo;

use sha2::{Digest, Sha256};

/// 小写十六进制 SHA-256。
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// 判断字符串是否是 64 位小写十六进制 SHA-256。
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
