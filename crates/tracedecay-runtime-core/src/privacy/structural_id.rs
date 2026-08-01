use sha2::{Digest, Sha256};

use super::detect::sanitize_provider_metadata_text;

const PROTECTION_DOMAIN_V1: &[u8] = b"tracedecay.structural-identity-protection.v1";
const PROTECTION_PREFIX_V1: &str = "privacy.structural-id.v1.";
const CLAUDE_OBSERVATION_SOURCE_ID_PREFIX_V1: &str =
    "tracedecay-claude-observation-source-v1-sha256-";

#[derive(Clone, Copy, Debug)]
pub struct StructuralIdProtectionError;

/// True when `value` is already a protected structural-ID token or an opaque
/// Claude observation source digest that must not be re-hashed.
pub fn is_already_protected_structural_id(value: &str) -> bool {
    has_canonical_sha256_suffix(value, PROTECTION_PREFIX_V1)
        || has_canonical_sha256_suffix(value, CLAUDE_OBSERVATION_SOURCE_ID_PREFIX_V1)
}

fn has_canonical_sha256_suffix(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Replaces credential-shaped structural identifiers with a stable,
/// versioned digest. Public identifiers and values already protected by this
/// version are preserved byte-for-byte.
pub fn protect_sensitive_structural_id(
    value: &str,
) -> Result<String, StructuralIdProtectionError> {
    if is_already_protected_structural_id(value) {
        return Ok(value.to_owned());
    }
    let sanitized = sanitize_provider_metadata_text(value).ok_or(StructuralIdProtectionError)?;
    if sanitized == value {
        return Ok(value.to_owned());
    }

    let mut hasher = Sha256::new();
    for part in [PROTECTION_DOMAIN_V1, value.as_bytes()] {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    Ok(format!(
        "{PROTECTION_PREFIX_V1}{}",
        hex::encode(hasher.finalize())
    ))
}

/// Protects an optional structural identifier. Empty and missing values stay
/// absent; already-protected and public values are preserved byte-for-byte.
pub fn protect_optional_sensitive_structural_id(
    value: Option<&str>,
) -> Result<Option<String>, StructuralIdProtectionError> {
    match value {
        None | Some("") => Ok(None),
        Some(value) => protect_sensitive_structural_id(value).map(Some),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CLAUDE_OBSERVATION_SOURCE_ID_PREFIX_V1, PROTECTION_PREFIX_V1,
        is_already_protected_structural_id, protect_optional_sensitive_structural_id,
        protect_sensitive_structural_id,
    };

    fn credential_shaped_fixture() -> String {
        ["AKIA", "SYNTHETIC", "CANARY", "1"].concat()
    }

    #[test]
    fn protection_is_stable_idempotent_and_preserves_public_ids() {
        let raw = credential_shaped_fixture();
        let protected = protect_sensitive_structural_id(&raw).unwrap();
        assert!(protected.starts_with(PROTECTION_PREFIX_V1));
        assert_ne!(protected, raw);
        assert_eq!(
            protect_sensitive_structural_id(&protected).unwrap(),
            protected
        );
        assert_eq!(
            protect_sensitive_structural_id("session.public-123").unwrap(),
            "session.public-123"
        );
        assert_eq!(
            protect_optional_sensitive_structural_id(Some(" session.public-123 ")).unwrap(),
            Some(" session.public-123 ".to_string())
        );
        let opaque_source = concat!(
            "tracedecay-claude-observation-source-v1-sha256-",
            "c35dddcc2fa7cfbcc40232d6f298e54f5a471a51227387a4e6a31b8f48e1ecb8"
        );
        assert_eq!(
            protect_sensitive_structural_id(opaque_source).unwrap(),
            opaque_source
        );
    }

    #[test]
    fn forged_protection_prefixes_do_not_bypass_secret_scanning() {
        for forged in [
            format!("{PROTECTION_PREFIX_V1}sk-test-123456"),
            format!("{CLAUDE_OBSERVATION_SOURCE_ID_PREFIX_V1}sk-test-123456"),
        ] {
            assert!(!is_already_protected_structural_id(&forged));
            let protected = protect_sensitive_structural_id(&forged).unwrap();
            assert!(is_already_protected_structural_id(&protected));
            assert!(!protected.contains("sk-test-123456"));
        }
    }
}
