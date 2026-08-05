//! Daemon-owned authenticated wire authority for multi-root continuations.

use thiserror::Error;
use tracedecay_application::{MultiRootContinuationStateV1, MultiRootContinuationV1};
use tracedecay_domain::{
    SessionCursorKeyIdV1, SessionCursorVersionV1, SignedCursorKeyRefV1, UtcMicros,
};
use tracedecay_temporal_query::ports::{CursorSignature, SessionCursorAuthenticator};

const MULTI_ROOT_CURSOR_PREFIX_V1: &str = "mr1";

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub(super) enum MultiRootContinuationError {
    #[error("multi-root continuation is malformed")]
    Malformed,
    #[error("multi-root continuation authentication failed")]
    AuthenticationFailed,
    #[error("multi-root continuation is expired")]
    Expired,
    #[error("multi-root continuation was issued in the future")]
    IssuedInFuture,
}

pub(super) fn seal(
    state: &MultiRootContinuationStateV1,
    key: &SignedCursorKeyRefV1,
    authenticator: &impl SessionCursorAuthenticator,
) -> Result<MultiRootContinuationV1, MultiRootContinuationError> {
    state
        .validate()
        .map_err(|_| MultiRootContinuationError::Malformed)?;
    let payload = serde_json::to_vec(state).map_err(|_| MultiRootContinuationError::Malformed)?;
    let key_id = hex::encode(key.key_id.as_str().as_bytes());
    let payload = hex::encode(payload);
    let authenticated = format!(
        "{MULTI_ROOT_CURSOR_PREFIX_V1}.{key_id}.{}.{}",
        key.version.value(),
        payload
    );
    let signature = authenticator
        .sign(key, authenticated.as_bytes())
        .map_err(|_| MultiRootContinuationError::AuthenticationFailed)?;
    MultiRootContinuationV1::from_opaque(format!("{authenticated}.{}", signature.to_hex()))
        .map_err(|_| MultiRootContinuationError::Malformed)
}

pub(super) fn open(
    continuation: &MultiRootContinuationV1,
    authenticator: &impl SessionCursorAuthenticator,
    observed_at: UtcMicros,
) -> Result<MultiRootContinuationStateV1, MultiRootContinuationError> {
    let mut parts = continuation.as_str().split('.');
    let prefix = parts.next().ok_or(MultiRootContinuationError::Malformed)?;
    let key_id_hex = parts.next().ok_or(MultiRootContinuationError::Malformed)?;
    let version = parts.next().ok_or(MultiRootContinuationError::Malformed)?;
    let payload_hex = parts.next().ok_or(MultiRootContinuationError::Malformed)?;
    let signature_hex = parts.next().ok_or(MultiRootContinuationError::Malformed)?;
    if prefix != MULTI_ROOT_CURSOR_PREFIX_V1 || parts.next().is_some() {
        return Err(MultiRootContinuationError::Malformed);
    }
    let key_id = String::from_utf8(
        hex::decode(key_id_hex).map_err(|_| MultiRootContinuationError::Malformed)?,
    )
    .map_err(|_| MultiRootContinuationError::Malformed)?;
    let key = SignedCursorKeyRefV1 {
        key_id: SessionCursorKeyIdV1::new(key_id)
            .map_err(|_| MultiRootContinuationError::Malformed)?,
        version: SessionCursorVersionV1::new(
            version
                .parse::<u16>()
                .map_err(|_| MultiRootContinuationError::Malformed)?,
        )
        .map_err(|_| MultiRootContinuationError::Malformed)?,
    };
    let authenticated =
        format!("{MULTI_ROOT_CURSOR_PREFIX_V1}.{key_id_hex}.{version}.{payload_hex}");
    let signature = CursorSignature::from_hex(signature_hex)
        .map_err(|_| MultiRootContinuationError::AuthenticationFailed)?;
    authenticator
        .verify(&key, authenticated.as_bytes(), &signature)
        .map_err(|_| MultiRootContinuationError::AuthenticationFailed)?;
    let payload = hex::decode(payload_hex).map_err(|_| MultiRootContinuationError::Malformed)?;
    let state: MultiRootContinuationStateV1 =
        serde_json::from_slice(&payload).map_err(|_| MultiRootContinuationError::Malformed)?;
    state
        .validate()
        .map_err(|_| MultiRootContinuationError::Malformed)?;
    if observed_at < state.issued_at {
        return Err(MultiRootContinuationError::IssuedInFuture);
    }
    if observed_at >= state.expires_at {
        return Err(MultiRootContinuationError::Expired);
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use tracedecay_application::{
        MultiRootAuthorizationBindingV1, MultiRootContinuationStateV1, MultiRootRootCursorV1,
    };
    use tracedecay_domain::{
        AuthorityEpoch, CodeGenerationId, ManifestDigest, RootGenerationV1, RootScopeOutcomeV1,
        ScopeOutcome, ScopeSetId, ScopeSetRevision,
    };
    use tracedecay_temporal_query::ports::InMemoryCursorAuthenticator;

    use super::*;

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
    }

    fn key() -> SignedCursorKeyRefV1 {
        SignedCursorKeyRefV1 {
            key_id: SessionCursorKeyIdV1::new("cursor-key.multi-root").unwrap(),
            version: SessionCursorVersionV1::new(3).unwrap(),
        }
    }

    fn state() -> MultiRootContinuationStateV1 {
        let generation = RootGenerationV1::new(
            digest('a'),
            CodeGenerationId::new("generation.cursor.1").unwrap(),
            digest('b'),
            digest('c'),
        )
        .unwrap();
        MultiRootContinuationStateV1::new(
            ScopeSetId::new("scope-set.cursor").unwrap(),
            ScopeSetRevision::new(7).unwrap(),
            digest('d'),
            vec![RootScopeOutcomeV1::new(digest('a'), ScopeOutcome::Exact(generation)).unwrap()],
            vec![MultiRootRootCursorV1::new(digest('a'), None).unwrap()],
            digest('e'),
            digest('f'),
            MultiRootAuthorizationBindingV1 {
                epoch: AuthorityEpoch(4),
                digest: digest('1'),
                expires_at: UtcMicros(1_000),
            },
            UtcMicros(100),
            UtcMicros(900),
            2,
            None,
        )
        .unwrap()
    }

    #[test]
    fn signed_continuation_rejects_tampering_and_expiry() {
        let key = key();
        let authenticator = InMemoryCursorAuthenticator::new(key.clone(), vec![0x5a; 32]).unwrap();
        let state = state();
        let continuation = seal(&state, &key, &authenticator).unwrap();

        assert_eq!(
            open(&continuation, &authenticator, UtcMicros(101)).unwrap(),
            state
        );
        let mut tampered = continuation.as_str().to_owned();
        let replacement = if tampered.ends_with('0') { '1' } else { '0' };
        tampered.pop();
        tampered.push(replacement);
        let tampered = MultiRootContinuationV1::from_opaque(tampered).unwrap();
        assert_eq!(
            open(&tampered, &authenticator, UtcMicros(101)),
            Err(MultiRootContinuationError::AuthenticationFailed)
        );
        assert_eq!(
            open(&continuation, &authenticator, UtcMicros(900)),
            Err(MultiRootContinuationError::Expired)
        );
    }

    #[test]
    fn public_wire_is_opaque() {
        let key = key();
        let authenticator = InMemoryCursorAuthenticator::new(key.clone(), vec![0x5a; 32]).unwrap();
        let continuation = seal(&state(), &key, &authenticator).unwrap();
        let wire = serde_json::to_string(&continuation).unwrap();

        assert!(wire.starts_with("\"mr1."));
        assert!(!wire.contains("scope-set.cursor"));
        assert!(!wire.contains("root_generations"));
    }
}
