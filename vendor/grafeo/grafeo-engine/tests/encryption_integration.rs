//! Integration tests for encryption primitives.
//!
//! Tests the encryption building blocks (KeyChain, PageEncryptor) that are
//! used by WAL and section-level encryption. Engine-level wiring (Config →
//! WAL/FileManager) is tracked separately.

// Miri cannot interpret AES-NI intrinsics used by aes-gcm: skip under Miri.
#![cfg(all(feature = "encryption", not(miri)))]

#[test]
fn test_key_chain_deterministic_derivation() {
    // Two KeyChains with the same ME produce identical derived keys,
    // so data encrypted by one can be decrypted by the other.
    let kc1 = grafeo_common::encryption::KeyChain::new([0x42; 32]);
    let kc2 = grafeo_common::encryption::KeyChain::new([0x42; 32]);

    let enc1 = kc1.encryptor_for("wal", b"0");
    let enc2 = kc2.encryptor_for("wal", b"0");

    let plaintext = b"secret data for determinism test";
    let nonce = [0u8; 12];
    let aad = b"test-aad";

    let ciphertext = enc1.encrypt(plaintext, &nonce, aad).unwrap();
    let decrypted = enc2.decrypt(&ciphertext, aad).unwrap();
    assert_eq!(&decrypted, plaintext);
}

#[test]
fn test_different_keys_cannot_decrypt() {
    let kc_a = grafeo_common::encryption::KeyChain::new([0xAA; 32]);
    let kc_b = grafeo_common::encryption::KeyChain::new([0xBB; 32]);

    let enc_a = kc_a.encryptor_for("wal", b"0");
    let enc_b = kc_b.encryptor_for("wal", b"0");

    let plaintext = b"cross-key test";
    let nonce = [0u8; 12];
    let aad = b"test-aad";

    let ciphertext = enc_a.encrypt(plaintext, &nonce, aad).unwrap();
    let result = enc_b.decrypt(&ciphertext, aad);
    assert!(result.is_err(), "decryption with wrong key should fail");
}

#[test]
fn test_different_contexts_produce_different_keys() {
    let kc = grafeo_common::encryption::KeyChain::new([0x42; 32]);

    let enc_wal = kc.encryptor_for("wal", b"0");
    let enc_section = kc.encryptor_for("section", b"0");

    let plaintext = b"context isolation test";
    let nonce = [0u8; 12];
    let aad = b"test-aad";

    let ciphertext = enc_wal.encrypt(plaintext, &nonce, aad).unwrap();
    let result = enc_section.decrypt(&ciphertext, aad);
    assert!(
        result.is_err(),
        "WAL-encrypted data should not decrypt with section key"
    );
}

#[test]
fn test_nonce_builder() {
    let nonce = grafeo_common::encryption::build_nonce(42, 1000);
    assert_eq!(nonce.len(), 12);
    // Verify deterministic
    let nonce2 = grafeo_common::encryption::build_nonce(42, 1000);
    assert_eq!(nonce, nonce2);
    // Different inputs produce different nonces
    let nonce3 = grafeo_common::encryption::build_nonce(43, 1000);
    assert_ne!(nonce, nonce3);
}

#[test]
fn test_large_payload_roundtrip() {
    let kc = grafeo_common::encryption::KeyChain::new([0x99; 32]);
    let enc = kc.encryptor_for("test", b"large");

    // reason: i % 256 is always in [0, 255], fits u8
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let plaintext: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
    let nonce = grafeo_common::encryption::build_nonce(0, 0);
    let aad = b"large-payload";

    let ciphertext = enc.encrypt(&plaintext, &nonce, aad).unwrap();
    let decrypted = enc.decrypt(&ciphertext, aad).unwrap();
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_tampered_ciphertext_fails() {
    let kc = grafeo_common::encryption::KeyChain::new([0x42; 32]);
    let enc = kc.encryptor_for("wal", b"0");

    let plaintext = b"tamper test";
    let nonce = [0u8; 12];
    let aad = b"test-aad";

    let mut ciphertext = enc.encrypt(plaintext, &nonce, aad).unwrap();
    // Flip a byte in the middle of the ciphertext
    let mid = ciphertext.len() / 2;
    ciphertext[mid] ^= 0xFF;

    let result = enc.decrypt(&ciphertext, aad);
    assert!(
        result.is_err(),
        "tampered ciphertext should fail authentication"
    );
}
