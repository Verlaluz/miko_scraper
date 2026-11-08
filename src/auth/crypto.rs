use aes_gcm::{
    aead::{Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use rand::RngCore;
use crate::errors::AppError;

pub const NONCE_SIZE: usize = 12; // 96-bit nonce standard for GCM

/// Encrypts plaintext data using AES-256-GCM.
///
/// Returns a tuple containing:
/// 1. `ciphertext`: The encrypted bytes concatenated with the 16-byte Poly1305 authentication tag.
/// 2. `nonce`: The unique 12-byte nonce used for this specific encryption operation.
pub fn encrypt_session(key: &[u8; 32], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>), AppError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AppError::Crypto(format!("Failed to initialize cipher: {}", e)))?;

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .map_err(|e| AppError::Crypto(format!("Encryption failed: {}", e)))?;

    Ok((ciphertext, nonce_bytes.to_vec()))
}

/// Decrypts AES-256-GCM ciphertext using the given 32-byte key and nonce.
///
/// Returns the decrypted plaintext bytes, or an error if authentication fails.
pub fn decrypt_session(
    key: &[u8; 32],
    ciphertext: &[u8],
    nonce_bytes: &[u8],
) -> Result<Vec<u8>, AppError> {
    if nonce_bytes.len() != NONCE_SIZE {
        return Err(AppError::Crypto(format!(
            "Invalid nonce size: expected {} bytes, got {}",
            NONCE_SIZE,
            nonce_bytes.len()
        )));
    }

    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| AppError::Crypto(format!("Failed to initialize cipher: {}", e)))?;

    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| AppError::Crypto("Decryption failed: invalid ciphertext, key, or tag mismatch".to_string()))?;

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encryption_and_decryption_roundtrip() {
        let key = [42u8; 32];
        let original_data = b"my_super_secret_session_token_and_cookies";

        let (ciphertext, nonce) = encrypt_session(&key, original_data).expect("Encryption failed");
        assert_ne!(ciphertext, original_data);

        let decrypted = decrypt_session(&key, &ciphertext, &nonce).expect("Decryption failed");
        assert_eq!(decrypted, original_data);
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let key = [42u8; 32];
        let original_data = b"sensitive payload";

        let (mut ciphertext, nonce) = encrypt_session(&key, original_data).unwrap();
        // Flip one byte in the ciphertext
        if let Some(first) = ciphertext.first_mut() {
            *first ^= 0xFF;
        }

        let result = decrypt_session(&key, &ciphertext, &nonce);
        assert!(result.is_err());
    }

    #[test]
    fn test_wrong_key_fails() {
        let key1 = [1u8; 32];
        let key2 = [2u8; 32];
        let original_data = b"session";

        let (ciphertext, nonce) = encrypt_session(&key1, original_data).unwrap();
        let result = decrypt_session(&key2, &ciphertext, &nonce);
        assert!(result.is_err());
    }
}
