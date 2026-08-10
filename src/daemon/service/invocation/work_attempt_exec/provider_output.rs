//! Bounded provider output accounting and provider-owned session correlation.
//!
//! Output content remains opaque except for the one protocol-defined session
//! start frame needed to bind a durable Work attempt to its owning transcript.

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt};
use tracedecay_application::{
    WorkAttemptProviderOutcomeV1, WorkAttemptStreamChannelV1, WorkAttemptStreamSummaryV1,
};
use tracedecay_domain::{
    ManifestDigest, ObservationSourceIdentityV1, ProviderId, SessionId, WorkProviderProtocol,
};

pub(super) async fn read_capped(
    stream: Option<impl AsyncRead + Unpin>,
    cap: u64,
) -> Option<(Vec<u8>, u64)> {
    let mut stream = stream?;
    let mut retained: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        match stream.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                total = total.saturating_add(read as u64);
                let room = usize::try_from(cap.saturating_sub(retained.len() as u64))
                    .unwrap_or(usize::MAX)
                    .min(read);
                retained.extend_from_slice(&buffer[..room]);
            }
        }
    }
    Some((retained, total))
}

pub(super) fn provider_session(
    protocol: WorkProviderProtocol,
    captured: Option<&(Vec<u8>, u64)>,
) -> Option<ObservationSourceIdentityV1> {
    let retained = captured?.0.as_slice();
    retained
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .find_map(|event| session_from_event(protocol, &event))
}

fn session_from_event(
    protocol: WorkProviderProtocol,
    event: &serde_json::Value,
) -> Option<ObservationSourceIdentityV1> {
    let (provider, session_id) = match protocol {
        WorkProviderProtocol::ClaudeStreamJson
            if event.get("type")?.as_str()? == "system"
                && event.get("subtype")?.as_str()? == "init" =>
        {
            ("claude", event.get("session_id")?.as_str()?)
        }
        WorkProviderProtocol::CodexExecJson if event.get("type")?.as_str()? == "thread.started" => {
            ("codex", event.get("thread_id")?.as_str()?)
        }
        WorkProviderProtocol::ClaudeStreamJson
        | WorkProviderProtocol::CodexExecJson
        | WorkProviderProtocol::CodexAppServerJsonRpc => return None,
    };
    ObservationSourceIdentityV1::for_provider(
        ProviderId::new(provider.to_owned()).ok()?,
        SessionId::new(session_id.to_owned()).ok()?,
    )
    .ok()
}

pub(super) fn stream_summary(
    captured: Option<(Vec<u8>, u64)>,
) -> Option<WorkAttemptStreamSummaryV1> {
    let (retained, total) = captured?;
    let digest =
        ManifestDigest::new(format!("sha256:{}", hex::encode(Sha256::digest(&retained)))).ok()?;
    Some(WorkAttemptStreamSummaryV1 {
        byte_length: total,
        truncated: total > retained.len() as u64,
        digest,
    })
}

/// A truncated stream means the provider exceeded its admitted output
/// budget; that is a typed overflow outcome, not a silent trim, unless the
/// attempt already ended in cancellation or timeout.
pub(super) fn overflow_outcome(
    outcome: WorkAttemptProviderOutcomeV1,
    stdout: &Option<WorkAttemptStreamSummaryV1>,
    stderr: &Option<WorkAttemptStreamSummaryV1>,
) -> WorkAttemptProviderOutcomeV1 {
    if matches!(
        outcome,
        WorkAttemptProviderOutcomeV1::Cancelled | WorkAttemptProviderOutcomeV1::TimedOut
    ) {
        return outcome;
    }
    if stdout.as_ref().is_some_and(|summary| summary.truncated) {
        return WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stdout,
        };
    }
    if stderr.as_ref().is_some_and(|summary| summary.truncated) {
        return WorkAttemptProviderOutcomeV1::StreamOverflow {
            channel: WorkAttemptStreamChannelV1::Stderr,
        };
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_correlation_skips_malformed_frames_but_requires_the_protocol_start_event() {
        let claude = captured(
            b"not-json\n{\"type\":\"assistant\",\"session_id\":\"forged\"}\n{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"claude-session\"}\n",
        );
        let source = provider_session(WorkProviderProtocol::ClaudeStreamJson, Some(&claude))
            .expect("Claude init session");
        assert_eq!(source.provider().as_str(), "claude");
        assert_eq!(source.session_id().as_str(), "claude-session");

        assert!(
            provider_session(WorkProviderProtocol::CodexExecJson, Some(&claude)).is_none(),
            "a Claude init frame cannot mint a Codex session",
        );
    }

    #[test]
    fn malformed_or_content_only_output_cannot_mint_a_provider_session() {
        for bytes in [
            b"not-json\n".as_slice(),
            b"{\"type\":\"assistant\",\"session_id\":\"forged\"}\n".as_slice(),
            b"{\"type\":\"thread.started\",\"thread_id\":\"\"}\n".as_slice(),
        ] {
            let captured = captured(bytes);
            assert!(
                provider_session(WorkProviderProtocol::CodexExecJson, Some(&captured)).is_none(),
                "content or an invalid identity must not become Work authority",
            );
        }
    }

    fn captured(bytes: &[u8]) -> (Vec<u8>, u64) {
        (bytes.to_vec(), bytes.len() as u64)
    }
}
