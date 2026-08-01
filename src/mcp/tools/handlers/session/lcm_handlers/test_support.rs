use std::sync::Mutex;

use serde_json::Value;
use tracedecay_domain::{
    RetrievalAnchorId, SessionSourceCoverageV1, SessionSourceFrontierV1, SessionSourceIdV1,
    SessionTemporalCoverageRequestV1, TemporalCoverageCountsV1, TemporalModeV1,
};

use super::super::message_search::{
    LcmDescribeServiceCommand, LcmDescribeServiceFuture, LcmDescribeServiceOutcome,
    LcmExpandServiceCommand, LcmExpandServiceFuture, LcmExpandServiceOutcome,
    SessionRetrievalCommand, SessionRetrievalExplanationView, SessionRetrievalPageView,
    SessionRetrievalServiceFuture, SessionRetrievalServiceOutcome, SessionRetrievalServicePort,
    SessionRetrievalUnavailable, SessionTemporalMetadataView, SessionTemporalWatermarksView,
};
use crate::application::session::SessionDataFreshness;
use crate::mcp::tools::ToolResult;
use crate::sessions::{SessionMessageRecord, SessionMessageSearchResult, SessionRecord};

pub(super) struct RecordingService {
    commands: Mutex<Vec<SessionRetrievalCommand>>,
    outcome: SessionRetrievalServiceOutcome,
    describe_commands: Mutex<Vec<LcmDescribeServiceCommand>>,
    describe_outcome: Mutex<LcmDescribeServiceOutcome>,
    expand_commands: Mutex<Vec<LcmExpandServiceCommand>>,
    expand_outcome: Mutex<LcmExpandServiceOutcome>,
}

impl RecordingService {
    pub(super) fn new(outcome: SessionRetrievalServiceOutcome) -> Self {
        Self {
            commands: Mutex::new(Vec::new()),
            outcome,
            describe_commands: Mutex::new(Vec::new()),
            describe_outcome: Mutex::new(LcmDescribeServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            )),
            expand_commands: Mutex::new(Vec::new()),
            expand_outcome: Mutex::new(LcmExpandServiceOutcome::Unavailable(
                SessionRetrievalUnavailable::service_not_configured(),
            )),
        }
    }

    pub(super) fn command(&self) -> SessionRetrievalCommand {
        self.commands.lock().unwrap().last().unwrap().clone()
    }

    pub(super) fn calls(&self) -> usize {
        self.commands.lock().unwrap().len()
    }

    pub(super) fn set_describe_outcome(&self, outcome: LcmDescribeServiceOutcome) {
        *self.describe_outcome.lock().unwrap() = outcome;
    }

    pub(super) fn describe_command(&self) -> LcmDescribeServiceCommand {
        self.describe_commands
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .clone()
    }

    pub(super) fn set_expand_outcome(&self, outcome: LcmExpandServiceOutcome) {
        *self.expand_outcome.lock().unwrap() = outcome;
    }

    pub(super) fn expand_command(&self) -> LcmExpandServiceCommand {
        self.expand_commands.lock().unwrap().last().unwrap().clone()
    }

    pub(super) fn expand_calls(&self) -> usize {
        self.expand_commands.lock().unwrap().len()
    }
}

impl SessionRetrievalServicePort for RecordingService {
    fn execute(&self, command: SessionRetrievalCommand) -> SessionRetrievalServiceFuture<'_> {
        self.commands.lock().unwrap().push(command);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }

    fn describe_lcm(&self, command: LcmDescribeServiceCommand) -> LcmDescribeServiceFuture<'_> {
        self.describe_commands.lock().unwrap().push(command);
        let outcome = self.describe_outcome.lock().unwrap().clone();
        Box::pin(async move { outcome })
    }

    fn expand_lcm(&self, command: LcmExpandServiceCommand) -> LcmExpandServiceFuture<'_> {
        self.expand_commands.lock().unwrap().push(command);
        let outcome = self.expand_outcome.lock().unwrap().clone();
        Box::pin(async move { outcome })
    }
}

pub(super) fn temporal(cursor: Option<&str>) -> SessionTemporalMetadataView {
    SessionTemporalMetadataView {
        anchors: vec![RetrievalAnchorId::new("anchor.compatibility.1").unwrap()],
        watermarks: SessionTemporalWatermarksView {
            generation: 9,
            source: 8,
            projection: 7,
            index: 6,
            summary: 5,
        },
        coverage: TemporalCoverageCountsV1 {
            visible: 1,
            hidden: 0,
            unknown: 0,
            redacted: 0,
        },
        source_coverage: vec![
            SessionSourceCoverageV1::from_frontiers(
                SessionSourceIdV1::new("claude").unwrap(),
                SessionSourceFrontierV1::new(9),
                SessionSourceFrontierV1::new(9),
                SessionSourceFrontierV1::new(9),
                SessionTemporalCoverageRequestV1::new(TemporalModeV1::Current),
            )
            .unwrap(),
        ],
        cursor: cursor.map(str::to_string),
        explanations: vec![SessionRetrievalExplanationView {
            anchor: RetrievalAnchorId::new("anchor.compatibility.1").unwrap(),
            summary: "exact canonical occurrence".to_string(),
        }],
        omissions: Vec::new(),
        authorized_root: Some("/project".to_string()),
    }
}

pub(super) fn result(text: &str, role: &str) -> SessionMessageSearchResult {
    SessionMessageSearchResult {
        session: SessionRecord {
            provider: "claude".to_string(),
            session_id: "session-exact".to_string(),
            project_key: "project".to_string(),
            project_path: "/project".to_string(),
            title: None,
            started_at: Some(10),
            ended_at: None,
            transcript_path: None,
            metadata_json: None,
            parent_session_id: None,
            is_subagent: false,
            agent_id: None,
            parent_tool_use_id: None,
        },
        message: SessionMessageRecord {
            provider: "claude".to_string(),
            message_id: "message-1".to_string(),
            session_id: "session-exact".to_string(),
            role: role.to_string(),
            timestamp: Some(20),
            ordinal: 3,
            text: text.to_string(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        },
        score: 0.875,
    }
}

pub(super) fn summary_result(text: &str, node_id: &str) -> SessionMessageSearchResult {
    let mut result = result(text, "summary");
    result.message.message_id = node_id.to_string();
    result.message.kind = Some("summary".to_string());
    result
}

pub(super) fn complete(
    text: &str,
    role: &str,
    cursor: Option<&str>,
) -> SessionRetrievalServiceOutcome {
    SessionRetrievalServiceOutcome::Complete {
        page: SessionRetrievalPageView {
            results: vec![result(text, role)],
            temporal: temporal(cursor),
        },
        freshness: SessionDataFreshness::Fresh,
    }
}

pub(super) fn payload(result: ToolResult) -> Value {
    serde_json::from_str(
        result.value["content"][0]["text"]
            .as_str()
            .expect("JSON tool result text"),
    )
    .expect("valid JSON tool result")
}

pub(super) fn response_text(result: ToolResult) -> String {
    result.value["content"][0]["text"]
        .as_str()
        .expect("tool result text")
        .to_string()
}
