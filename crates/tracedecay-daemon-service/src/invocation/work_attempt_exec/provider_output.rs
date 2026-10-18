//! Bounded provider output accounting and provider-owned session correlation.
//!
//! Output content remains opaque except for the one protocol-defined session
//! start frame needed to bind a durable Work attempt to its owning transcript.

use std::io::{ErrorKind, Read};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};
use tracedecay_contracts::{
    WorkAttemptProviderOutcomeV1, WorkAttemptStreamChannelV1, WorkAttemptStreamSummaryV1,
};
use tracedecay_domain::{
    ManifestDigest, ObservationSourceIdentityV1, ProviderId, SessionId, WorkProviderProtocol,
};

/// How often a reader parked on a quiet pipe re-checks its stop flag.
const STOP_POLL_INTERVAL_MS: i32 = 50;

/// A captured pipe end the reader can wait on without blocking in `read`.
#[cfg(unix)]
pub(super) trait CapturedPipe: Read + std::os::fd::AsFd {}
#[cfg(unix)]
impl<T: Read + std::os::fd::AsFd> CapturedPipe for T {}
#[cfg(not(unix))]
pub(super) trait CapturedPipe: Read {}
#[cfg(not(unix))]
impl<T: Read> CapturedPipe for T {}

/// Whether the pipe has data, EOF, or an error to read. A pipe held open by a
/// descendant stays quiet until `STOP_POLL_INTERVAL_MS` elapses.
#[cfg(unix)]
fn readable(stream: &impl std::os::fd::AsFd) -> bool {
    use std::os::fd::AsRawFd;

    let mut poll = libc::pollfd {
        fd: stream.as_fd().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll` is one valid `pollfd` and the count passed is 1.
    let ready = unsafe { libc::poll(&raw mut poll, 1, STOP_POLL_INTERVAL_MS) };
    ready > 0 || (ready < 0 && std::io::Error::last_os_error().kind() != ErrorKind::Interrupted)
}

#[cfg(not(unix))]
fn readable(_stream: &impl Read) -> bool {
    true
}

/// Reads `stream` to EOF, retaining at most `cap` bytes. Once `stop` is set the
/// reader returns what it has so far, so a pipe a descendant never closes
/// cannot pin the thread or its file descriptor.
pub(super) fn read_capped(
    stream: Option<impl CapturedPipe>,
    cap: u64,
    stop: &AtomicBool,
) -> Option<(Vec<u8>, u64)> {
    let mut stream = stream?;
    let mut retained: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut buffer = [0_u8; 16 * 1024];
    while !stop.load(Ordering::Acquire) {
        if !readable(&stream) {
            continue;
        }
        match stream.read(&mut buffer) {
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
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
    let digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&retained)).ok()?;
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
        let started = captured(b"{\"type\":\"thread.started\",\"thread_id\":\"codex-thread\"}\n");
        let source = provider_session(WorkProviderProtocol::CodexExecJson, Some(&started))
            .expect("Codex thread.started session");
        assert_eq!(source.provider().as_str(), "codex");
        assert_eq!(source.session_id().as_str(), "codex-thread");
    }

    fn captured(bytes: &[u8]) -> (Vec<u8>, u64) {
        (bytes.to_vec(), bytes.len() as u64)
    }
}
