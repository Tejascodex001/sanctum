//! App PINs (the real one and the decoy) are stored only as slow hashes,
//! so a copied phone can't be brute-forced quickly (OWASP's Argon2id
//! settings: 19 MiB, 2 passes).

use argon2::{Algorithm, Argon2, Params, Version};

pub fn pin_hash(pin: &str, salt: &[u8]) -> [u8; 32] {
    let params = Params::new(19 * 1024, 2, 1, Some(32)).expect("valid parameters");
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = [0u8; 32];
    argon
        .hash_password_into(pin.as_bytes(), salt, &mut out)
        .expect("salt of 8+ bytes");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_differ_by_pin_and_salt() {
        let salt = [7u8; 16];
        assert_eq!(pin_hash("2468", &salt), pin_hash("2468", &salt));
        assert_ne!(pin_hash("2468", &salt), pin_hash("2469", &salt));
        assert_ne!(pin_hash("2468", &salt), pin_hash("2468", &[8u8; 16]));
    }
}
