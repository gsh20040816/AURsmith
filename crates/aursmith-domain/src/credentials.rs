use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use uuid::Uuid;

pub const MINIMUM_PASSWORD_LENGTH: usize = 12;

pub fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.chars().count() < MINIMUM_PASSWORD_LENGTH {
        return Err("密码至少需要 12 个字符");
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes())?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_verifies_only_the_original_password() {
        let hash = hash_password("一段足够长的密码").unwrap();
        assert!(verify_password("一段足够长的密码", &hash));
        assert!(!verify_password("错误密码", &hash));
        assert!(!hash.contains("一段足够长的密码"));
    }

    #[test]
    fn short_passwords_are_rejected_before_hashing() {
        assert!(validate_password("short").is_err());
        assert!(validate_password("足够长的测试密码-123456").is_ok());
    }
}
