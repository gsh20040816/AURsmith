use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand_core::OsRng;

pub const MAXIMUM_PASSWORD_BYTES: usize = 512;

pub fn validate_password(password: &str) -> Result<(), &'static str> {
    if password.chars().count() < 12 {
        return Err("管理员密码至少需要 12 个字符");
    }
    if password.len() > MAXIMUM_PASSWORD_BYTES {
        return Err("管理员密码不能超过 512 个 UTF-8 字节");
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).is_ok_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_verifies_passwords_without_plaintext_storage() {
        let password = "足够长的管理员密码-123456";
        let encoded = hash_password(password).unwrap();
        assert!(!encoded.contains(password));
        assert!(verify_password(password, &encoded));
        assert!(!verify_password("错误但同样足够长-123456", &encoded));
        assert!(validate_password("短密码").is_err());
    }

    #[test]
    fn password_length_is_bounded_by_utf8_bytes() {
        assert!(validate_password(&"a".repeat(MAXIMUM_PASSWORD_BYTES)).is_ok());
        assert!(validate_password(&"a".repeat(MAXIMUM_PASSWORD_BYTES + 1)).is_err());
        assert!(validate_password(&"密".repeat(170)).is_ok());
        assert!(validate_password(&"密".repeat(171)).is_err());
    }
}
