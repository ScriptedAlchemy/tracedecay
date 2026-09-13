//! Shared native ingest source identity for admission and cursor lookup.
//!
//! Each host writes one [`ObservationSourceIdentityV1`] per independently
//! ordered stream. Cursor lookup and projection fixtures must reconstruct that
//! same identity instead of collapsing every stream of a session onto
//! `for_provider`.
//!
use tracedecay_domain::{
    ClineTranscriptStream, ObservationSourceIdentityV1, ProviderId, SessionId,
};

use crate::runtime::source::TranscriptIngestResult;

/// Source identity admission writes for one native stream.
///
/// `source_key` identifies an explicit native stream. Cline-family lookups
/// without a stream select API history through the canonical domain authority;
/// Codex selects its hashed v2 source. Other hosts retain their session source.
pub fn native_ingest_source_identity(
    provider: &str,
    session_id: &str,
    source_key: Option<&str>,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    if provider == "codex" && source_key.is_none() {
        return crate::runtime::codex::codex_observation_source_v2(session_id);
    }
    if matches!(provider, "cline" | "roo-code" | "kilo") && source_key.is_none() {
        return Ok(ClineTranscriptStream::ApiHistory
            .source_identity(ProviderId::new(provider)?, SessionId::new(session_id)?)?);
    }
    let provider = ProviderId::new(provider)?;
    let session_id = SessionId::new(session_id.to_string())?;
    Ok(match source_key {
        Some(source_key) => ObservationSourceIdentityV1::for_provider_source(
            provider,
            session_id,
            SessionId::new(source_key.to_string())?,
        )?,
        None => ObservationSourceIdentityV1::for_provider(provider, session_id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::{
        ClineTranscriptStream, ObservationSourceIdentityV1, ProviderId, SessionId,
    };

    #[test]
    fn cline_ui_stream_stays_independent_of_the_api_session_source() {
        let api = native_ingest_source_identity("cline", "task-1", None).unwrap();
        let ui = native_ingest_source_identity("cline", "task-1", Some("ui_messages")).unwrap();
        assert_eq!(
            api,
            ClineTranscriptStream::ApiHistory
                .source_identity(
                    ProviderId::new("cline").unwrap(),
                    SessionId::new("task-1").unwrap(),
                )
                .unwrap()
        );
        assert_eq!(
            ui,
            ClineTranscriptStream::UiMessages
                .source_identity(
                    ProviderId::new("cline").unwrap(),
                    SessionId::new("task-1").unwrap(),
                )
                .unwrap()
        );
        assert_ne!(api, ui);
        assert_eq!(api.session_id().as_str(), "task-1");
        assert_eq!(
            ui.explicit_source_key().map(SessionId::as_str),
            Some("ui_messages")
        );
    }

    #[test]
    fn cursor_and_hermes_keep_the_session_as_the_single_source() {
        for provider in ["cursor", "hermes"] {
            let source = native_ingest_source_identity(provider, "session-1", None).unwrap();
            assert_eq!(source.provider().as_str(), provider);
            assert_eq!(source.session_id().as_str(), "session-1");
            assert!(source.explicit_source_key().is_none());
        }
    }

    #[test]
    fn codex_lookup_uses_the_v2_authority_not_the_legacy_session_source() {
        let written =
            crate::runtime::codex::codex_observation_source_v2("codex-goal-dedupe").unwrap();
        let looked_up = native_ingest_source_identity("codex", "codex-goal-dedupe", None).unwrap();
        let legacy = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("codex").unwrap(),
            SessionId::new("codex-goal-dedupe").unwrap(),
        )
        .unwrap();
        assert_eq!(written, looked_up);
        assert_ne!(written, legacy);
        assert!(written.explicit_source_key().is_some());
    }
}
