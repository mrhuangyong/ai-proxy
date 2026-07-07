use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};

use super::error::BackupError;

const PBKDF2_ITERATIONS: u32 = 200_000;
const SALT_LEN: usize = 16;
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// Derive a 32-byte key from a passphrase + salt via PBKDF2-HMAC-SHA256.
pub fn derive_key(passphrase: &str, salt: &[u8]) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        std::num::NonZeroU32::new(PBKDF2_ITERATIONS).unwrap(),
        salt,
        passphrase.as_bytes(),
        &mut key,
    );
    key
}

/// Generate a random 16-byte salt.
pub fn generate_salt() -> [u8; SALT_LEN] {
    let rng = SystemRandom::new();
    let mut salt = [0u8; SALT_LEN];
    rng.fill(&mut salt).expect("rng fill failed");
    salt
}

/// Encrypt with AES-256-GCM using a passphrase-derived key. Returns (ciphertext+tag, nonce).
pub fn passphrase_encrypt(
    plaintext: &[u8],
    key: &[u8; KEY_LEN],
) -> Result<(Vec<u8>, [u8; NONCE_LEN]), BackupError> {
    let rng = SystemRandom::new();
    let nonce_bytes: [u8; NONCE_LEN] = ring::rand::generate(&rng)
        .map_err(|_| BackupError::Crypto("nonce generation failed".into()))?
        .expose();

    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| BackupError::Crypto("key creation failed".into()))?;
    let less_safe = LessSafeKey::new(unbound);

    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let mut buf = plaintext.to_vec();
    let tag = less_safe
        .seal_in_place_separate_tag(nonce, Aad::empty(), &mut buf)
        .map_err(|_| BackupError::Crypto("encryption failed".into()))?;
    buf.extend_from_slice(tag.as_ref());

    Ok((buf, nonce_bytes))
}

/// Decrypt AES-256-GCM ciphertext (tag appended). Fails on wrong key / tampering.
pub fn passphrase_decrypt(
    ciphertext: &[u8],
    nonce_bytes: &[u8; NONCE_LEN],
    key: &[u8; KEY_LEN],
) -> Result<Vec<u8>, BackupError> {
    let unbound = UnboundKey::new(&AES_256_GCM, key)
        .map_err(|_| BackupError::Crypto("key creation failed".into()))?;
    let less_safe = LessSafeKey::new(unbound);

    let nonce = Nonce::assume_unique_for_key(*nonce_bytes);
    let mut buf = ciphertext.to_vec();
    let plain = less_safe
        .open_in_place(nonce, Aad::empty(), &mut buf)
        .map_err(|_| BackupError::DecryptionFailed)?;
    Ok(plain.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let salt = generate_salt();
        let key = derive_key("correct horse battery", &salt);
        let plaintext = b"sk-secret-api-key-12345";
        let (ct, nonce) = passphrase_encrypt(plaintext, &key).unwrap();
        let pt = passphrase_decrypt(&ct, &nonce, &key).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn test_wrong_passphrase_fails() {
        let salt = generate_salt();
        let key1 = derive_key("right passphrase", &salt);
        let key2 = derive_key("WRONG passphrase", &salt);
        let (ct, nonce) = passphrase_encrypt(b"secret", &key1).unwrap();
        assert!(passphrase_decrypt(&ct, &nonce, &key2).is_err());
    }

    #[test]
    fn test_different_salt_produces_different_key() {
        let k1 = derive_key("same", &[0u8; 16]);
        let k2 = derive_key("same", &[1u8; 16]);
        assert_ne!(k1, k2);
    }
}
