use super::extraction;
use super::types::LcmExtractionResult;
use super::{
    LcmRawMessage, LcmSummarizerMode, LcmSummaryRequest, LcmSummarySourceMessage,
    LcmSummarySourceRange,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistedSummaryInvocation {
    pub(crate) summary_text: String,
    pub(crate) route: Option<String>,
    pub(crate) extraction_result: Option<LcmExtractionResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CompressionSummarizerAdapter {
    Noop,
    HermesAuxiliary,
    Persisted(PersistedSummaryInvocation),
}

impl CompressionSummarizerAdapter {
    pub(crate) fn from_mode(mode: LcmSummarizerMode) -> Self {
        match mode {
            LcmSummarizerMode::Noop => Self::Noop,
            LcmSummarizerMode::HermesAuxiliary => Self::HermesAuxiliary,
            LcmSummarizerMode::Fake { summary_text } => {
                Self::Persisted(PersistedSummaryInvocation {
                    summary_text,
                    route: None,
                    extraction_result: None,
                })
            }
            LcmSummarizerMode::Provided {
                summary_text,
                route,
            } => {
                let (route, extraction_result) = extraction::split_summary_route(route.as_deref());
                Self::Persisted(PersistedSummaryInvocation {
                    summary_text,
                    route,
                    extraction_result,
                })
            }
        }
    }

    pub(crate) fn is_noop(&self) -> bool {
        matches!(self, Self::Noop)
    }

    pub(crate) fn persisted_summary_invocation(&self) -> Option<&PersistedSummaryInvocation> {
        match self {
            Self::Persisted(invocation) => Some(invocation),
            Self::Noop | Self::HermesAuxiliary => None,
        }
    }

    pub(crate) fn summary_request(
        &self,
        provider: &str,
        session_id: &str,
        focus_topic: Option<String>,
        backlog: &[LcmRawMessage],
    ) -> Option<LcmSummaryRequest> {
        match self {
            Self::HermesAuxiliary => Some(summary_request_for_backlog(
                provider,
                session_id,
                focus_topic,
                backlog,
            )),
            Self::Noop | Self::Persisted(_) => None,
        }
    }
}

fn summary_request_for_backlog(
    provider: &str,
    session_id: &str,
    focus_topic: Option<String>,
    backlog: &[LcmRawMessage],
) -> LcmSummaryRequest {
    let first_store_id = backlog.first().map_or(0, |message| message.store_id);
    let last_store_id = backlog.last().map_or(0, |message| message.store_id);
    let source_range = LcmSummarySourceRange {
        from_store_id: first_store_id,
        to_store_id: last_store_id,
    };
    let source_messages = backlog
        .iter()
        .map(|message| LcmSummarySourceMessage {
            store_id: message.store_id,
            role: message.role.clone(),
            content: message.content.clone(),
        })
        .collect::<Vec<_>>();
    let focus = focus_topic.as_deref().unwrap_or("the conversation so far");
    let prompt = format!(
        "Summarize LCM raw messages for provider '{provider}', session '{session_id}', \
         store_id range {first_store_id}..={last_store_id}. Focus on {focus}. \
         Preserve durable instructions, decisions, open tasks, and facts needed to continue."
    );

    LcmSummaryRequest {
        provider: provider.to_string(),
        session_id: session_id.to_string(),
        focus_topic,
        prompt,
        source_range: source_range.clone(),
        source_messages: source_messages.clone(),
        extraction_request: extraction::build_extraction_request(
            session_id,
            &source_range,
            &source_messages,
        ),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::sessions::lcm::{LcmRawMessage, LcmStorageKind, LcmSummarizerMode};

    fn raw_message(store_id: i64, role: &str, content: &str) -> LcmRawMessage {
        LcmRawMessage {
            provider: "cursor".into(),
            message_id: format!("message-{store_id}"),
            session_id: "session-1".into(),
            store_id,
            role: role.into(),
            ordinal: store_id,
            timestamp: Some(1_715_000_000 + store_id),
            content: content.into(),
            content_hash: format!("hash-{store_id}"),
            storage_kind: LcmStorageKind::Inline,
            payload_ref: None,
            legacy_source: false,
            legacy_truncated: false,
            metadata_json: None,
        }
    }

    #[test]
    fn noop_mode_selects_noop_adapter() {
        let adapter = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Noop);

        assert!(adapter.is_noop());
        assert!(adapter.persisted_summary_invocation().is_none());
        assert!(adapter
            .summary_request("cursor", "session-1", None, &[])
            .is_none());
    }

    #[test]
    fn fake_mode_selects_persisted_summary_without_route_metadata() {
        let adapter = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Fake {
            summary_text: "fake summary".into(),
        });

        assert!(!adapter.is_noop());
        let invocation = adapter
            .persisted_summary_invocation()
            .expect("fake mode should persist a summary");
        assert_eq!(
            invocation,
            &PersistedSummaryInvocation {
                summary_text: "fake summary".into(),
                route: None,
                extraction_result: None,
            }
        );
    }

    #[test]
    fn provided_mode_selects_persisted_summary_and_splits_route_envelope() {
        let adapter = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::Provided {
            summary_text: "provided summary".into(),
            route: Some(
                json!({
                    "route": "backup",
                    "pre_compaction_extraction": {
                        "status": "ok",
                        "items": ["Decision: keep nightly backups"],
                        "model": "openai/gpt-5.4-mini"
                    }
                })
                .to_string(),
            ),
        });

        assert_eq!(
            adapter
                .persisted_summary_invocation()
                .expect("provided mode should persist a summary"),
            &PersistedSummaryInvocation {
                summary_text: "provided summary".into(),
                route: Some("backup".into()),
                extraction_result: Some(LcmExtractionResult {
                    status: "ok".into(),
                    items: vec!["Decision: keep nightly backups".into()],
                    text: None,
                    model: Some("openai/gpt-5.4-mini".into()),
                    output_path: None,
                    error: None,
                }),
            }
        );
    }

    #[test]
    fn hermes_auxiliary_mode_builds_summary_request_from_backlog_inputs() {
        let adapter = CompressionSummarizerAdapter::from_mode(LcmSummarizerMode::HermesAuxiliary);
        let backlog = vec![
            raw_message(11, "assistant", "old-1"),
            raw_message(12, "assistant", "old-2"),
        ];

        let request = adapter
            .summary_request("cursor", "session-1", Some("billing".into()), &backlog)
            .expect("auxiliary mode should request a summary");

        assert_eq!(request.provider, "cursor");
        assert_eq!(request.session_id, "session-1");
        assert_eq!(request.focus_topic.as_deref(), Some("billing"));
        assert!(request.prompt.contains("session-1"));
        assert!(request.prompt.contains("billing"));
        assert_eq!(request.source_range.from_store_id, 11);
        assert_eq!(request.source_range.to_store_id, 12);
        assert_eq!(
            request
                .source_messages
                .iter()
                .map(|message| {
                    (
                        message.store_id,
                        message.role.as_str(),
                        message.content.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            vec![(11, "assistant", "old-1"), (12, "assistant", "old-2")]
        );
        let extraction_request = request
            .extraction_request
            .expect("auxiliary request should include extraction request");
        assert_eq!(extraction_request.session_id, "session-1");
        assert_eq!(extraction_request.source_range, request.source_range);
        assert!(extraction_request.prompt.contains("NOTHING_TO_EXTRACT"));
        assert!(extraction_request.prompt.contains("[ASSISTANT]: old-1"));
        assert!(extraction_request.prompt.contains("[ASSISTANT]: old-2"));
    }
}
