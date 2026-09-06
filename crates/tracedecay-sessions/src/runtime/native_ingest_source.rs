//! Shared native ingest source identity for admission and cursor lookup.
//!
//! Each host writes one [`ObservationSourceIdentityV1`] per independently
//! ordered stream. Cursor lookup and projection fixtures must reconstruct that
//! same identity instead of collapsing every stream of a session onto
//! `for_provider`.
//!
//! Every host but one commits under the plain provider/session pair, so this
//! is a constructor rather than a registry. Codex is the exception, and it is
//! a deliberate one: its observations commit under a hashed v2 identity, and
//! `for_provider` names only the pre-v2 legacy source that replay reconciles,
//! so a lookup that reconstructed the pair would read the wrong cursor. The
//! knowledge of what that identity is stays in the Codex host module
//! ([`crate::runtime::codex::codex_observation_source_v2`]); only the routing
//! to it lives here, because a caller holding just a provider string has no
//! other way to reach the host that owns the answer.

use tracedecay_domain::{ObservationSourceIdentityV1, ProviderId, SessionId};

use crate::runtime::source::TranscriptIngestResult;

/// Source identity admission writes for one native stream.
///
/// `source_key` names an independently appended stream inside the session
/// (a Cline task's `<task>:ui_messages`, from
/// [`crate::runtime::cline_like::ui_messages_source_key`]). `None` keeps the
/// session's own single-source identity.
pub fn native_ingest_source_identity(
    provider: &str,
    session_id: &str,
    source_key: Option<&str>,
) -> TranscriptIngestResult<ObservationSourceIdentityV1> {
    // See the module doc: Codex, and only Codex, commits under an identity the
    // provider/session pair cannot reconstruct.
    if provider == "codex" && source_key.is_none() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::cline_like::ui_messages_source_key;
    use crate::runtime::shared::path_identity_key;

    #[test]
    fn cline_ui_stream_stays_independent_of_the_api_session_source() {
        let api = native_ingest_source_identity("cline", "task-1", None).unwrap();
        let ui = native_ingest_source_identity(
            "cline",
            "task-1",
            Some(&ui_messages_source_key("task-1")),
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
        let looked_up = native_ingest_source_identity("codex", "codex-goal-dedupe", None).unwrap();
        let legacy = tracedecay_domain::ObservationSourceIdentityV1::for_provider(
            tracedecay_domain::ProviderId::new("codex").unwrap(),
            SessionId::new("codex-goal-dedupe".to_owned()).unwrap(),
        )
        .unwrap();
        assert_eq!(written, looked_up);
        assert_ne!(written, legacy);
        assert!(written.explicit_source_key().is_some());
    }

    #[test]
    fn path_identity_collapses_only_windows_display_forms() {
        assert_eq!(
            path_identity_key(r"C:\Users\agent\task\api_conversation_history.json"),
            "c:/Users/agent/task/api_conversation_history.json",
        );
        assert_eq!(
            path_identity_key(r"\\?\C:\Users\agent\task\ui_messages.json"),
            path_identity_key(r"c:/Users/agent/task/ui_messages.json"),
        );
        // Case outside the drive letter is preserved: the same column carries
        // case-sensitive opaque cursor keys.
        assert_eq!(
            path_identity_key("claude-cursor-unix-bytes-AB12cd"),
            "claude-cursor-unix-bytes-AB12cd",
        );
        assert_ne!(
            path_identity_key("task-1"),
            path_identity_key("task-1:ui_messages")
        );
        // Idempotent: normalising a stored key again is the identity.
        let stored = path_identity_key(r"\\?\D:\Work\repo\session.jsonl");
        assert_eq!(path_identity_key(&stored), stored);
    }
}
