use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationFactV1, ObservationId, ObservationScopeV1,
};

use super::{
    MAX_DISCOVERY_CANDIDATES, MAX_DISCOVERY_FAILURE_EVIDENCE, MAX_DISCOVERY_INPUT_BYTES,
    MAX_DISCOVERY_UNITS, PI_AGENT_RELATIVE, PiDiscoveryReport, PiSource, capture_pi_observations,
    capture_pi_session, pi_agent_dir_for, session_dir_name, warn_discovery_failures,
};
use crate::admission::{HostAdmission, test_support::MemoryHostAdmission};
use crate::observation::ObservationCancellation;
use crate::runtime::host_scan::{HOST_SCAN_WINDOW, HostScanBudget};
use crate::runtime::source::{FileDiscoveryReport, TranscriptDiscoveryBounds};

const FIXTURE_NAME: &str = "2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl";
const FIXTURE: &str = include_str!(
    "../../../../../tests/fixtures/transcript_golden/pi/2026-09-25T16-00-00-000Z_5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90.jsonl"
);
const SESSION_ID: &str = "5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90";

struct Layout {
    _temp: tempfile::TempDir,
    project: PathBuf,
    agent_dir: PathBuf,
}

impl Layout {
    fn new() -> Self {
        crate::runtime::observation::jsonl_observation_admission::install_test_shared_jsonl_preparation_authority();
        let temp = tempfile::TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let agent_dir = temp.path().join(PI_AGENT_RELATIVE);
        Self {
            _temp: temp,
            project,
            agent_dir,
        }
    }

    fn session_dir(&self) -> PathBuf {
        let dir = self
            .agent_dir
            .join("sessions")
            .join(session_dir_name(&self.project));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_fixture(&self) -> PathBuf {
        let path = self.session_dir().join(FIXTURE_NAME);
        let project = serde_json::to_string(&self.project).unwrap();
        std::fs::write(&path, FIXTURE.replace("\"<PROJECT_ROOT>\"", &project)).unwrap();
        path
    }

    fn write_malformed_files(&self, count: usize) {
        for index in 0..count {
            std::fs::write(
                self.session_dir()
                    .join(format!("2026-09-25T16-00-00-000Z_invalid-{index}.jsonl")),
                "{\"not\":\"a Pi session header\"}\n",
            )
            .unwrap();
        }
    }

    fn source(&self) -> PiSource {
        PiSource::with_agent_dir(&self.agent_dir)
    }
}

async fn capture(layout: &Layout, admission: &MemoryHostAdmission) -> super::PiCaptureOutcome {
    capture_pi_observations(
        admission,
        &layout.source(),
        &layout.project,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap()
}

fn envelopes(admission: &MemoryHostAdmission) -> Vec<CanonicalObservationEnvelopeV1> {
    admission
        .observations()
        .iter()
        .map(|stored| serde_json::from_value(stored.observation().payload().clone()).unwrap())
        .collect()
}

fn record<'a>(
    envelopes: &'a [CanonicalObservationEnvelopeV1],
    entry_id: &str,
) -> &'a CanonicalObservationEnvelopeV1 {
    let stable = format!("{SESSION_ID}:{entry_id}");
    envelopes
        .iter()
        .find(|envelope| envelope.stable_record_id().as_str() == stable)
        .unwrap_or_else(|| panic!("no record {stable}"))
}

#[tokio::test]
async fn discovery_summary_reports_changes_and_recovery_without_repeating_unchanged_failures() {
    let first = Layout::new();
    let second = Layout::new();
    first.write_malformed_files(24);
    second.write_malformed_files(25);
    async fn report(layout: &Layout) -> PiDiscoveryReport {
        let (mut discovery, mut budget) = layout
            .source()
            .discover(
                &layout.project,
                TranscriptDiscoveryBounds::from_discovered_units(MAX_DISCOVERY_CANDIDATES),
                None,
                HostScanBudget::new(
                    MAX_DISCOVERY_INPUT_BYTES,
                    MAX_DISCOVERY_UNITS,
                    std::time::Instant::now() + HOST_SCAN_WINDOW,
                    ObservationCancellation::default(),
                ),
                None,
            )
            .unwrap();
        // Each report has fresh admission state so this test exercises the
        // log gate independently from durable path convergence.
        super::reconcile_invalid_headers(
            &MemoryHostAdmission::default(),
            &ObservationScopeV1::Profile,
            &mut discovery,
            &mut budget,
        )
        .await
        .unwrap();
        discovery
    }
    let first_report = report(&first).await;
    assert_eq!(first_report.failure_count, 24);
    assert_eq!(first_report.failures.len(), MAX_DISCOVERY_FAILURE_EVIDENCE);
    let summary_count = |layout: &Layout, discovery: &PiDiscoveryReport| {
        let (_, output) = tracedecay_runtime_core::logging::capture_formatted_tracing(|| {
            warn_discovery_failures(&layout.source().sessions_root(), discovery);
        });
        output
            .matches("additional Pi discovery failures were bounded")
            .count()
    };
    assert_eq!(summary_count(&first, &first_report), 1);
    assert_eq!(
        summary_count(&first, &first_report),
        0,
        "an unchanged refusal summary is not repeated"
    );
    let interrupted = PiDiscoveryReport {
        files: FileDiscoveryReport {
            paths: Vec::new(),
            truncated: None,
            skipped_oversized_entries: 0,
            bytes_charged: 0,
            files_considered: 0,
        },
        failures: Vec::new(),
        failure_count: 0,
        invalid_headers: Vec::new(),
        quarantined_headers: 0,
        scan_complete: false,
        reached_end: false,
    };
    assert_eq!(summary_count(&first, &interrupted), 0);
    assert_eq!(
        summary_count(&first, &first_report),
        0,
        "an interrupted empty scan must not clear the admitted summary"
    );
    first.write_malformed_files(25);
    let changed = report(&first).await;
    assert_eq!(changed.failure_count, 25);
    assert_eq!(
        summary_count(&first, &changed),
        1,
        "changed totals are logged"
    );
    assert_eq!(
        summary_count(&second, &report(&second).await),
        1,
        "another sessions root is logged independently"
    );
    for index in MAX_DISCOVERY_FAILURE_EVIDENCE..25 {
        std::fs::remove_file(
            first
                .session_dir()
                .join(format!("2026-09-25T16-00-00-000Z_invalid-{index}.jsonl")),
        )
        .unwrap();
    }
    let fully_reported = report(&first).await;
    assert_eq!(
        fully_reported.failure_count,
        MAX_DISCOVERY_FAILURE_EVIDENCE as u64
    );
    assert_eq!(
        fully_reported.failures.len(),
        MAX_DISCOVERY_FAILURE_EVIDENCE
    );
    assert_eq!(summary_count(&first, &fully_reported), 0);
    first.write_malformed_files(25);
    assert_eq!(
        summary_count(&first, &report(&first).await),
        1,
        "full failure evidence clears the previous summary"
    );
    for index in 0..25 {
        std::fs::remove_file(
            first
                .session_dir()
                .join(format!("2026-09-25T16-00-00-000Z_invalid-{index}.jsonl")),
        )
        .unwrap();
    }
    first.write_fixture();
    let recovered = report(&first).await;
    assert_eq!(recovered.failure_count, 0);
    assert_eq!(summary_count(&first, &recovered), 0);
    first.write_malformed_files(25);
    let recurred = report(&first).await;
    assert_eq!(
        summary_count(&first, &recurred),
        1,
        "recurrence after recovery is logged"
    );
    assert_eq!(summary_count(&first, &recurred), 0);
}

#[tokio::test]
async fn repeated_discovery_summary_keeps_malformed_sources_and_coverage_refused() {
    let layout = Layout::new();
    let malformed = MAX_DISCOVERY_FAILURE_EVIDENCE + 1;
    layout.write_malformed_files(malformed);
    let admission = MemoryHostAdmission::default();
    let first_outcome = capture(&layout, &admission).await;
    assert_eq!(first_outcome.discovery_failures, malformed as u64);
    assert!(first_outcome.deferred);
    assert!(admission.observations().is_empty());
    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://pi/v1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        coverage.file_id, 2,
        "malformed sources retain partial coverage"
    );
    // A later pass quarantines the unchanged malformed files: they are not
    // re-opened, not re-counted, and the refused coverage is retained.
    let second_outcome = capture(&layout, &admission).await;
    assert_eq!(
        second_outcome.discovery_failures, 0,
        "unchanged malformed files are not re-failed"
    );
    assert!(
        second_outcome.deferred,
        "quarantined sources retain partial coverage"
    );
    assert!(admission.observations().is_empty());
    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://pi/v1")
        .await
        .unwrap();
    assert_eq!(
        coverage.unwrap().file_id,
        2,
        "quarantined malformed sources must not become complete coverage"
    );
}

#[tokio::test]
async fn empty_host_is_observed_once_without_revising_unchanged_frontier() {
    let layout = Layout::new();
    let admission = MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Profile;
    let key = super::PI_DISCOVERY_FRONTIER_KEY;
    assert!(
        admission
            .get_parse_offset(&scope, key)
            .await
            .unwrap()
            .is_none()
    );

    capture(&layout, &admission).await;
    let first = admission
        .get_parse_offset(&scope, key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.byte_offset, first.file_id, first.mtime), (0, 0, 1));
    let coverage = admission
        .get_parse_offset(&scope, "host-coverage://pi/v1")
        .await
        .unwrap()
        .unwrap();

    capture(&layout, &admission).await;
    assert_eq!(
        admission.get_parse_offset(&scope, key).await.unwrap(),
        Some(first)
    );
    assert_eq!(
        admission
            .get_parse_offset(&scope, "host-coverage://pi/v1")
            .await
            .unwrap(),
        Some(coverage)
    );
    assert!(admission.observations().is_empty());
}

#[tokio::test]
async fn fixture_session_lands_with_stable_entry_identities_and_messages() {
    let layout = Layout::new();
    layout.write_fixture();
    let admission = MemoryHostAdmission::default();

    let outcome = capture(&layout, &admission).await;

    assert!(!outcome.deferred, "{outcome:?}");
    assert_eq!(outcome.discovery_failures, 0);
    let stored = admission.observations();
    assert_eq!(
        stored.len(),
        9,
        "every fixture line, including the empty user turn, is one observation"
    );
    for observation in &stored {
        assert_eq!(
            observation.observation().source().session_id().as_str(),
            SESSION_ID
        );
    }
    let envelopes = envelopes(&admission);

    let header = record(&envelopes, "session");
    assert!(header.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Session { project_path: Some(path), started_at: Some(1_790_352_000), .. }
            if Path::new(path) == layout.project
    )));

    let user = record(&envelopes, "c2d3e4f5");
    assert_eq!(
        user.relations().parent_message_id().map(|id| id.as_str()),
        Some(format!("{SESSION_ID}:b1c2d3e4").as_str())
    );
    assert!(user.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Message { role: CanonicalMessageRoleV1::User, content, timestamp: Some(1_790_352_001), .. }
            if content == &json!("Fix the flaky billing retry test")
    )));

    let assistant = record(&envelopes, "d3e4f5a6");
    let facts = assistant.facts();
    assert!(facts.iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Message { role: CanonicalMessageRoleV1::Assistant, content, model: Some(model), .. }
            if model == "claude-sonnet-4-5"
                && content == &json!([{"type": "text", "text": "I will pin the retry clock in the billing test."}])
    )));
    assert!(facts.iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Reasoning { content: Some(content), .. }
            if content == "The retry loop reads the clock twice."
    )));
    assert!(facts.iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::ToolInvocation { invocation_id, name, .. }
            if invocation_id.as_ref().map(ObservationId::as_str) == Some("toolu_01PiEdit")
                && name == "edit"
    )));
    assert!(facts.iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Git {
            evidence_kind: CanonicalGitEvidenceKindV1::FileEdit,
            reference: Some(path),
            ..
        } if path == "src/billing/retry.rs"
    )));

    let result = record(&envelopes, "e4f5a6b7");
    assert!(result.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::ToolResult { invocation_id: Some(id), success: Some(true), .. }
            if id.as_str() == "toolu_01PiEdit"
    )));
    assert!(result.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::Tool,
            timestamp: Some(1_790_352_003),
            ..
        }
    )));

    let empty_user = record(&envelopes, "b7c8d9e0");
    assert!(empty_user.facts().iter().any(|fact| matches!(
        fact,
        CanonicalObservationFactV1::Message {
            role: CanonicalMessageRoleV1::User,
            content,
            timestamp: Some(1_790_352_005),
            ..
        } if content == &json!("")
    )));

    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://pi/v1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(coverage.file_id, 1);

    // An unchanged session resumes at its cursor and admits nothing twice.
    capture(&layout, &admission).await;
    assert_eq!(admission.observations().len(), 9);
}

#[tokio::test]
async fn malformed_session_file_is_refused_with_a_typed_failure() {
    let layout = Layout::new();
    let session = std::fs::read(layout.write_fixture()).unwrap();
    // The header embeds the project path, so each line's byte span depends on
    // the temp directory's length.
    let [b0, b1, b2, b3, b4, b5, b6, b7, b8, b9]: [usize; 10] = std::iter::once(0)
        .chain(
            session
                .split_inclusive(|byte| *byte == b'\n')
                .scan(0, |end, line| {
                    *end += line.len();
                    Some(*end)
                }),
        )
        .collect::<Vec<_>>()
        .try_into()
        .expect("the Pi fixture has nine lines");
    let malformed = layout
        .session_dir()
        .join("2026-09-25T17-00-00-000Z_0d7f6c1b-2e4a-4b8d-8c3f-1a2b3c4d5e6f.jsonl");
    std::fs::write(
        &malformed,
        "{\"type\":\"message\",\"id\":\"deadbeef\",\"message\":{\"role\":\"user\",\"content\":\"orphan entry without a header\"}}\n",
    )
    .unwrap();
    let admission = MemoryHostAdmission::default();

    let outcome = capture(&layout, &admission).await;

    assert_eq!(outcome.discovery_failures, 1);
    assert!(outcome.deferred, "a refused file leaves coverage partial");
    let payloads = admission
        .observations()
        .iter()
        .map(|stored| stored.observation().payload().to_string())
        .collect::<String>();
    let project = layout.project.to_string_lossy().into_owned();
    // The payloads are serialized JSON: match the path's escaped spelling,
    // which doubles the `\\` separators on Windows.
    let escaped = serde_json::to_string(&project).unwrap();
    let normalized = payloads.replace(&escaped[1..escaped.len() - 1], "/project");
    let temp_root = layout._temp.path().to_string_lossy().into_owned();
    let escaped_temp = serde_json::to_string(&temp_root).unwrap();
    let normalized = normalized.replace(&escaped_temp[1..escaped_temp.len() - 1], "/tmp-root");
    let expected_transcript = serde_json::to_string(&layout.session_dir().join(FIXTURE_NAME))
        .unwrap()
        .replace(&escaped[1..escaped.len() - 1], "/project")
        .replace(&escaped_temp[1..escaped_temp.len() - 1], "/tmp-root");

    // The contentless user message (b7..b8) is a user turn. session_info
    // follows it.
    assert_eq!(
        normalized,
        format!(
            r#"{{"evidence":{{"native_timestamp":1790352000,"ordering_domain":"file_bytes","range":{{"end":{b1},"start":{b0}}}}},"facts":[{{"kind":"session","native_source":"pi","project_path":"/project","source":"pi_session","started_at":1790352000,"transcript_path":{expected_transcript}}}],"native_record_kind":"session","provider":"pi","relations":{{"session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:session","version":1}}{{"evidence":{{"native_timestamp":1790352000,"ordering_domain":"file_bytes","range":{{"end":{b2},"start":{b1}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"kind":"unknown","native_kind":"message.system","state":"unsupported"}}],"native_record_kind":"message.system","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a0b1c2d3","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a0b1c2d3","version":1}}{{"evidence":{{"native_timestamp":1790352000,"ordering_domain":"file_bytes","range":{{"end":{b3},"start":{b2}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"kind":"unknown","native_kind":"model_change","state":"unsupported"}}],"native_record_kind":"model_change","provider":"pi","relations":{{"parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a0b1c2d3","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:b1c2d3e4","version":1}}{{"evidence":{{"native_timestamp":1790352001,"ordering_domain":"file_bytes","range":{{"end":{b4},"start":{b3}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":"Fix the flaky billing retry test","kind":"message","role":"user","timestamp":1790352001}}],"native_record_kind":"message.user","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:c2d3e4f5","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:b1c2d3e4","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:c2d3e4f5","version":1}}{{"evidence":{{"native_timestamp":1790352002,"ordering_domain":"file_bytes","range":{{"end":{b5},"start":{b4}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":"The retry loop reads the clock twice.","kind":"reasoning","visibility":"visible"}},{{"content":{{"change_type":"edit","edited_at_micros":1790352002000000,"id":"toolu_01PiEdit","type":"toolCall"}},"evidence_kind":"file_edit","kind":"git","reference":"src/billing/retry.rs"}},{{"arguments":{{"newText":"<REDACTED_NEW_TEXT>","oldText":"<REDACTED_OLD_TEXT>","path":"src/billing/retry.rs"}},"invocation_id":"toolu_01PiEdit","kind":"tool_invocation","name":"edit"}},{{"content":[{{"text":"I will pin the retry clock in the billing test.","type":"text"}}],"kind":"message","model":"claude-sonnet-4-5","role":"assistant","timestamp":1790352002}}],"native_record_kind":"message.assistant","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:d3e4f5a6","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:c2d3e4f5","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:d3e4f5a6","version":1}}{{"evidence":{{"native_timestamp":1790352003,"ordering_domain":"file_bytes","range":{{"end":{b6},"start":{b5}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":[{{"text":"<REDACTED_TOOL_OUTPUT>","type":"text"}}],"invocation_id":"toolu_01PiEdit","kind":"tool_result","success":true,"token_count":8}},{{"content":[{{"text":"<REDACTED_TOOL_OUTPUT>","type":"text"}}],"kind":"message","role":"tool","timestamp":1790352003}}],"native_record_kind":"message.toolResult","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:e4f5a6b7","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:d3e4f5a6","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:e4f5a6b7","version":1}}{{"evidence":{{"native_timestamp":1790352004,"ordering_domain":"file_bytes","range":{{"end":{b7},"start":{b6}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":[{{"text":"The billing retry test now pins its clock.","type":"text"}}],"kind":"message","model":"claude-sonnet-4-5","role":"assistant","timestamp":1790352004}}],"native_record_kind":"message.assistant","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:e4f5a6b7","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","version":1}}{{"evidence":{{"native_timestamp":1790352005,"ordering_domain":"file_bytes","range":{{"end":{b8},"start":{b7}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":"","kind":"message","role":"user","timestamp":1790352005}}],"native_record_kind":"message.user","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:b7c8d9e0","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:b7c8d9e0","version":1}}{{"evidence":{{"native_timestamp":1790352005,"ordering_domain":"file_bytes","range":{{"end":{b9},"start":{b8}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","title":"Billing retry clock","transcript_path":{expected_transcript}}}],"native_record_kind":"session_info","provider":"pi","relations":{{"parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a6b7c8d9","version":1}}"#
        )
    );
    assert!(!payloads.contains("orphan entry without a header"));
    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://pi/v1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(coverage.file_id, 2);
}

#[tokio::test]
async fn header_naming_another_session_than_its_file_is_refused() {
    let layout = Layout::new();
    let path = layout.write_fixture();
    let renamed = path.with_file_name("2026-09-25T16-00-00-000Z_other-session.jsonl");
    std::fs::rename(&path, &renamed).unwrap();
    let admission = MemoryHostAdmission::default();

    let outcome = capture(&layout, &admission).await;

    assert_eq!(outcome.discovery_failures, 1);
    assert!(admission.observations().is_empty());
}

#[tokio::test]
async fn named_session_capture_lands_only_that_session_in_scope() {
    let layout = Layout::new();
    layout.write_fixture();
    let sibling = layout
        .session_dir()
        .join("2026-09-25T18-00-00-000Z_sibling-session.jsonl");
    std::fs::write(
        &sibling,
        format!(
            "{}\n{}\n",
            json!({"type": "session", "version": 3, "id": "sibling-session", "timestamp": "2026-09-25T18:00:00.000Z", "cwd": layout.project}),
            json!({"type": "message", "id": "11112222", "parentId": null, "timestamp": "2026-09-25T18:00:01.000Z", "message": {"role": "user", "content": "sibling text", "timestamp": 1_790_359_201_000_u64}}),
        ),
    )
    .unwrap();
    let admission = MemoryHostAdmission::default();

    let outcome = capture_pi_session(
        &admission,
        &layout.source(),
        &layout.project,
        &layout.project,
        SESSION_ID,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();

    assert!(!outcome.deferred);
    let stored = admission.observations();
    assert_eq!(stored.len(), 9);
    assert!(
        stored
            .iter()
            .all(|stored| stored.observation().source().session_id().as_str() == SESSION_ID)
    );

    let other_project = layout.project.join("elsewhere");
    let out_of_scope = MemoryHostAdmission::default();
    capture_pi_session(
        &out_of_scope,
        &layout.source(),
        &other_project,
        &layout.project,
        SESSION_ID,
        ObservationScopeV1::Profile,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert!(
        out_of_scope.observations().is_empty(),
        "a session whose header cwd is outside the project never lands there"
    );
}

#[test]
fn session_directory_name_matches_pi_encoding() {
    assert_eq!(
        session_dir_name(Path::new("/home/dev/work/app")),
        "--home-dev-work-app--"
    );
    assert_eq!(
        session_dir_name(Path::new(r"C:\Users\dev\app")),
        "--C--Users-dev-app--"
    );
}

#[test]
fn ambient_agent_dir_is_honored_only_when_absolute_and_inside_home() {
    #[cfg(unix)]
    let (home, outside) = (
        Path::new("/home/operator"),
        Path::new("/elsewhere/pi-agent"),
    );
    #[cfg(not(unix))]
    let (home, outside) = (
        Path::new(r"C:\home\operator"),
        Path::new(r"D:\elsewhere\pi-agent"),
    );
    assert_eq!(pi_agent_dir_for(home, None), home.join(".pi/agent"));
    let inside = home.join("relocated/pi-agent");
    assert_eq!(pi_agent_dir_for(home, Some(inside.as_os_str())), inside);
    assert_eq!(
        pi_agent_dir_for(home, Some(outside.as_os_str())),
        home.join(".pi/agent")
    );
    for relative in ["relocated/agent", "./agent", "~/.pi/other", ""] {
        assert_eq!(
            pi_agent_dir_for(home, Some(OsStr::new(relative))),
            home.join(".pi/agent"),
            "relative override {relative:?} must not relocate the agent directory"
        );
    }
}

#[test]
fn a_foreign_home_never_resolves_outside_itself() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        super::pi_agent_dir(home.path()),
        home.path().join(PI_AGENT_RELATIVE)
    );
}

#[test]
fn system_prompt_replay_is_not_projected_as_conversation() {
    let system: Value = serde_json::from_str(FIXTURE.lines().nth(1).unwrap()).unwrap();
    let range = tracedecay_domain::ObservationSourceRangeV1::new(0, 10).unwrap();
    let envelope =
        tracedecay_capture::pi::normalize_observation(&system, SESSION_ID, None, range).unwrap();
    assert!(
        envelope
            .facts()
            .iter()
            .all(|fact| matches!(fact, CanonicalObservationFactV1::Unknown { .. }))
    );
}

/// A fresh provider run uses the durable completion decision or the canonical
/// JSONL coverage cursor without admitting unchanged records again.
#[tokio::test]
async fn converged_session_file_is_not_re_admitted_by_a_fresh_sweep() {
    let layout = Layout::new();
    let path = layout.write_fixture();
    let settled = crate::runtime::source::spin_until_jsonl_change_settled(&path);
    let admission = MemoryHostAdmission::default();
    let first = capture(&layout, &admission).await;
    assert!(!first.deferred);
    if settled {
        let witness = super::observe_file_witness(&path).await.unwrap();
        assert_eq!(
            super::PathMemo::read(&admission, &ObservationScopeV1::Profile, &path)
                .await
                .unwrap()
                .disposition(witness),
            Some(super::Disposition::Complete),
        );
    }
    let first_counts = admission.capture_call_counts();
    let second = capture(&layout, &admission).await;
    assert!(!second.deferred);
    assert_eq!(
        admission.capture_call_counts(),
        first_counts,
        "an unchanged, fully admitted file must not be re-admitted"
    );
}

/// Discovery checks each header's current content, but an unchanged durable
/// refusal is not reported again by a fresh provider run.
#[tokio::test]
async fn malformed_session_header_is_not_re_failed_by_a_fresh_sweep() {
    let layout = Layout::new();
    layout.write_fixture();
    layout.write_malformed_files(1);
    let admission = MemoryHostAdmission::default();
    let first = capture(&layout, &admission).await;
    assert!(
        first.discovery_failures >= 1,
        "the malformed file must fail once"
    );
    let second = capture(&layout, &admission).await;
    assert_eq!(
        second.discovery_failures, 0,
        "an unchanged malformed file must be quarantined after its first failure"
    );
    assert!(
        second.deferred,
        "quarantine does not authorize complete coverage"
    );
    let path = layout
        .session_dir()
        .join("2026-09-25T16-00-00-000Z_invalid-0.jsonl");
    let super::HeaderRead::Invalid { witness, .. } = super::read_session_header(&path).unwrap()
    else {
        panic!("fixture must retain its invalid header");
    };
    assert_eq!(
        super::PathMemo::read(&admission, &ObservationScopeV1::Profile, &path)
            .await
            .unwrap()
            .disposition(Some(witness)),
        Some(super::Disposition::InvalidHeader),
        "the refusal lives in the durable admission authority",
    );
}

#[tokio::test]
async fn same_length_rewrite_with_restored_mtime_starts_a_new_generation() {
    let layout = Layout::new();
    let path = layout.write_fixture();
    crate::runtime::source::spin_until_jsonl_change_settled(&path);
    let admission = MemoryHostAdmission::default();
    assert!(!capture(&layout, &admission).await.deferred);
    let original = std::fs::metadata(&path).unwrap();
    let original_mtime = filetime::FileTime::from_last_modification_time(&original);
    let original_generation = admission.observations()[0]
        .observation()
        .identity()
        .generation();
    let before = std::fs::read_to_string(&path).unwrap();
    let edited = before
        .replace(
            "Fix the flaky billing retry test",
            "Fix the flaky billing retry case",
        )
        .replace("\"id\":\"c2d3e4f5\"", "\"id\":\"c2d3e4f6\"");
    assert_ne!(before, edited);
    assert_eq!(before.len(), edited.len());
    std::fs::write(&path, edited).unwrap();
    filetime::set_file_mtime(&path, original_mtime).unwrap();
    crate::runtime::source::spin_until_jsonl_change_settled(&path);
    let rewritten = std::fs::metadata(&path).unwrap();
    assert_eq!(rewritten.len(), original.len());
    assert_eq!(
        filetime::FileTime::from_last_modification_time(&rewritten),
        original_mtime,
    );
    #[cfg(unix)]
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&rewritten),
        std::os::unix::fs::MetadataExt::ino(&original),
    );

    assert!(!capture(&layout, &admission).await.deferred);
    let observations = admission.observations();
    let changed = observations
        .iter()
        .find(|stored| {
            stored
                .observation()
                .payload()
                .to_string()
                .contains("Fix the flaky billing retry case")
        })
        .expect("the canonical prefix witness must discover the rewritten record");
    assert_ne!(
        changed.observation().identity().generation(),
        original_generation,
    );
    assert_eq!(observations.len(), 10, "only the changed record is new");
    let calls = admission.capture_call_counts();
    assert!(!capture(&layout, &admission).await.deferred);
    assert_eq!(admission.capture_call_counts(), calls);
}

#[tokio::test]
async fn quarantine_replacement_accepts_older_mtime_and_preserves_a_concurrent_winner() {
    let layout = Layout::new();
    layout.write_malformed_files(1);
    let path = layout
        .session_dir()
        .join("2026-09-25T16-00-00-000Z_invalid-0.jsonl");
    let admission = MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Profile;
    assert_eq!(capture(&layout, &admission).await.discovery_failures, 1);
    let stale = super::PathMemo::read(&admission, &scope, &path)
        .await
        .unwrap();
    let super::HeaderRead::Invalid { witness: old, .. } =
        super::read_session_header(&path).unwrap()
    else {
        panic!("fixture must have an invalid header");
    };
    let original_mtime =
        filetime::FileTime::from_last_modification_time(&std::fs::metadata(&path).unwrap());
    std::fs::write(&path, "{\"not\":\"a changed invalid header\"}\n").unwrap();
    let earlier = filetime::FileTime::from_unix_time(original_mtime.unix_seconds() - 60, 0);
    filetime::set_file_mtime(&path, earlier).unwrap();
    let super::HeaderRead::Invalid { witness: new, .. } =
        super::read_session_header(&path).unwrap()
    else {
        panic!("replacement must have an invalid header");
    };
    assert_ne!(old, new);
    let changed = capture(&layout, &admission).await;
    assert_eq!(changed.discovery_failures, 1);
    assert!(changed.deferred);
    assert!(
        !stale
            .record(&admission, &scope, old, super::Disposition::Complete)
            .await
            .unwrap(),
        "an older in-flight decision cannot overwrite the newer quarantine",
    );
    let current = super::PathMemo::read(&admission, &scope, &path)
        .await
        .unwrap();
    assert_eq!(current.disposition(Some(old)), None);
    assert_eq!(
        current.disposition(Some(new)),
        Some(super::Disposition::InvalidHeader)
    );
    let retry = capture(&layout, &admission).await;
    assert_eq!(
        retry.discovery_failures, 0,
        "the new refusal converges durably"
    );
    assert!(retry.deferred);
}

#[tokio::test]
async fn queued_session_rewritten_into_another_project_is_rechecked_before_admission() {
    let layout = Layout::new();
    let path = layout.write_fixture();
    let admission = MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Profile;
    let queued = capture_pi_observations(
        &admission,
        &layout.source(),
        &layout.project,
        scope.clone(),
        Some(0),
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert!(queued.deferred);
    assert!(admission.observations().is_empty());
    assert_eq!(
        admission
            .discovery_paths_after(&scope, super::PROVIDER, 0, 10)
            .await
            .unwrap()[0]
            .path,
        path,
    );
    let foreign = layout._temp.path().join("foreign-project");
    std::fs::create_dir_all(&foreign).unwrap();
    let foreign_json = serde_json::to_string(&foreign).unwrap();
    std::fs::write(&path, FIXTURE.replace("\"<PROJECT_ROOT>\"", &foreign_json)).unwrap();

    capture(&layout, &admission).await;
    assert!(
        admission.observations().is_empty(),
        "the durable queue must not carry the previous header's scope authority",
    );
    capture_pi_observations(
        &admission,
        &layout.source(),
        &foreign,
        scope,
        None,
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    assert_eq!(admission.observations().len(), 9);
}

#[tokio::test]
async fn admission_rechecks_the_scanned_header_before_fresh_or_resumed_capture() {
    for resumed in [false, true] {
        let layout = Layout::new();
        let path = layout.session_dir().join(FIXTURE_NAME);
        let foreign = layout._temp.path().join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        let header = |cwd: &Path| {
            format!(
                "{{\"type\":\"session\",\"version\":3,\"id\":\"{SESSION_ID}\",\"padding\":\"{}\",\"cwd\":{},\"timestamp\":\"2026-09-25T16:00:00.000Z\"}}\n",
                "x".repeat(2048),
                serde_json::to_string(cwd).unwrap(),
            )
        };
        let accepted_header = header(&layout.project);
        let foreign_header = header(&foreign);
        assert_ne!(accepted_header, foreign_header);
        assert_eq!(
            &accepted_header.as_bytes()[..1024],
            &foreign_header.as_bytes()[..1024],
            "the changed cwd is beyond the native identity window",
        );
        let (_, body) = FIXTURE.split_once('\n').unwrap();
        let foreign_transcript = format!("{foreign_header}{body}");
        let admission = MemoryHostAdmission::default();
        let scope = ObservationScopeV1::Profile;
        if resumed {
            std::fs::write(&path, &foreign_transcript).unwrap();
            capture_pi_observations(
                &admission,
                &layout.source(),
                &foreign,
                scope.clone(),
                None,
                &ObservationCancellation::default(),
            )
            .await
            .unwrap();
            assert_eq!(admission.observations().len(), 9);
        }
        std::fs::write(&path, format!("{accepted_header}{body}")).unwrap();
        let super::HeaderRead::Header { header, witness } =
            super::read_session_header(&path).unwrap()
        else {
            panic!("the accepted header must be valid");
        };
        assert!(
            layout
                .source()
                .matcher(&layout.project)
                .accepts(Some(Path::new(&header.cwd)))
        );

        // The path changes after the host accepted its scope but before the
        // canonical scanner opens it. On resume, the foreign prefix already
        // has a cursor, so only the appended record is in the admission page.
        let appended = json!({
            "type": "message", "id": "b8c9d0e1", "parentId": null,
            "timestamp": "2026-09-25T16:00:06.000Z",
            "message": {"role": "user", "content": "foreign appended text", "timestamp": 1_790_352_006_000_u64},
        });
        std::fs::write(&path, format!("{foreign_transcript}{appended}\n")).unwrap();
        let before_count = admission.observations().len();
        let before_calls = admission.capture_call_counts();
        let source = admission
            .observations()
            .first()
            .map(|stored| stored.observation().source().clone());
        let before_cursor = match &source {
            Some(source) => admission.get_source_cursor(source, &scope).await.unwrap(),
            None => None,
        };
        let memo_key = format!("host-memo://pi/file/v1/{}", super::source_digest(&path));
        let before_memo = admission
            .read_session_backfill_state(&scope, &memo_key)
            .await
            .unwrap();
        let stale = super::admit_session_file(
            &admission,
            &path,
            &header,
            witness,
            &scope,
            None,
            &ObservationCancellation::default(),
        )
        .await;
        assert!(matches!(
            stale,
            Err(crate::runtime::source::TranscriptIngestError::ScanGenerationChanged { .. })
        ));
        assert_eq!(admission.observations().len(), before_count);
        assert_eq!(admission.capture_call_counts(), before_calls);
        if let Some(source) = &source {
            assert_eq!(
                admission.get_source_cursor(source, &scope).await.unwrap(),
                before_cursor
            );
        } else {
            assert!(admission.non_durable_advances().is_empty());
        }
        assert_eq!(
            admission
                .read_session_backfill_state(&scope, &memo_key)
                .await
                .unwrap(),
            before_memo
        );

        let recovered = capture_pi_observations(
            &admission,
            &layout.source(),
            &foreign,
            scope,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
        assert!(!recovered.deferred);
        assert_eq!(admission.observations().len(), 10);

        // A complete resumed page has no new frames, but still carries the
        // canonical prefix's header proof; it cannot accept the stale header.
        let eof_calls = admission.capture_call_counts();
        assert!(matches!(
            super::admit_session_file(
                &admission,
                &path,
                &header,
                witness,
                &ObservationScopeV1::Profile,
                None,
                &ObservationCancellation::default(),
            )
            .await,
            Err(crate::runtime::source::TranscriptIngestError::ScanGenerationChanged { .. })
        ));
        let super::HeaderRead::Header { header, witness } =
            super::read_session_header(&path).unwrap()
        else {
            panic!("the recovered header must be valid");
        };
        let eof = super::admit_session_file(
            &admission,
            &path,
            &header,
            witness,
            &ObservationScopeV1::Profile,
            None,
            &ObservationCancellation::default(),
        )
        .await
        .unwrap();
        assert_eq!(eof.bytes_consumed, 0);
        assert!(!eof.source_deferred);
        assert_eq!(admission.capture_call_counts(), eof_calls);
    }
}

#[tokio::test]
async fn unavailable_queued_header_remains_retryable_without_a_quarantine_memo() {
    let layout = Layout::new();
    let path = layout.write_fixture();
    let admission = MemoryHostAdmission::default();
    capture_pi_observations(
        &admission,
        &layout.source(),
        &layout.project,
        ObservationScopeV1::Profile,
        Some(0),
        &ObservationCancellation::default(),
    )
    .await
    .unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let unavailable = capture(&layout, &admission).await;
    assert!(unavailable.deferred);
    assert_eq!(unavailable.discovery_failures, 1);
    assert!(admission.observations().is_empty());
    let key = format!("host-memo://pi/file/v1/{}", super::source_digest(&path));
    assert_eq!(
        admission
            .read_session_backfill_state(&ObservationScopeV1::Profile, &key)
            .await
            .unwrap(),
        None,
    );
    std::fs::remove_dir(&path).unwrap();
    layout.write_fixture();
    assert!(!capture(&layout, &admission).await.deferred);
    assert_eq!(admission.observations().len(), 9);
}

#[tokio::test]
async fn an_incomplete_rewritten_header_refuses_before_any_admission_mutation() {
    let layout = Layout::new();
    let path = layout.session_dir().join(FIXTURE_NAME);
    let header_bytes = format!(
        "{{\"type\":\"session\",\"version\":3,\"id\":\"{SESSION_ID}\",\"padding\":\"{}\",\"cwd\":{},\"timestamp\":\"2026-09-25T16:00:00.000Z\"}}\n",
        "x".repeat(2048),
        serde_json::to_string(&layout.project).unwrap(),
    );
    let (_, body) = FIXTURE.split_once('\n').unwrap();
    let original = format!("{header_bytes}{body}");
    std::fs::write(&path, &original).unwrap();
    let admission = MemoryHostAdmission::default();
    let scope = ObservationScopeV1::Profile;
    assert!(!capture(&layout, &admission).await.deferred);
    assert_eq!(admission.observations().len(), 9);
    let source = admission.observations()[0].observation().source().clone();
    let before_cursor = admission
        .get_source_cursor(&source, &scope)
        .await
        .unwrap()
        .unwrap();
    assert!(before_cursor.position() > header_bytes.len() as u64);
    let before_calls = admission.capture_call_counts();
    let before_coverage = admission.non_durable_advances().len();
    let memo_key = format!("host-memo://pi/file/v1/{}", super::source_digest(&path));
    let before_memo = admission
        .read_session_backfill_state(&scope, &memo_key)
        .await
        .unwrap();
    let super::HeaderRead::Header { header, witness } = super::read_session_header(&path).unwrap()
    else {
        panic!("the original header must be valid");
    };
    let before_identity = crate::runtime::source::jsonl_file_identity(&path).unwrap();
    // Keep the native identity's first 1024 bytes, while truncating the first
    // complete header after the separate scope read. The durable source still
    // has a nonzero cursor, but its replacement page cannot prove a header.
    std::fs::write(&path, &header_bytes.as_bytes()[..1500]).unwrap();
    assert_eq!(
        crate::runtime::source::jsonl_file_identity(&path).unwrap(),
        before_identity
    );
    let refused = super::admit_session_file(
        &admission,
        &path,
        &header,
        witness,
        &scope,
        Some(1),
        &ObservationCancellation::default(),
    )
    .await;
    assert!(matches!(
        refused,
        Err(crate::runtime::source::TranscriptIngestError::ScanGenerationChanged { .. })
    ));
    assert_eq!(admission.observations().len(), 9);
    assert_eq!(admission.capture_call_counts(), before_calls);
    assert_eq!(admission.non_durable_advances().len(), before_coverage);
    assert_eq!(
        admission.get_source_cursor(&source, &scope).await.unwrap(),
        Some(before_cursor)
    );
    assert_eq!(
        admission
            .read_session_backfill_state(&scope, &memo_key)
            .await
            .unwrap(),
        before_memo
    );

    std::fs::write(&path, original).unwrap();
    assert!(!capture(&layout, &admission).await.deferred);
    assert_eq!(admission.observations().len(), 9);
}

#[tokio::test]
async fn interrupted_invalid_header_reconciliation_preserves_partial_discovery() {
    for cancelled in [false, true] {
        let layout = Layout::new();
        layout.write_malformed_files(1);
        let (mut discovery, _) = layout
            .source()
            .discover(
                &layout.project,
                TranscriptDiscoveryBounds::from_discovered_units(MAX_DISCOVERY_CANDIDATES),
                None,
                HostScanBudget::new(
                    MAX_DISCOVERY_INPUT_BYTES,
                    MAX_DISCOVERY_UNITS,
                    std::time::Instant::now() + HOST_SCAN_WINDOW,
                    ObservationCancellation::default(),
                ),
                None,
            )
            .unwrap();
        assert!(discovery.scan_complete);
        assert_eq!(discovery.invalid_headers.len(), 1);
        let path = discovery.invalid_headers[0].path.clone();
        let witness = discovery.invalid_headers[0].witness;
        let cancellation = ObservationCancellation::default();
        let deadline = if cancelled {
            cancellation.cancel();
            std::time::Instant::now() + HOST_SCAN_WINDOW
        } else {
            std::time::Instant::now()
        };
        let mut budget = HostScanBudget::new(
            MAX_DISCOVERY_INPUT_BYTES,
            MAX_DISCOVERY_UNITS,
            deadline,
            cancellation,
        );
        let admission = MemoryHostAdmission::default();
        super::reconcile_invalid_headers(
            &admission,
            &ObservationScopeV1::Profile,
            &mut discovery,
            &mut budget,
        )
        .await
        .unwrap();
        assert!(
            !discovery.scan_complete,
            "the frontier must remain uncommitted"
        );
        assert!(budget.evidence().is_deferred());
        assert_eq!(discovery.failure_count, 0);
        assert_eq!(
            super::PathMemo::read(&admission, &ObservationScopeV1::Profile, &path)
                .await
                .unwrap()
                .disposition(Some(witness)),
            None,
            "interrupted discovery must not publish an unexamined refusal",
        );
        assert_eq!(capture(&layout, &admission).await.discovery_failures, 1);
        assert_eq!(capture(&layout, &admission).await.discovery_failures, 0);
    }
}
