//! Shared native ingest source identity for admission, cursor lookup, and tests.
//!
//! Each host writes one [`ObservationSourceIdentityV1`] per independently
//! ordered stream. Cursor lookup and projection fixtures must reconstruct that
//! same identity instead of collapsing every provider onto `for_provider`.

use tracedecay_domain::{ObservationSourceIdentityV1, ProviderId, SessionId};

use crate::runtime::source::TranscriptIngestResult;

/// Source identity admission writes for one native stream.
///
/// `source_key` names an independently appended stream inside the session
/// (Cline `<task>:ui_messages`). `None` keeps the session's own single-source
/// identity. Codex always uses its v2 canonical source key; callers must not
/// substitute the pre-v2 session-only identity when reading that cursor.
pub fn native_ingest_source_identity(
    provider: &str,
    session_id: &str,
    source_key: Option<&str>,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    if provider == "codex" {
        return crate::runtime::codex::codex_observation_source_v2(session_id);
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

/// Reconstruct the identity a host would write for `provider`/`session_id`.
///
/// Prefer [`native_ingest_source_identity`] when the caller already knows the
/// explicit stream key. This helper exists for fixtures that only have the
/// session id and must still hit the same cursor admission wrote.
pub fn native_ingest_session_source_identity(
    provider: &str,
    session_id: &str,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    native_ingest_source_identity(provider, session_id, None)
}

/// Cline-family UI stream key. API history keeps the task's own identity.
#[must_use]
pub fn cline_like_ui_source_key(session_id: &str) -> String {
    format!("{session_id}:ui_messages")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::shared::{
        path_identity_eq, path_identity_key, path_identity_lookup_candidates,
    };
    use tracedecay_domain::{ObservationSourceIdentityV1, ProviderId, SessionId};

    #[test]
    fn cline_ui_stream_stays_independent_of_the_api_session_source() {
        let api = native_ingest_source_identity("cline", "task-1", None).unwrap();
        let ui = native_ingest_source_identity(
            "cline",
            "task-1",
            Some(&cline_like_ui_source_key("task-1")),
        )
        .unwrap();
        assert_ne!(api, ui);
        assert_eq!(api.session_id().as_str(), "task-1");
        assert_eq!(
            ui.explicit_source_key().map(SessionId::as_str),
            Some("task-1:ui_messages")
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
        let looked_up =
            native_ingest_session_source_identity("codex", "codex-goal-dedupe").unwrap();
        let legacy = ObservationSourceIdentityV1::for_provider(
            ProviderId::new("codex").unwrap(),
            SessionId::new("codex-goal-dedupe").unwrap(),
        )
        .unwrap();
        assert_eq!(written, looked_up);
        assert_ne!(written, legacy);
        assert!(written.explicit_source_key().is_some());
    }

    #[test]
    fn path_identity_collapses_only_windows_display_forms() {
        assert!(path_identity_eq(
            r"C:\Users\agent\task\api_conversation_history.json",
            r"c:/Users/agent/task/api_conversation_history.json",
        ));
        assert!(path_identity_eq(
            r"\\?\C:\Users\agent\task\api_conversation_history.json",
            r"C:\Users\agent\task\api_conversation_history.json",
        ));
        assert_eq!(
            path_identity_key(r"\\?\C:\Users\agent\task\ui_messages.json"),
            path_identity_key(r"c:/Users/agent/task/ui_messages.json"),
        );
        assert!(!path_identity_eq("task-1", "task-1:ui_messages"));
        assert!(
            path_identity_lookup_candidates(r"C:\Users\agent\task\api_conversation_history.json")
                .iter()
                .any(|candidate| candidate.contains('/'))
        );
    }
}
