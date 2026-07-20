//! Trust-root set for signed model artifacts (Plan 20-owned trust roots,
//! packet `pr10/prep-artifact-manifest`).
//!
//! Two root classes: embedded release roots (shipped with the release) and
//! explicitly imported local roots. Every root carries an ID, an Ed25519
//! public key, a validity window, a rotation epoch, and a status; revocations
//! are an explicit append-only list consulted at every verification. Trust
//! roots are never fetched from the artifact being verified — the caller
//! supplies the admitted `TrustRootSetV1`.
//!
//! Ed25519 verification is behind the `Ed25519Verifier` port. SCOPE
//! DEVIATION / ESCALATION: the workspace has no direct Ed25519 crate
//! (`ring` is present only transitively via rustls, and `Cargo.toml` is
//! integrator-owned), so this packet ships the port plus a deterministic
//! test-fake only. The integrator must select the real backend
//! (`ed25519-dalek` or a direct `ring` dependency) before any artifact is
//! admitted outside tests.
//!
//! QUARANTINE: no I/O, no network access, not reachable from production code.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::manifest::Ed25519PublicKeyHex;

/// Ed25519 verification port. Real backend selection is escalated to the
/// integrator (see module docs); tests use a deterministic fake.
pub trait Ed25519Verifier: Send + Sync {
    fn verify_ed25519(
        &self,
        public_key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<(), SignatureVerificationErrorV1>;
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("Ed25519 signature verification failed")]
pub struct SignatureVerificationErrorV1;

/// Lifecycle status of a trust root. A retired root was rotated out: it no
/// longer admits new artifacts, and admission under it is a typed rejection
/// (rollback to artifacts signed before retirement is a Plan 20 decision made
/// with an explicit older root, never an implicit fallback).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrustRootStatusV1 {
    Active,
    Retired,
}

/// One trust root: identity, key material, validity window, rotation epoch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRootV1 {
    pub root_id: String,
    pub public_key: Ed25519PublicKeyHex,
    pub not_before_unix: u64,
    pub not_after_unix: u64,
    pub rotation_epoch: u32,
    pub status: TrustRootStatusV1,
}

/// Append-only revocation entry for a trust root ID.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevocationRecordV1 {
    pub root_id: String,
    pub revoked_at_unix: u64,
    pub reason: String,
}

/// The admitted trust-root set: embedded release roots plus explicitly
/// imported local roots, with revocations. Resolution order is deterministic:
/// revocation first, then validity window, then status. Revocation applies to
/// both classes; local roots cannot shadow a release root ID.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustRootSetV1 {
    pub release_roots: Vec<TrustRootV1>,
    pub local_roots: Vec<TrustRootV1>,
    pub revocations: Vec<RevocationRecordV1>,
}

/// Why a trust root cannot admit an artifact.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum TrustRootErrorV1 {
    #[error("unknown trust root: {root_id}")]
    Unknown { root_id: String },
    #[error("trust root id is empty")]
    EmptyRootId,
    #[error("duplicate trust root id: {root_id}")]
    DuplicateRootId { root_id: String },
    #[error("trust root validity window is inverted: {root_id}")]
    InvalidValidityWindow { root_id: String },
    #[error("revocation root id is empty")]
    EmptyRevocationRootId,
    #[error("revocation reason is empty for trust root: {root_id}")]
    EmptyRevocationReason { root_id: String },
    #[error("trust root revoked at {revoked_at_unix}: {root_id}")]
    Revoked {
        root_id: String,
        revoked_at_unix: u64,
    },
    #[error("trust root not yet valid at {now_unix}: {root_id}")]
    NotYetValid { root_id: String, now_unix: u64 },
    #[error("trust root expired at {now_unix}: {root_id}")]
    Expired { root_id: String, now_unix: u64 },
    #[error("trust root retired by rotation: {root_id}")]
    Retired { root_id: String },
    #[error("local trust root id collides with a release root: {root_id}")]
    LocalShadowsRelease { root_id: String },
}

impl TrustRootSetV1 {
    /// Validate set-level invariants before any signature admission.
    pub fn validate(&self) -> Result<(), TrustRootErrorV1> {
        let mut release_ids = BTreeSet::new();
        for root in &self.release_roots {
            validate_root(root)?;
            if !release_ids.insert(root.root_id.as_str()) {
                return Err(TrustRootErrorV1::DuplicateRootId {
                    root_id: root.root_id.clone(),
                });
            }
        }
        let mut local_ids = BTreeSet::new();
        for local in &self.local_roots {
            validate_root(local)?;
            if release_ids.contains(local.root_id.as_str()) {
                return Err(TrustRootErrorV1::LocalShadowsRelease {
                    root_id: local.root_id.clone(),
                });
            }
            if !local_ids.insert(local.root_id.as_str()) {
                return Err(TrustRootErrorV1::DuplicateRootId {
                    root_id: local.root_id.clone(),
                });
            }
        }
        for revocation in &self.revocations {
            if revocation.root_id.trim().is_empty() {
                return Err(TrustRootErrorV1::EmptyRevocationRootId);
            }
            if revocation.reason.trim().is_empty() {
                return Err(TrustRootErrorV1::EmptyRevocationReason {
                    root_id: revocation.root_id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Resolve the admitted root for `root_id` at `now_unix`, applying
    /// revocation, validity window, and retirement in that order.
    pub fn resolve(&self, root_id: &str, now_unix: u64) -> Result<&TrustRootV1, TrustRootErrorV1> {
        if let Some(record) = self.revocations.iter().find(|r| r.root_id == root_id) {
            return Err(TrustRootErrorV1::Revoked {
                root_id: root_id.to_string(),
                revoked_at_unix: record.revoked_at_unix,
            });
        }
        let root = self
            .release_roots
            .iter()
            .chain(self.local_roots.iter())
            .find(|r| r.root_id == root_id)
            .ok_or_else(|| TrustRootErrorV1::Unknown {
                root_id: root_id.to_string(),
            })?;
        if now_unix < root.not_before_unix {
            return Err(TrustRootErrorV1::NotYetValid {
                root_id: root_id.to_string(),
                now_unix,
            });
        }
        if now_unix > root.not_after_unix {
            return Err(TrustRootErrorV1::Expired {
                root_id: root_id.to_string(),
                now_unix,
            });
        }
        if root.status == TrustRootStatusV1::Retired {
            return Err(TrustRootErrorV1::Retired {
                root_id: root_id.to_string(),
            });
        }
        Ok(root)
    }
}

fn validate_root(root: &TrustRootV1) -> Result<(), TrustRootErrorV1> {
    if root.root_id.trim().is_empty() {
        return Err(TrustRootErrorV1::EmptyRootId);
    }
    if root.not_before_unix > root.not_after_unix {
        return Err(TrustRootErrorV1::InvalidValidityWindow {
            root_id: root.root_id.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    //! Deterministic fake verifier for tests. NOT a real signature scheme:
    //! `fake_sign` derives a 64-byte stand-in from SHA-256(public_key ||
    //! message) so the valid/invalid signature matrix can run without a real
    //! Ed25519 backend. Never construct outside `#[cfg(test)]`.
    use super::*;
    use sha2::{Digest, Sha256};

    pub fn fake_sign(public_key: &[u8; 32], message: &[u8]) -> [u8; 64] {
        let mut hasher = Sha256::new();
        hasher.update(public_key);
        hasher.update(message);
        let first = hasher.finalize();
        let mut hasher = Sha256::new();
        hasher.update(first);
        let second = hasher.finalize();
        let mut out = [0u8; 64];
        out[..32].copy_from_slice(&first);
        out[32..].copy_from_slice(&second);
        out
    }

    #[derive(Clone, Copy, Debug, Default)]
    pub struct FakeEd25519Verifier;

    impl Ed25519Verifier for FakeEd25519Verifier {
        fn verify_ed25519(
            &self,
            public_key: &[u8; 32],
            message: &[u8],
            signature: &[u8; 64],
        ) -> Result<(), SignatureVerificationErrorV1> {
            if fake_sign(public_key, message) == *signature {
                Ok(())
            } else {
                Err(SignatureVerificationErrorV1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn root(id: &str, key_byte: u8) -> TrustRootV1 {
        TrustRootV1 {
            root_id: id.to_string(),
            public_key: Ed25519PublicKeyHex::new(hex::encode([key_byte; 32])).unwrap(),
            not_before_unix: 1_000,
            not_after_unix: 2_000,
            rotation_epoch: 1,
            status: TrustRootStatusV1::Active,
        }
    }

    fn set() -> TrustRootSetV1 {
        TrustRootSetV1 {
            release_roots: vec![root("release-1", 1)],
            local_roots: vec![root("local-1", 2)],
            revocations: vec![],
        }
    }

    #[test]
    fn resolves_release_and_local_roots_inside_window() {
        let set = set();
        set.validate().unwrap();
        assert_eq!(set.resolve("release-1", 1_500).unwrap().rotation_epoch, 1);
        assert_eq!(set.resolve("local-1", 1_000).unwrap().root_id, "local-1");
        assert_eq!(set.resolve("local-1", 2_000).unwrap().root_id, "local-1");
    }

    #[test]
    fn rejects_unknown_expired_and_not_yet_valid_roots() {
        let set = set();
        assert!(matches!(
            set.resolve("missing", 1_500),
            Err(TrustRootErrorV1::Unknown { .. })
        ));
        assert!(matches!(
            set.resolve("release-1", 999),
            Err(TrustRootErrorV1::NotYetValid { .. })
        ));
        assert!(matches!(
            set.resolve("release-1", 2_001),
            Err(TrustRootErrorV1::Expired { .. })
        ));
    }

    #[test]
    fn revoked_root_is_rejected_even_inside_window() {
        let mut set = set();
        set.revocations.push(RevocationRecordV1 {
            root_id: "release-1".to_string(),
            revoked_at_unix: 1_200,
            reason: "key compromise drill".to_string(),
        });
        assert!(matches!(
            set.resolve("release-1", 1_500),
            Err(TrustRootErrorV1::Revoked {
                revoked_at_unix: 1_200,
                ..
            })
        ));
    }

    #[test]
    fn retired_root_is_rejected_and_local_cannot_shadow_release() {
        let mut set = set();
        set.release_roots[0].status = TrustRootStatusV1::Retired;
        assert!(matches!(
            set.resolve("release-1", 1_500),
            Err(TrustRootErrorV1::Retired { .. })
        ));

        let mut shadowed = set.clone();
        shadowed.local_roots.push(root("release-1", 9));
        assert!(matches!(
            shadowed.validate(),
            Err(TrustRootErrorV1::LocalShadowsRelease { .. })
        ));
    }

    #[test]
    fn validation_rejects_ambiguous_or_malformed_root_sets() {
        let mut duplicate_release = set();
        duplicate_release.release_roots.push(root("release-1", 9));
        assert!(duplicate_release.validate().is_err());

        let mut duplicate_local = set();
        duplicate_local.local_roots.push(root("local-1", 9));
        assert!(duplicate_local.validate().is_err());

        let mut invalid_window = set();
        invalid_window.release_roots[0].not_before_unix = 2_001;
        assert!(invalid_window.validate().is_err());

        let mut empty_revocation = set();
        empty_revocation.revocations.push(RevocationRecordV1 {
            root_id: String::new(),
            revoked_at_unix: 1_200,
            reason: String::new(),
        });
        assert!(empty_revocation.validate().is_err());
    }

    #[test]
    fn fake_verifier_accepts_its_own_signatures_and_rejects_tampering() {
        let key = [42u8; 32];
        let message = b"canonical manifest bytes";
        let signature = fake_sign(&key, message);
        let verifier = FakeEd25519Verifier;
        verifier.verify_ed25519(&key, message, &signature).unwrap();

        let mut bad = signature;
        bad[0] ^= 1;
        assert!(verifier.verify_ed25519(&key, message, &bad).is_err());
        assert!(
            verifier
                .verify_ed25519(&[7u8; 32], message, &signature)
                .is_err()
        );
        assert!(verifier.verify_ed25519(&key, b"other", &signature).is_err());
    }
}
