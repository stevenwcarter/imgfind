//! Argon2 password hashing helpers for telnet accounts.

use anyhow::{Result, anyhow};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};

/// Hash a plaintext password into an Argon2 PHC string (random salt).
pub fn hash_password(plain: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let phc = Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map_err(|e| anyhow!("failed to hash password: {e}"))?
        .to_string();
    Ok(phc)
}

/// Verify a plaintext password against a stored PHC string.
/// Returns `false` for any mismatch or malformed hash — never panics.
pub fn verify_password(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_not_plaintext_and_verifies() {
        let hash = hash_password("hunter2").unwrap();
        assert_ne!(hash, "hunter2");
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password("hunter2", &hash));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let hash = hash_password("hunter2").unwrap();
        assert!(!verify_password("hunter3", &hash));
    }

    #[test]
    fn malformed_hash_never_panics() {
        assert!(!verify_password("anything", "not-a-valid-phc-string"));
    }

    #[test]
    fn two_hashes_of_same_password_differ() {
        // Random salt => different PHC strings, both verify.
        let a = hash_password("same").unwrap();
        let b = hash_password("same").unwrap();
        assert_ne!(a, b);
        assert!(verify_password("same", &a));
        assert!(verify_password("same", &b));
    }
}
