use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{
    RetrievalGrainV1, SessionCursorKeyIdV1, SessionCursorVersionV1, SignedCursorKeyRefV1,
};

use crate::global_db::GlobalDbReadSnapshot;
use crate::query::temporal::ports::{CursorSignature, SessionCursorAuthenticator};

use super::cursor_keys::GlobalDbCursorKeyProvider;

const CURSOR_VERSION: &str = "2";
const CURSOR_MAX_HEX_BYTES: usize = 64 * 1024;
const CURSOR_LIFETIME_MICROS: i64 = 24 * 60 * 60 * 1_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuthorizedSessionExpandCursorBinding {
    provider: String,
    session_id: String,
    target: String,
    grain: String,
    content_offset: usize,
    content_limit: usize,
    source_limit: Option<usize>,
    authorization_digest: String,
}

impl AuthorizedSessionExpandCursorBinding {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        provider: impl Into<String>,
        session_id: impl Into<String>,
        target: impl Into<String>,
        grain: RetrievalGrainV1,
        content_offset: usize,
        content_limit: usize,
        source_limit: Option<usize>,
        authorization_digest: impl Into<String>,
    ) -> Result<Self, CompatibilityCursorError> {
        let binding = Self {
            provider: provider.into(),
            session_id: session_id.into(),
            target: target.into(),
            grain: grain.as_str().to_string(),
            content_offset,
            content_limit,
            source_limit,
            authorization_digest: authorization_digest.into(),
        };
        if [&binding.provider, &binding.session_id, &binding.target]
            .into_iter()
            .any(|value| {
                value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control)
            })
            || binding.content_limit == 0
            || binding.source_limit == Some(0)
            || !is_digest(&binding.authorization_digest)
        {
            return Err(CompatibilityCursorError::Denied);
        }
        Ok(binding)
    }

    fn session_id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpandCursorPayload {
    issued_at_micros: i64,
    binding: AuthorizedSessionExpandCursorBinding,
    generation: u64,
    source_watermark: u64,
    projection_watermark: u64,
    index_watermark: u64,
    summary_watermark: u64,
    key_ref: SignedCursorKeyRefV1,
    source_offset: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct FrozenWatermarks {
    active_generation: u64,
    cursor_key: Option<SignedCursorKeyRefV1>,
    projection_frontier: u64,
    source_frontier: u64,
    summary_frontier: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum CompatibilityCursorError {
    #[error("session compatibility cursor is unavailable")]
    Unavailable,
    #[error("session compatibility cursor was denied")]
    Denied,
}

pub(crate) async fn encode_expand_cursor(
    read: &GlobalDbReadSnapshot,
    binding: AuthorizedSessionExpandCursorBinding,
    source_offset: usize,
) -> Result<String, CompatibilityCursorError> {
    let frozen = load_frozen(read, binding.session_id()).await?;
    let key_ref = frozen
        .cursor_key
        .clone()
        .ok_or(CompatibilityCursorError::Unavailable)?;
    let provider = GlobalDbCursorKeyProvider::from_key_ref(read, key_ref.clone())
        .await
        .map_err(|_| CompatibilityCursorError::Unavailable)?;
    let payload = ExpandCursorPayload {
        issued_at_micros: now_micros(),
        binding,
        generation: frozen.active_generation,
        source_watermark: frozen.source_frontier,
        projection_watermark: frozen.projection_frontier,
        index_watermark: frozen.projection_frontier,
        summary_watermark: frozen.summary_frontier,
        key_ref: key_ref.clone(),
        source_offset,
    };
    let payload_bytes =
        serde_json::to_vec(&payload).map_err(|_| CompatibilityCursorError::Unavailable)?;
    let payload_hex = hex::encode(payload_bytes);
    let key_id_hex = hex::encode(key_ref.key_id.as_str().as_bytes());
    if payload_hex.is_empty()
        || payload_hex.len() > CURSOR_MAX_HEX_BYTES
        || key_id_hex.is_empty()
        || key_id_hex.len() > CURSOR_MAX_HEX_BYTES
    {
        return Err(CompatibilityCursorError::Unavailable);
    }
    let authenticated = format!(
        "{CURSOR_VERSION}.{key_id_hex}.{}.{}",
        key_ref.version.value(),
        payload_hex
    );
    let signature = provider
        .sign(&key_ref, authenticated.as_bytes())
        .map_err(|_| CompatibilityCursorError::Unavailable)?;
    Ok(format!("{authenticated}.{}", signature.to_hex()))
}

pub(crate) async fn decode_expand_cursor(
    read: &GlobalDbReadSnapshot,
    expected: &AuthorizedSessionExpandCursorBinding,
    encoded: &str,
) -> Result<usize, CompatibilityCursorError> {
    let mut parts = encoded.split('.');
    let version = parts.next().ok_or(CompatibilityCursorError::Denied)?;
    let key_id_hex = parts.next().ok_or(CompatibilityCursorError::Denied)?;
    let key_version = parts.next().ok_or(CompatibilityCursorError::Denied)?;
    let payload_hex = parts.next().ok_or(CompatibilityCursorError::Denied)?;
    let signature_hex = parts.next().ok_or(CompatibilityCursorError::Denied)?;
    if parts.next().is_some()
        || version != CURSOR_VERSION
        || key_id_hex.is_empty()
        || key_id_hex.len() > CURSOR_MAX_HEX_BYTES
        || payload_hex.is_empty()
        || payload_hex.len() > CURSOR_MAX_HEX_BYTES
        || signature_hex.len() != 64
    {
        return Err(CompatibilityCursorError::Denied);
    }
    let key_id = SessionCursorKeyIdV1::new(
        String::from_utf8(hex::decode(key_id_hex).map_err(|_| CompatibilityCursorError::Denied)?)
            .map_err(|_| CompatibilityCursorError::Denied)?,
    )
    .map_err(|_| CompatibilityCursorError::Denied)?;
    let version_value = key_version
        .parse::<u16>()
        .map_err(|_| CompatibilityCursorError::Denied)?;
    let key_ref = SignedCursorKeyRefV1 {
        key_id,
        version: SessionCursorVersionV1::new(version_value)
            .map_err(|_| CompatibilityCursorError::Denied)?,
    };
    let provider = GlobalDbCursorKeyProvider::from_key_ref(read, key_ref.clone())
        .await
        .map_err(|_| CompatibilityCursorError::Denied)?;
    let authenticated = format!("{CURSOR_VERSION}.{key_id_hex}.{key_version}.{payload_hex}");
    let signature =
        CursorSignature::from_hex(signature_hex).map_err(|_| CompatibilityCursorError::Denied)?;
    provider
        .verify(&key_ref, authenticated.as_bytes(), &signature)
        .map_err(|_| CompatibilityCursorError::Denied)?;

    let payload_bytes = hex::decode(payload_hex).map_err(|_| CompatibilityCursorError::Denied)?;
    let payload: ExpandCursorPayload =
        serde_json::from_slice(&payload_bytes).map_err(|_| CompatibilityCursorError::Denied)?;
    if serde_json::to_vec(&payload).map_err(|_| CompatibilityCursorError::Denied)? != payload_bytes
        || payload_hex != hex::encode(&payload_bytes)
        || payload.key_ref != key_ref
        || &payload.binding != expected
        || payload.issued_at_micros > now_micros()
        || now_micros().saturating_sub(payload.issued_at_micros) > CURSOR_LIFETIME_MICROS
    {
        return Err(CompatibilityCursorError::Denied);
    }
    let frozen = load_frozen(read, expected.session_id()).await?;
    if frozen.cursor_key.as_ref() != Some(&key_ref)
        || payload.generation != frozen.active_generation
        || payload.source_watermark != frozen.source_frontier
        || payload.projection_watermark != frozen.projection_frontier
        || payload.index_watermark != frozen.projection_frontier
        || payload.summary_watermark != frozen.summary_frontier
    {
        return Err(CompatibilityCursorError::Denied);
    }
    Ok(payload.source_offset)
}

async fn load_frozen(
    read: &GlobalDbReadSnapshot,
    session_id: &str,
) -> Result<FrozenWatermarks, CompatibilityCursorError> {
    let mut rows = read
        .query(
            "SELECT generation, frozen_watermarks_json
             FROM session_temporal_generations
             WHERE session_id = ?1 AND state = 'active'
             ORDER BY generation DESC
             LIMIT 2",
            [session_id],
        )
        .await
        .map_err(|_| CompatibilityCursorError::Unavailable)?;
    let row = rows
        .next()
        .await
        .map_err(|_| CompatibilityCursorError::Unavailable)?
        .ok_or(CompatibilityCursorError::Unavailable)?;
    let generation = u64::try_from(
        row.get::<i64>(0)
            .map_err(|_| CompatibilityCursorError::Unavailable)?,
    )
    .map_err(|_| CompatibilityCursorError::Unavailable)?;
    let encoded: String = row
        .get(1)
        .map_err(|_| CompatibilityCursorError::Unavailable)?;
    if rows
        .next()
        .await
        .map_err(|_| CompatibilityCursorError::Unavailable)?
        .is_some()
    {
        return Err(CompatibilityCursorError::Unavailable);
    }
    let frozen: FrozenWatermarks =
        serde_json::from_str(&encoded).map_err(|_| CompatibilityCursorError::Unavailable)?;
    if frozen.active_generation != generation {
        return Err(CompatibilityCursorError::Unavailable);
    }
    Ok(frozen)
}

fn is_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn now_micros() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_micros()).ok())
        .unwrap_or(i64::MAX)
}
