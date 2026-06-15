use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::digest::{digest, SHA256};

use crate::error::ProxyError;

const NONCE_SIZE: usize = 12;

pub struct KeyStore {
    key: LessSafeKey,
}

impl KeyStore {
    pub fn new(master_key: &[u8; 32]) -> Self {
        let unbound_key =
            UnboundKey::new(&AES_256_GCM, master_key).expect("AES-256-GCM key creation failed");
        Self {
            key: LessSafeKey::new(unbound_key),
        }
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<(Vec<u8>, [u8; NONCE_SIZE]), ProxyError> {
        let rng = ring::rand::SystemRandom::new();
        let nonce_bytes: [u8; NONCE_SIZE] = ring::rand::generate(&rng)
            .map_err(|_| ProxyError::KeyManagement("failed to generate nonce".into()))?
            .expose();

        let nonce = Nonce::assume_unique_for_key(nonce_bytes);

        let mut in_out = plaintext.to_vec();
        let tag = self
            .key
            .seal_in_place_separate_tag(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| ProxyError::KeyManagement("encryption failed".into()))?;

        in_out.extend_from_slice(tag.as_ref());

        Ok((in_out, nonce_bytes))
    }

    pub fn decrypt(
        &self,
        encrypted: &[u8],
        nonce_bytes: &[u8; NONCE_SIZE],
    ) -> Result<Vec<u8>, ProxyError> {
        let nonce = Nonce::assume_unique_for_key(*nonce_bytes);

        let mut in_out = encrypted.to_vec();
        let decrypted = self
            .key
            .open_in_place(nonce, Aad::empty(), &mut in_out)
            .map_err(|_| ProxyError::KeyManagement("decryption failed".into()))?;

        Ok(decrypted.to_vec())
    }

    pub fn derive_key() -> Result<[u8; 32], ProxyError> {
        // Server mode: prefer environment variable
        #[cfg(feature = "server")]
        {
            if let Ok(env_key) = std::env::var("AI_PROXY_MASTER_KEY") {
                if !env_key.is_empty() {
                    let hash = digest(&SHA256, env_key.as_bytes());
                    let mut key = [0u8; 32];
                    key.copy_from_slice(hash.as_ref());
                    return Ok(key);
                }
            }
        }

        // Desktop mode (or server fallback): derive from hostname
        let hostname = hostname::get()
            .map_err(|_| ProxyError::KeyManagement("failed to get hostname".into()))?;
        let hostname_str = hostname.to_str().unwrap_or("default-ai-proxy-key");

        let hash = digest(&SHA256, hostname_str.as_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(hash.as_ref());
        Ok(key)
    }
}

pub fn encrypt_api_key(key: &str) -> Result<(Vec<u8>, [u8; NONCE_SIZE]), ProxyError> {
    let master = KeyStore::derive_key()?;
    let store = KeyStore::new(&master);
    store.encrypt(key.as_bytes())
}

pub fn decrypt_api_key(encrypted: &[u8], nonce: &[u8; NONCE_SIZE]) -> Result<String, ProxyError> {
    let master = KeyStore::derive_key()?;
    let store = KeyStore::new(&master);
    let decrypted = store.decrypt(encrypted, nonce)?;
    String::from_utf8(decrypted)
        .map_err(|e| ProxyError::KeyManagement(format!("UTF-8 decode error: {}", e)))
}

/// Re-export of `SelectedKey` so callers using `key::store` don't need to reach into rotation.
pub use crate::key::rotation::SelectedKey;

/// Load all active API keys for a given provider, without mutating usage counters.
///
/// Used by the proxy retry path to gather every available key for rotation
/// across upstream attempts. Decryption is intentionally left to the caller
/// so this function can stay a thin read query.
pub async fn list_active_keys(provider_id: &str) -> Result<Vec<SelectedKey>, ProxyError> {
    use sqlx::FromRow;

    #[derive(FromRow)]
    struct DbApiKey {
        id: String,
        encrypted_key: Vec<u8>,
        nonce: Vec<u8>,
    }

    let pool = crate::db::get_pool().await;
    let rows: Vec<DbApiKey> = sqlx::query_as(
        "SELECT id, encrypted_key, nonce FROM api_keys WHERE provider_id = ? AND is_active = 1",
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await
    .map_err(|e| ProxyError::KeyManagement(e.to_string()))?;

    Ok(rows
        .into_iter()
        .map(|r| SelectedKey {
            key_id: r.id,
            encrypted_key: r.encrypted_key,
            nonce: r.nonce,
        })
        .collect())
}
