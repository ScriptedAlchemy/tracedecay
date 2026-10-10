use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tracedecay_domain::{
    CanonicalGitEvidenceKindV1, CanonicalMessageRoleV1, CanonicalObservationEnvelopeV1,
    CanonicalObservationFactV1, ObservationId, ObservationScopeV1,
};

use super::{
    PI_AGENT_RELATIVE, PiSource, capture_pi_observations, capture_pi_session, pi_agent_dir_for,
    session_dir_name,
};
use crate::admission::{HostAdmission, test_support::MemoryHostAdmission};
use crate::observation::ObservationCancellation;

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
        None,
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
        8,
        "the contentless message line is refused and every other fixture line is one observation"
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

    let coverage = admission
        .get_parse_offset(&ObservationScopeV1::Profile, "host-coverage://pi/v1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(coverage.file_id, 1);

    // An unchanged session resumes at its cursor and admits nothing twice.
    capture(&layout, &admission).await;
    assert_eq!(admission.observations().len(), 8);
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
    // The encoded project directory includes the platform's temporary ancestry,
    // which can itself contain high-entropy tokens. Derive only this variable
    // path through the canonical sanitizer; keep the full payload byte check.
    let transcript = layout.session_dir().join(FIXTURE_NAME);
    let expected_transcript =
        tracedecay_privacy::sanitize_provider_metadata_text(&transcript.to_string_lossy())
            .expect("fixture transcript path is sanitizable");
    let expected_transcript = serde_json::to_string(&expected_transcript)
        .unwrap()
        .replace(&escaped[1..escaped.len() - 1], "/project")
        .replace(&escaped_temp[1..escaped_temp.len() - 1], "/tmp-root");

    // The contentless user message (b7..b8) is refused, so session_info is
    // the next observation.
    assert_eq!(
        normalized,
        format!(
            r#"{{"evidence":{{"native_timestamp":1790352000,"ordering_domain":"file_bytes","range":{{"end":{b1},"start":{b0}}}}},"facts":[{{"kind":"session","native_source":"pi","project_path":"/project","source":"pi_session","started_at":1790352000,"transcript_path":{expected_transcript}}}],"native_record_kind":"session","provider":"pi","relations":{{"session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:session","version":1}}{{"evidence":{{"native_timestamp":1790352000,"ordering_domain":"file_bytes","range":{{"end":{b2},"start":{b1}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"kind":"unknown","native_kind":"message.system","state":"unsupported"}}],"native_record_kind":"message.system","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a0b1c2d3","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a0b1c2d3","version":1}}{{"evidence":{{"native_timestamp":1790352000,"ordering_domain":"file_bytes","range":{{"end":{b3},"start":{b2}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"kind":"unknown","native_kind":"model_change","state":"unsupported"}}],"native_record_kind":"model_change","provider":"pi","relations":{{"parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a0b1c2d3","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:b1c2d3e4","version":1}}{{"evidence":{{"native_timestamp":1790352001,"ordering_domain":"file_bytes","range":{{"end":{b4},"start":{b3}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":"Fix the flaky billing retry test","kind":"message","role":"user","timestamp":1790352001}}],"native_record_kind":"message.user","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:c2d3e4f5","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:b1c2d3e4","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:c2d3e4f5","version":1}}{{"evidence":{{"native_timestamp":1790352002,"ordering_domain":"file_bytes","range":{{"end":{b5},"start":{b4}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":"The retry loop reads the clock twice.","kind":"reasoning","visibility":"visible"}},{{"content":{{"change_type":"edit","edited_at_micros":1790352002000000,"id":"toolu_01PiEdit","type":"toolCall"}},"evidence_kind":"file_edit","kind":"git","reference":"src/billing/retry.rs"}},{{"arguments":{{"newText":"<REDACTED_NEW_TEXT>","oldText":"<REDACTED_OLD_TEXT>","path":"src/billing/retry.rs"}},"invocation_id":"toolu_01PiEdit","kind":"tool_invocation","name":"edit"}},{{"content":[{{"text":"I will pin the retry clock in the billing test.","type":"text"}}],"kind":"message","model":"claude-sonnet-4-5","role":"assistant","timestamp":1790352002}}],"native_record_kind":"message.assistant","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:d3e4f5a6","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:c2d3e4f5","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:d3e4f5a6","version":1}}{{"evidence":{{"native_timestamp":1790352003,"ordering_domain":"file_bytes","range":{{"end":{b6},"start":{b5}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":[{{"text":"<REDACTED_TOOL_OUTPUT>","type":"text"}}],"invocation_id":"toolu_01PiEdit","kind":"tool_result","success":true,"token_count":8}}],"native_record_kind":"message.toolResult","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:e4f5a6b7","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:d3e4f5a6","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:e4f5a6b7","version":1}}{{"evidence":{{"native_timestamp":1790352004,"ordering_domain":"file_bytes","range":{{"end":{b7},"start":{b6}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","transcript_path":{expected_transcript}}},{{"content":[{{"text":"The billing retry test now pins its clock.","type":"text"}}],"kind":"message","model":"claude-sonnet-4-5","role":"assistant","timestamp":1790352004}}],"native_record_kind":"message.assistant","provider":"pi","relations":{{"message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:e4f5a6b7","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","version":1}}{{"evidence":{{"native_timestamp":1790352005,"ordering_domain":"file_bytes","range":{{"end":{b9},"start":{b8}}}}},"facts":[{{"kind":"session","native_source":"pi","source":"pi_session","title":"Billing retry clock","transcript_path":{expected_transcript}}}],"native_record_kind":"session_info","provider":"pi","relations":{{"parent_message_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:f5a6b7c8","session_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90"}},"stable_record_id":"5f0c2a8e-3b1d-4c7e-9a2f-6d8e1b4c7a90:a6b7c8d9","version":1}}"#
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
    assert_eq!(stored.len(), 8);
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
