use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId, SignedCursorKeyRefV1, TemporalModeV1,
};

use super::ports::{
    CursorKeyError, CursorSignature, SessionCursorAuthenticator, TemporalExecutionSnapshot,
    TemporalParticipantManifest, TemporalRetrievalScope,
};

const CURSOR_FORMAT_VERSION: &str = "2";
const MAX_CURSOR_PAYLOAD_HEX_BYTES: usize = 2 * 65_536;
const MAX_CURSOR_KEY_ID_HEX_BYTES: usize = 2 * 1024;
const MAX_SORT_KEY_STABLE_ID_BYTES: usize = 4 * 1024;
pub const CURSOR_LIFETIME_MICROS: i64 = 24 * 60 * 60 * 1_000_000;
pub const CURSOR_CLOCK_SKEW_MICROS: i64 = 5 * 60 * 1_000_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StableSortKey {
    pub normalized_score_micros: u64,
    pub knowledge_at_micros: i64,
    pub stable_id: String,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CursorError {
    #[error("cursor is malformed")]
    Malformed,
    #[error("cursor authentication failed")]
    Tampered,
    #[error("cursor belongs to a different request")]
    WrongRequest,
    #[error("cursor semantic filters changed")]
    FilterMismatch,
    #[error("cursor root binding changed")]
    RootMismatch,
    #[error("cursor retrieval scope or session binding changed")]
    SessionMismatch,
    #[error("cursor belongs to a different authorization scope")]
    WrongAccess,
    #[error("cursor temporal mode or cutoff changed")]
    TemporalModeMismatch,
    #[error("cursor retrieval grain changed")]
    GrainMismatch,
    #[error("cursor schema version changed")]
    SchemaMismatch,
    #[error("cursor ranking version changed")]
    RankingMismatch,
    #[error("cursor configuration binding changed")]
    ConfigurationMismatch,
    #[error("cursor signing key id changed")]
    KeyIdMismatch,
    #[error("cursor signing key version changed")]
    KeyVersionMismatch,
    #[error("cursor execution generation changed")]
    GenerationMismatch,
    #[error("cursor participant generation manifest changed")]
    ParticipantManifestMismatch,
    #[error("cursor snapshot epoch changed")]
    EpochMismatch,
    #[error("cursor source watermark changed")]
    SourceWatermarkMismatch,
    #[error("cursor projection watermark changed")]
    ProjectionWatermarkMismatch,
    #[error("cursor index watermark changed")]
    IndexWatermarkMismatch,
    #[error("cursor summary watermark changed")]
    SummaryWatermarkMismatch,
    #[error("cursor stable sort key changed or is invalid")]
    SortKeyMismatch,
    #[error("cursor expired or has an invalid validity window")]
    Expired,
    #[error("cursor signing key is unknown or expired")]
    UnknownOrExpiredKey,
    #[error("cursor signing key is unavailable")]
    KeyUnavailable,
    #[error("cursor signing key material is invalid")]
    InvalidKeyMaterial,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorPayload {
    issued_at_micros: i64,
    expires_at_micros: i64,
    request_digest: String,
    filter_digest: String,
    root_digest: String,
    scope_kind: CursorScopeKind,
    session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_scope: Option<String>,
    access_digest: String,
    temporal_mode: String,
    cutoff_micros: Option<i64>,
    grain: String,
    generation: u64,
    source_watermark: u64,
    projection_watermark: u64,
    index_watermark: u64,
    summary_watermark: u64,
    participant_manifest: TemporalParticipantManifest,
    epoch_digest: String,
    schema_version: u32,
    ranking_version: u32,
    configuration_digest: String,
    last_sort_key: StableSortKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CursorScopeKind(String);

impl CursorPayload {
    fn from_snapshot(
        snapshot: &TemporalExecutionSnapshot,
        last_sort_key: StableSortKey,
        issued_at_micros: i64,
    ) -> Result<Self, CursorError> {
        validate_sort_key(&last_sort_key)?;
        snapshot.cursor_key().ok_or(CursorError::KeyUnavailable)?;
        let expires_at_micros = issued_at_micros
            .checked_add(CURSOR_LIFETIME_MICROS)
            .ok_or(CursorError::Malformed)?;
        Ok(Self {
            issued_at_micros,
            expires_at_micros,
            request_digest: snapshot.request_digest().as_str().to_string(),
            filter_digest: snapshot.filter_digest().as_str().to_string(),
            root_digest: snapshot.root_digest().as_str().to_string(),
            scope_kind: cursor_scope_kind(snapshot.request().retrieval_scope()),
            session_id: snapshot
                .request()
                .retrieval_scope()
                .session_id()
                .map(ToString::to_string),
            provider_scope: snapshot.provider_scope().map(str::to_string),
            access_digest: snapshot.access_digest().as_str().to_string(),
            temporal_mode: snapshot.temporal_mode().as_str().to_string(),
            cutoff_micros: temporal_cutoff(snapshot.temporal_mode()),
            grain: snapshot.grain().as_str().to_string(),
            generation: snapshot.watermarks().generation,
            source_watermark: snapshot.watermarks().source,
            projection_watermark: snapshot.watermarks().projection,
            index_watermark: snapshot.watermarks().index,
            summary_watermark: snapshot.watermarks().summary,
            participant_manifest: snapshot.participant_manifest().clone(),
            epoch_digest: snapshot.participant_manifest().epoch_digest().to_string(),
            schema_version: snapshot.versions().schema,
            ranking_version: snapshot.versions().ranking,
            configuration_digest: snapshot
                .versions()
                .configuration_digest
                .as_str()
                .to_string(),
            last_sort_key,
        })
    }
}

pub fn encode_cursor(
    snapshot: &TemporalExecutionSnapshot,
    last_sort_key: &StableSortKey,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
) -> Result<String, CursorError> {
    encode_cursor_at(snapshot, last_sort_key, authenticator, now_micros()?)
}

fn encode_cursor_at(
    snapshot: &TemporalExecutionSnapshot,
    last_sort_key: &StableSortKey,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
    issued_at_micros: i64,
) -> Result<String, CursorError> {
    let payload = CursorPayload::from_snapshot(snapshot, last_sort_key.clone(), issued_at_micros)?;
    let payload_bytes = serde_json::to_vec(&payload).map_err(|_| CursorError::Malformed)?;
    let payload_hex = hex::encode(payload_bytes);
    let key_ref = snapshot.cursor_key().ok_or(CursorError::KeyUnavailable)?;
    let key_id_hex = hex::encode(key_ref.key_id.as_str().as_bytes());
    if key_id_hex.is_empty()
        || key_id_hex.len() > MAX_CURSOR_KEY_ID_HEX_BYTES
        || payload_hex.is_empty()
        || payload_hex.len() > MAX_CURSOR_PAYLOAD_HEX_BYTES
    {
        return Err(CursorError::Malformed);
    }
    let key_version = key_ref.version.value();
    let authenticated = format!("{CURSOR_FORMAT_VERSION}.{key_id_hex}.{key_version}.{payload_hex}");
    let signature = authenticator
        .sign(key_ref, authenticated.as_bytes())
        .map_err(map_key_error)?
        .to_hex();
    Ok(format!("{authenticated}.{signature}"))
}

pub fn verify_cursor(
    encoded: &str,
    expected: &TemporalExecutionSnapshot,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
) -> Result<StableSortKey, CursorError> {
    verify_cursor_at(encoded, expected, authenticator, now_micros()?)
}

fn verify_cursor_at(
    encoded: &str,
    expected: &TemporalExecutionSnapshot,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
    now_micros: i64,
) -> Result<StableSortKey, CursorError> {
    let mut parts = encoded.split('.');
    let version = parts.next().ok_or(CursorError::Malformed)?;
    let key_id_hex = parts.next().ok_or(CursorError::Malformed)?;
    let key_version_text = parts.next().ok_or(CursorError::Malformed)?;
    let payload_hex = parts.next().ok_or(CursorError::Malformed)?;
    let signature_hex = parts.next().ok_or(CursorError::Malformed)?;
    if parts.next().is_some()
        || version != CURSOR_FORMAT_VERSION
        || key_id_hex.is_empty()
        || key_id_hex.len() > MAX_CURSOR_KEY_ID_HEX_BYTES
        || payload_hex.is_empty()
        || payload_hex.len() > MAX_CURSOR_PAYLOAD_HEX_BYTES
        || signature_hex.len() != 64
    {
        return Err(CursorError::Malformed);
    }

    let key_id_bytes = hex::decode(key_id_hex).map_err(|_| CursorError::Malformed)?;
    let key_id_text = String::from_utf8(key_id_bytes).map_err(|_| CursorError::Malformed)?;
    let key_id = SessionCursorKeyIdV1::new(key_id_text).map_err(|_| CursorError::Malformed)?;
    let key_version_value = key_version_text
        .parse::<u16>()
        .map_err(|_| CursorError::Malformed)?;
    let key_version =
        SessionCursorVersionV1::new(key_version_value).map_err(|_| CursorError::Malformed)?;
    if key_id_hex != hex::encode(key_id.as_str().as_bytes())
        || key_version_text != key_version.value().to_string()
    {
        return Err(CursorError::Malformed);
    }
    let routed_key = SignedCursorKeyRefV1 {
        key_id,
        version: key_version,
    };

    let authenticated = format!("{version}.{key_id_hex}.{key_version_text}.{payload_hex}");
    let signature = CursorSignature::from_hex(signature_hex).map_err(|_| CursorError::Malformed)?;
    if signature_hex != signature.to_hex() {
        return Err(CursorError::Malformed);
    }
    authenticator
        .verify(&routed_key, authenticated.as_bytes(), &signature)
        .map_err(map_key_error)?;
    let payload_bytes = hex::decode(payload_hex).map_err(|_| CursorError::Malformed)?;
    let payload: CursorPayload =
        serde_json::from_slice(&payload_bytes).map_err(|_| CursorError::Malformed)?;
    let canonical = serde_json::to_vec(&payload).map_err(|_| CursorError::Malformed)?;
    if canonical != payload_bytes || payload_hex != hex::encode(&payload_bytes) {
        return Err(CursorError::Malformed);
    }
    verify_validity_window(&payload, now_micros)?;
    let expected_key = expected.cursor_key().ok_or(CursorError::KeyUnavailable)?;
    if routed_key.key_id != expected_key.key_id {
        return Err(CursorError::KeyIdMismatch);
    }
    if routed_key.version != expected_key.version {
        return Err(CursorError::KeyVersionMismatch);
    }
    verify_bindings(&payload, expected)?;
    validate_sort_key(&payload.last_sort_key)?;
    Ok(payload.last_sort_key)
}

pub fn verify_cursor_for_sort_key(
    encoded: &str,
    expected: &TemporalExecutionSnapshot,
    expected_sort_key: &StableSortKey,
    authenticator: &(impl SessionCursorAuthenticator + ?Sized),
) -> Result<(), CursorError> {
    let actual = verify_cursor(encoded, expected, authenticator)?;
    if &actual != expected_sort_key {
        return Err(CursorError::SortKeyMismatch);
    }
    Ok(())
}

fn verify_bindings(
    payload: &CursorPayload,
    expected: &TemporalExecutionSnapshot,
) -> Result<(), CursorError> {
    if payload.root_digest != expected.root_digest().as_str() {
        return Err(CursorError::RootMismatch);
    }
    let expected_scope = expected.request().retrieval_scope();
    if payload.scope_kind != cursor_scope_kind(expected_scope) {
        return Err(CursorError::SessionMismatch);
    }
    if payload.session_id.as_deref() != expected_scope.session_id().map(SessionId::as_str) {
        return Err(CursorError::SessionMismatch);
    }
    if payload.provider_scope.as_deref() != expected.provider_scope() {
        return Err(CursorError::WrongRequest);
    }
    if payload.request_digest != expected.request_digest().as_str() {
        return Err(CursorError::WrongRequest);
    }
    if payload.filter_digest != expected.filter_digest().as_str() {
        return Err(CursorError::FilterMismatch);
    }
    if payload.access_digest != expected.access_digest().as_str() {
        return Err(CursorError::WrongAccess);
    }
    if payload.temporal_mode != expected.temporal_mode().as_str()
        || payload.cutoff_micros != temporal_cutoff(expected.temporal_mode())
    {
        return Err(CursorError::TemporalModeMismatch);
    }
    if payload.grain != expected.grain().as_str() {
        return Err(CursorError::GrainMismatch);
    }
    let expected_watermarks = expected.watermarks();
    if payload.generation != expected_watermarks.generation {
        return Err(CursorError::GenerationMismatch);
    }
    if payload.source_watermark != expected_watermarks.source {
        return Err(CursorError::SourceWatermarkMismatch);
    }
    if payload.projection_watermark != expected_watermarks.projection {
        return Err(CursorError::ProjectionWatermarkMismatch);
    }
    if payload.index_watermark != expected_watermarks.index {
        return Err(CursorError::IndexWatermarkMismatch);
    }
    if payload.summary_watermark != expected_watermarks.summary {
        return Err(CursorError::SummaryWatermarkMismatch);
    }
    if &payload.participant_manifest != expected.participant_manifest() {
        return Err(CursorError::ParticipantManifestMismatch);
    }
    if payload.epoch_digest != expected.participant_manifest().epoch_digest() {
        return Err(CursorError::EpochMismatch);
    }
    if payload.schema_version != expected.versions().schema {
        return Err(CursorError::SchemaMismatch);
    }
    if payload.ranking_version != expected.versions().ranking {
        return Err(CursorError::RankingMismatch);
    }
    if payload.configuration_digest != expected.versions().configuration_digest.as_str() {
        return Err(CursorError::ConfigurationMismatch);
    }
    Ok(())
}

fn cursor_scope_kind(scope: &TemporalRetrievalScope) -> CursorScopeKind {
    match scope {
        TemporalRetrievalScope::Session(_) => CursorScopeKind("session".to_string()),
        TemporalRetrievalScope::AllSessionsInAuthorizedRoot => {
            CursorScopeKind("all_sessions_in_authorized_root".to_string())
        }
    }
}

fn validate_sort_key(sort_key: &StableSortKey) -> Result<(), CursorError> {
    if sort_key.stable_id.is_empty()
        || sort_key.stable_id.len() > MAX_SORT_KEY_STABLE_ID_BYTES
        || sort_key.stable_id.chars().any(char::is_control)
    {
        return Err(CursorError::SortKeyMismatch);
    }
    Ok(())
}

fn verify_validity_window(payload: &CursorPayload, now_micros: i64) -> Result<(), CursorError> {
    let expected_expiry = payload
        .issued_at_micros
        .checked_add(CURSOR_LIFETIME_MICROS)
        .ok_or(CursorError::Expired)?;
    let latest_accepted_issue = now_micros.saturating_add(CURSOR_CLOCK_SKEW_MICROS);
    if payload.issued_at_micros < 0
        || payload.expires_at_micros != expected_expiry
        || payload.issued_at_micros > latest_accepted_issue
        || now_micros >= payload.expires_at_micros
    {
        return Err(CursorError::Expired);
    }
    Ok(())
}

fn now_micros() -> Result<i64, CursorError> {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CursorError::Malformed)?
        .as_micros();
    i64::try_from(micros).map_err(|_| CursorError::Malformed)
}

const fn temporal_cutoff(mode: TemporalModeV1) -> Option<i64> {
    match mode {
        TemporalModeV1::AsOf { cutoff } => Some(cutoff.0),
        TemporalModeV1::Current | TemporalModeV1::Evolution | TemporalModeV1::Forensic => None,
    }
}

const fn map_key_error(error: CursorKeyError) -> CursorError {
    match error {
        CursorKeyError::Unavailable => CursorError::UnknownOrExpiredKey,
        CursorKeyError::InvalidMaterial => CursorError::InvalidKeyMaterial,
        CursorKeyError::AuthenticationFailed => CursorError::Tampered,
    }
}

#[cfg(test)]
mod tests {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    use tracedecay_domain::{
        RetrievalGrainV1, SessionCursorKeyIdV1, SessionCursorVersionV1, SessionId,
        SignedCursorKeyRefV1, TemporalModeV1,
    };

    use super::*;
    use crate::temporal::ports::{
        BindingDigest, CursorKeyError, CursorSignature, KernelVersions, SessionCursorAuthenticator,
        TemporalExecutionSnapshot, TemporalParticipantAuthorization, TemporalParticipantGeneration,
        TemporalParticipantManifest, TemporalSnapshotRequest, TemporalSourceAccess,
        TemporalWatermarks,
    };

    const TEST_NOW_MICROS: i64 = 1_800_000_000_000_000;

    struct KeyAuth {
        key: SignedCursorKeyRefV1,
        secret: [u8; 32],
    }

    impl KeyAuth {
        fn new(key: SignedCursorKeyRefV1, secret: [u8; 32]) -> Self {
            Self { key, secret }
        }
    }

    impl SessionCursorAuthenticator for KeyAuth {
        fn sign(
            &self,
            key: &SignedCursorKeyRefV1,
            authenticated: &[u8],
        ) -> Result<CursorSignature, CursorKeyError> {
            if key != &self.key {
                return Err(CursorKeyError::Unavailable);
            }
            let mut mac =
                <Hmac<Sha256> as KeyInit>::new_from_slice(&self.secret).expect("valid test key");
            mac.update(authenticated);
            Ok(
                CursorSignature::from_hex(&hex::encode(mac.finalize().into_bytes()))
                    .expect("valid signature"),
            )
        }

        fn verify(
            &self,
            key: &SignedCursorKeyRefV1,
            authenticated: &[u8],
            signature: &CursorSignature,
        ) -> Result<(), CursorKeyError> {
            let expected = self.sign(key, authenticated)?;
            if expected == *signature {
                Ok(())
            } else {
                Err(CursorKeyError::AuthenticationFailed)
            }
        }
    }

    fn digest(byte: char) -> String {
        format!("sha256:{}", byte.to_string().repeat(64))
    }

    fn snapshot_for(session: &str, access: char, projection: u64) -> TemporalExecutionSnapshot {
        snapshot_for_key(session, access, projection, "key-1", 1)
    }

    fn snapshot_for_key(
        session: &str,
        access: char,
        projection: u64,
        key_id: &str,
        key_version: u16,
    ) -> TemporalExecutionSnapshot {
        let session_id: SessionId =
            serde_json::from_str(&format!("\"{session}\"")).expect("valid session id");
        let request = TemporalSnapshotRequest::new(
            session_id,
            digest('0'),
            digest('1'),
            digest(access),
            TemporalModeV1::AsOf {
                cutoff: tracedecay_domain::UtcMicros(42),
            },
            RetrievalGrainV1::Turn,
        )
        .expect("valid request");
        TemporalExecutionSnapshot::new(
            request,
            TemporalWatermarks {
                generation: 7,
                source: 11,
                projection,
                index: 17,
                summary: 19,
            },
            KernelVersions {
                schema: 3,
                ranking: 5,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            Some(SignedCursorKeyRefV1 {
                key_id: SessionCursorKeyIdV1::new(key_id).expect("valid key id"),
                version: SessionCursorVersionV1::new(key_version).expect("valid key version"),
            }),
        )
        .expect("valid snapshot")
    }

    fn snapshot(access: char, projection: u64) -> TemporalExecutionSnapshot {
        snapshot_for("session-1", access, projection)
    }

    fn auth(secret: u8) -> KeyAuth {
        KeyAuth::new(
            snapshot('2', 13)
                .cursor_key()
                .expect("snapshot key")
                .clone(),
            [secret; 32],
        )
    }

    fn sort_key() -> StableSortKey {
        StableSortKey {
            normalized_score_micros: 875_000,
            knowledge_at_micros: 42,
            stable_id: "anchor-9".to_string(),
        }
    }

    fn participant_manifest(generation: u64) -> TemporalParticipantManifest {
        TemporalParticipantManifest::new(vec![
            TemporalParticipantGeneration::new(
                SessionId::new("session-1").expect("session"),
                "source-1",
                TemporalWatermarks {
                    generation,
                    source: 11,
                    projection: 13,
                    index: 17,
                    summary: 19,
                },
                23,
                &BindingDigest::new("configuration", digest('3')).expect("configuration"),
                &BindingDigest::new("authorization", digest('2')).expect("authorization"),
                TemporalParticipantAuthorization::Authorized,
                TemporalSourceAccess::Available,
            )
            .expect("participant"),
        ])
        .expect("manifest")
    }

    fn resign(
        authenticated: &str,
        key_ref: &SignedCursorKeyRefV1,
        authenticator: &impl SessionCursorAuthenticator,
    ) -> String {
        let signature = authenticator
            .sign(key_ref, authenticated.as_bytes())
            .expect("test signing");
        format!("{authenticated}.{}", signature.to_hex())
    }

    fn mutate_and_resign(
        encoded: &str,
        key_ref: &SignedCursorKeyRefV1,
        authenticator: &impl SessionCursorAuthenticator,
        mutate: impl FnOnce(&mut CursorPayload),
    ) -> String {
        let mut parts = encoded.split('.');
        let version = parts.next().expect("version");
        let key_id = parts.next().expect("key id");
        let key_version = parts.next().expect("key version");
        let payload_hex = parts.next().expect("payload");
        let bytes = hex::decode(payload_hex).expect("payload hex");
        let mut payload: CursorPayload = serde_json::from_slice(&bytes).expect("payload json");
        mutate(&mut payload);
        let canonical = serde_json::to_vec(&payload).expect("canonical payload");
        resign(
            &format!(
                "{version}.{key_id}.{key_version}.{}",
                hex::encode(canonical)
            ),
            key_ref,
            authenticator,
        )
    }

    #[test]
    fn cursor_round_trip_is_restart_stable_and_canonical() {
        let provider = auth(7);
        let encoded = encode_cursor_at(&snapshot('2', 13), &sort_key(), &provider, TEST_NOW_MICROS)
            .expect("encode");
        assert_eq!(encoded.split('.').count(), 5);

        let restarted_auth = auth(7);
        let decoded = verify_cursor_at(
            &encoded,
            &snapshot('2', 13),
            &restarted_auth,
            TEST_NOW_MICROS,
        )
        .expect("same persisted key verifies after restart");
        assert_eq!(decoded, sort_key());
    }

    #[test]
    fn cursor_expiry_is_bounded_and_skew_is_limited() {
        let provider = auth(8);
        let expected = snapshot('2', 13);
        let encoded =
            encode_cursor_at(&expected, &sort_key(), &provider, TEST_NOW_MICROS).expect("encode");
        let payload_hex = encoded.split('.').nth(3).expect("payload");
        let payload: CursorPayload =
            serde_json::from_slice(&hex::decode(payload_hex).expect("payload hex"))
                .expect("payload json");
        assert_eq!(payload.issued_at_micros, TEST_NOW_MICROS);
        assert_eq!(
            payload.expires_at_micros,
            TEST_NOW_MICROS + CURSOR_LIFETIME_MICROS
        );
        assert_eq!(
            verify_cursor_at(
                &encoded,
                &expected,
                &provider,
                payload.expires_at_micros - 1,
            ),
            Ok(sort_key())
        );
        assert_eq!(
            verify_cursor_at(&encoded, &expected, &provider, payload.expires_at_micros,),
            Err(CursorError::Expired)
        );
        assert_eq!(
            verify_cursor_at(
                &encoded,
                &expected,
                &provider,
                TEST_NOW_MICROS - CURSOR_CLOCK_SKEW_MICROS,
            ),
            Ok(sort_key())
        );
        assert_eq!(
            verify_cursor_at(
                &encoded,
                &expected,
                &provider,
                TEST_NOW_MICROS - CURSOR_CLOCK_SKEW_MICROS - 1,
            ),
            Err(CursorError::Expired)
        );

        let key_ref = expected.cursor_key().expect("snapshot key");
        let overlong = mutate_and_resign(&encoded, key_ref, &provider, |payload| {
            payload.expires_at_micros += 1;
        });
        assert_eq!(
            verify_cursor_at(&overlong, &expected, &provider, TEST_NOW_MICROS),
            Err(CursorError::Expired)
        );
    }

    #[test]
    fn root_wide_cursor_is_restart_stable_and_scope_is_unambiguous() {
        let provider = auth(7);
        let session_snapshot = snapshot('2', 13);
        let session_id = SessionId::new("compatibility-session").expect("valid session");
        let request = TemporalSnapshotRequest::new(
            session_id,
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::AsOf {
                cutoff: tracedecay_domain::UtcMicros(42),
            },
            RetrievalGrainV1::Turn,
        )
        .expect("valid request")
        .with_retrieval_scope(TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
        let root_snapshot = TemporalExecutionSnapshot::new(
            request,
            TemporalWatermarks {
                generation: 7,
                source: 11,
                projection: 13,
                index: 17,
                summary: 19,
            },
            KernelVersions {
                schema: 3,
                ranking: 5,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            session_snapshot.cursor_key().cloned(),
        )
        .expect("valid root snapshot");
        let encoded = encode_cursor(&root_snapshot, &sort_key(), &provider).expect("encode");

        let payload_hex = encoded.split('.').nth(3).expect("payload");
        let payload: CursorPayload =
            serde_json::from_slice(&hex::decode(payload_hex).expect("payload hex"))
                .expect("payload json");
        assert_eq!(
            payload.scope_kind,
            cursor_scope_kind(&TemporalRetrievalScope::AllSessionsInAuthorizedRoot)
        );
        assert_eq!(payload.session_id, None);
        assert_eq!(
            root_snapshot.retrieval_scope(),
            &TemporalRetrievalScope::AllSessionsInAuthorizedRoot
        );

        let restarted_auth = auth(7);
        assert_eq!(
            verify_cursor(&encoded, &root_snapshot, &restarted_auth),
            Ok(sort_key())
        );
        assert_eq!(
            verify_cursor(&encoded, &session_snapshot, &restarted_auth),
            Err(CursorError::SessionMismatch)
        );
    }

    #[test]
    fn cursor_tampering_is_rejected_before_binding_checks() {
        let auth = auth(9);
        let encoded = encode_cursor(&snapshot('2', 13), &sort_key(), &auth).expect("encode");
        let mut parts = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        parts[3].push('0');
        let tampered = parts.join(".");

        assert_eq!(
            verify_cursor(&tampered, &snapshot('4', 99), &auth),
            Err(CursorError::Tampered)
        );
    }

    #[test]
    fn cursor_distinguishes_access_and_watermark_drift() {
        let auth = auth(11);
        let encoded = encode_cursor(&snapshot('2', 13), &sort_key(), &auth).expect("encode");

        assert_eq!(
            verify_cursor(&encoded, &snapshot('4', 13), &auth),
            Err(CursorError::WrongAccess)
        );
        assert_eq!(
            verify_cursor(&encoded, &snapshot('2', 99), &auth),
            Err(CursorError::ProjectionWatermarkMismatch)
        );
        assert_eq!(
            verify_cursor(&encoded, &snapshot_for("session-2", '2', 13), &auth),
            Err(CursorError::SessionMismatch)
        );
    }

    #[test]
    fn cursor_binds_filters_participant_manifest_and_epoch_independently() {
        let auth = auth(31);
        let expected = snapshot('2', 13)
            .with_participant_manifest(participant_manifest(7))
            .expect("manifest");
        let encoded = encode_cursor(&expected, &sort_key(), &auth).expect("encode");

        let filter_drift = TemporalExecutionSnapshot::new(
            expected
                .request()
                .clone()
                .with_filter_digest(digest('9'))
                .expect("filter"),
            expected.watermarks(),
            expected.versions().clone(),
            expected.cursor_key().cloned(),
        )
        .expect("filter snapshot")
        .with_participant_manifest(expected.participant_manifest().clone())
        .expect("filter manifest");
        assert_eq!(
            verify_cursor(&encoded, &filter_drift, &auth),
            Err(CursorError::FilterMismatch)
        );

        let participant_drift = snapshot('2', 13)
            .with_participant_manifest(participant_manifest(8))
            .expect("changed manifest");
        assert_eq!(
            verify_cursor(&encoded, &participant_drift, &auth),
            Err(CursorError::ParticipantManifestMismatch)
        );
    }

    #[test]
    fn malformed_cursor_is_typed() {
        assert_eq!(
            verify_cursor("not-a-cursor", &snapshot('2', 13), &auth(1)),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn cursor_rejects_authenticated_noncanonical_hex_reencoding() {
        let auth = auth(13);
        let encoded = encode_cursor(&snapshot('2', 13), &sort_key(), &auth).expect("encode");
        let mut parts = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        parts[3] = parts[3].to_ascii_uppercase();
        let authenticated = parts[..4].join(".");
        let reencoded = resign(
            &authenticated,
            snapshot('2', 13).cursor_key().expect("snapshot key"),
            &auth,
        );

        assert_eq!(
            verify_cursor(&reencoded, &snapshot('2', 13), &auth),
            Err(CursorError::Malformed)
        );

        let mut parts = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        parts[4] = parts[4].to_ascii_uppercase();
        assert_eq!(
            verify_cursor(&parts.join("."), &snapshot('2', 13), &auth),
            Err(CursorError::Malformed)
        );
    }

    #[test]
    fn authenticated_retained_key_reports_rotation_after_mac() {
        struct CountingAuth {
            inner: KeyAuth,
            verify_calls: std::sync::atomic::AtomicUsize,
        }
        impl SessionCursorAuthenticator for CountingAuth {
            fn sign(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
            ) -> Result<CursorSignature, CursorKeyError> {
                self.inner.sign(key, authenticated)
            }
            fn verify(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
                signature: &CursorSignature,
            ) -> Result<(), CursorKeyError> {
                self.verify_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.verify(key, authenticated, signature)
            }
        }
        let auth = CountingAuth {
            inner: auth(15),
            verify_calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let encoded = encode_cursor_at(
            &snapshot('2', 13),
            &sort_key(),
            &auth.inner,
            TEST_NOW_MICROS,
        )
        .expect("encode");
        let rotated_id = snapshot_for_key("session-1", '2', 13, "key-2", 1);
        let rotated_version = snapshot_for_key("session-1", '2', 13, "key-1", 2);

        assert_eq!(
            verify_cursor_at(&encoded, &rotated_id, &auth, TEST_NOW_MICROS),
            Err(CursorError::KeyIdMismatch)
        );
        assert_eq!(
            auth.verify_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
        assert_eq!(
            verify_cursor_at(&encoded, &rotated_version, &auth, TEST_NOW_MICROS),
            Err(CursorError::KeyVersionMismatch)
        );
        assert_eq!(
            auth.verify_calls.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }

    #[test]
    fn cursor_reports_precise_projection_watermark_mismatch() {
        let auth = auth(17);
        let encoded = encode_cursor(&snapshot('2', 13), &sort_key(), &auth).expect("encode");

        assert_eq!(
            verify_cursor(&encoded, &snapshot('2', 99), &auth)
                .expect_err("projection drift must be rejected")
                .to_string(),
            "cursor projection watermark changed"
        );
    }

    #[test]
    fn cursor_reports_every_binding_drift_independently() {
        let auth = auth(19);
        let expected = snapshot('2', 13);
        let encoded = encode_cursor(&expected, &sort_key(), &auth).expect("encode");
        let key_ref = expected.cursor_key().expect("snapshot key");

        macro_rules! mismatch {
            ($mutation:expr, $expected_error:expr) => {
                assert_eq!(
                    verify_cursor(
                        &mutate_and_resign(&encoded, key_ref, &auth, $mutation),
                        &expected,
                        &auth,
                    ),
                    Err($expected_error)
                );
            };
        }

        mismatch!(
            |payload: &mut CursorPayload| payload.request_digest = digest('9'),
            CursorError::WrongRequest
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.filter_digest = digest('9'),
            CursorError::FilterMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.root_digest = digest('9'),
            CursorError::RootMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| {
                payload.scope_kind =
                    cursor_scope_kind(&TemporalRetrievalScope::AllSessionsInAuthorizedRoot);
                payload.session_id = None;
            },
            CursorError::SessionMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.session_id = Some("session-9".to_string()),
            CursorError::SessionMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.access_digest = digest('9'),
            CursorError::WrongAccess
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.cutoff_micros = Some(99),
            CursorError::TemporalModeMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.grain = "session".to_string(),
            CursorError::GrainMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.schema_version += 1,
            CursorError::SchemaMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.ranking_version += 1,
            CursorError::RankingMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.configuration_digest = digest('9'),
            CursorError::ConfigurationMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.generation += 1,
            CursorError::GenerationMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.source_watermark += 1,
            CursorError::SourceWatermarkMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.projection_watermark += 1,
            CursorError::ProjectionWatermarkMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.index_watermark += 1,
            CursorError::IndexWatermarkMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.summary_watermark += 1,
            CursorError::SummaryWatermarkMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.epoch_digest = digest('9'),
            CursorError::EpochMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| payload.last_sort_key.stable_id.clear(),
            CursorError::SortKeyMismatch
        );
        mismatch!(
            |payload: &mut CursorPayload| {
                payload.provider_scope = Some("other-provider".to_string());
            },
            CursorError::WrongRequest
        );

        let mut different_sort_key = sort_key();
        different_sort_key.stable_id = "anchor-10".to_string();
        assert_eq!(
            verify_cursor_for_sort_key(&encoded, &expected, &different_sort_key, &auth),
            Err(CursorError::SortKeyMismatch)
        );
    }

    #[test]
    fn cursor_rejects_oversized_segments_before_authentication() {
        struct CountingAuth {
            inner: KeyAuth,
            calls: std::sync::atomic::AtomicUsize,
        }
        impl SessionCursorAuthenticator for CountingAuth {
            fn sign(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
            ) -> Result<CursorSignature, CursorKeyError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.sign(key, authenticated)
            }
            fn verify(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
                signature: &CursorSignature,
            ) -> Result<(), CursorKeyError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.verify(key, authenticated, signature)
            }
        }
        let auth = CountingAuth {
            inner: auth(21),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let snapshot = snapshot('2', 13);
        let key_id = hex::encode(b"key-1");
        let oversized_payload = "ab".repeat(MAX_CURSOR_PAYLOAD_HEX_BYTES / 2 + 1);
        let forged = format!(
            "{CURSOR_FORMAT_VERSION}.{key_id}.1.{oversized_payload}.{}",
            "00".repeat(32)
        );
        assert_eq!(
            verify_cursor(&forged, &snapshot, &auth),
            Err(CursorError::Malformed)
        );
        assert_eq!(auth.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let oversized_key = "ab".repeat(MAX_CURSOR_KEY_ID_HEX_BYTES / 2 + 1);
        let forged_key = format!(
            "{CURSOR_FORMAT_VERSION}.{oversized_key}.1.abcd.{}",
            "00".repeat(32)
        );
        assert_eq!(
            verify_cursor(&forged_key, &snapshot, &auth),
            Err(CursorError::Malformed)
        );
        assert_eq!(auth.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn cursor_rejects_invalid_sort_keys_on_encode_and_verify() {
        let auth = auth(23);
        let expected = snapshot('2', 13);
        for bad in [
            StableSortKey {
                normalized_score_micros: 1,
                knowledge_at_micros: 1,
                stable_id: String::new(),
            },
            StableSortKey {
                normalized_score_micros: 1,
                knowledge_at_micros: 1,
                stable_id: "has\0control".to_string(),
            },
            StableSortKey {
                normalized_score_micros: 1,
                knowledge_at_micros: 1,
                stable_id: "x".repeat(MAX_SORT_KEY_STABLE_ID_BYTES + 1),
            },
        ] {
            assert_eq!(
                encode_cursor(&expected, &bad, &auth),
                Err(CursorError::SortKeyMismatch)
            );
        }

        let encoded = encode_cursor(&expected, &sort_key(), &auth).expect("encode");
        let key_ref = expected.cursor_key().expect("snapshot key");
        let mutated = mutate_and_resign(&encoded, key_ref, &auth, |payload| {
            payload.last_sort_key.stable_id = "x".repeat(MAX_SORT_KEY_STABLE_ID_BYTES + 1);
        });
        assert_eq!(
            verify_cursor(&mutated, &expected, &auth),
            Err(CursorError::SortKeyMismatch)
        );
    }

    #[test]
    fn cursor_rejects_noncanonical_route_encoding_before_mac() {
        struct CountingAuth {
            inner: KeyAuth,
            calls: std::sync::atomic::AtomicUsize,
        }
        impl SessionCursorAuthenticator for CountingAuth {
            fn sign(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
            ) -> Result<CursorSignature, CursorKeyError> {
                self.inner.sign(key, authenticated)
            }
            fn verify(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
                signature: &CursorSignature,
            ) -> Result<(), CursorKeyError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.verify(key, authenticated, signature)
            }
        }
        let auth = CountingAuth {
            inner: auth(25),
            calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let encoded = encode_cursor(&snapshot('2', 13), &sort_key(), &auth.inner).expect("encode");
        let mut parts = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        parts[1] = parts[1].to_ascii_uppercase();
        assert_eq!(
            verify_cursor(&parts.join("."), &snapshot('2', 13), &auth),
            Err(CursorError::Malformed)
        );
        assert_eq!(auth.calls.load(std::sync::atomic::Ordering::SeqCst), 0);

        let mut parts = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        parts[2] = format!("0{}", parts[2]);
        let resigned = resign(
            &parts[..4].join("."),
            snapshot('2', 13).cursor_key().expect("key"),
            &auth.inner,
        );
        assert_eq!(
            verify_cursor(&resigned, &snapshot('2', 13), &auth),
            Err(CursorError::Malformed)
        );
        assert_eq!(auth.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn provider_scoped_cursor_binds_exact_provider_and_rejects_drift() {
        let provider = auth(27);
        let session_id: SessionId =
            serde_json::from_str("\"session-1\"").expect("valid session id");
        let request = TemporalSnapshotRequest::new(
            session_id,
            digest('0'),
            digest('1'),
            digest('2'),
            TemporalModeV1::AsOf {
                cutoff: tracedecay_domain::UtcMicros(42),
            },
            RetrievalGrainV1::Turn,
        )
        .expect("valid request")
        .with_provider_scope(Some("claude".to_string()))
        .expect("provider");
        let scoped = TemporalExecutionSnapshot::new(
            request,
            TemporalWatermarks {
                generation: 7,
                source: 11,
                projection: 13,
                index: 17,
                summary: 19,
            },
            KernelVersions {
                schema: 3,
                ranking: 5,
                configuration_digest: BindingDigest::new("configuration_digest", digest('3'))
                    .expect("valid digest"),
            },
            Some(SignedCursorKeyRefV1 {
                key_id: SessionCursorKeyIdV1::new("key-1").expect("valid key id"),
                version: SessionCursorVersionV1::new(1).expect("valid key version"),
            }),
        )
        .expect("scoped snapshot");
        let encoded = encode_cursor(&scoped, &sort_key(), &provider).expect("encode");
        assert_eq!(verify_cursor(&encoded, &scoped, &provider), Ok(sort_key()));
        assert_eq!(
            verify_cursor(&encoded, &snapshot('2', 13), &provider),
            Err(CursorError::WrongRequest)
        );
    }

    #[test]
    fn unknown_routes_and_tampering_do_not_disclose_rotation() {
        struct CountingAuth {
            inner: KeyAuth,
            verify_calls: std::sync::atomic::AtomicUsize,
        }
        impl SessionCursorAuthenticator for CountingAuth {
            fn sign(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
            ) -> Result<CursorSignature, CursorKeyError> {
                self.inner.sign(key, authenticated)
            }
            fn verify(
                &self,
                key: &SignedCursorKeyRefV1,
                authenticated: &[u8],
                signature: &CursorSignature,
            ) -> Result<(), CursorKeyError> {
                self.verify_calls
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.inner.verify(key, authenticated, signature)
            }
        }
        let auth = CountingAuth {
            inner: auth(29),
            verify_calls: std::sync::atomic::AtomicUsize::new(0),
        };
        let encoded = encode_cursor_at(
            &snapshot('2', 13),
            &sort_key(),
            &auth.inner,
            TEST_NOW_MICROS,
        )
        .expect("encode");
        let mut unknown_route = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        unknown_route[1] = hex::encode("unknown-key");
        assert_eq!(
            verify_cursor_at(
                &unknown_route.join("."),
                &snapshot_for_key("session-1", '2', 13, "key-2", 1),
                &auth,
                TEST_NOW_MICROS,
            ),
            Err(CursorError::UnknownOrExpiredKey)
        );
        assert_eq!(
            auth.verify_calls.load(std::sync::atomic::Ordering::SeqCst),
            1
        );

        let mut tampered = encoded.split('.').map(str::to_string).collect::<Vec<_>>();
        tampered[3].push('0');
        assert_eq!(
            verify_cursor_at(
                &tampered.join("."),
                &snapshot_for_key("session-1", '2', 13, "key-2", 1),
                &auth,
                TEST_NOW_MICROS,
            ),
            Err(CursorError::Tampered)
        );
        assert_eq!(
            auth.verify_calls.load(std::sync::atomic::Ordering::SeqCst),
            2
        );
    }
}
