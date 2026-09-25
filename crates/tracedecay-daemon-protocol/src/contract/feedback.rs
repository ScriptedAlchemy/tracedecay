use serde::{Deserialize, Serialize};
use tracedecay_contracts::{
    AuthorityReceipt, EvidenceAuthority, EvidenceCoverage, EvidencePacket, EvidenceScore, Omission,
    OperationReceipt, PageState, RetrieverContribution, TemporalState,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DaemonFeedbackResult {
    temporal: TemporalState,
    authority: AuthorityReceipt,
    evidence_authorities: Vec<EvidenceAuthority>,
    coverage: EvidenceCoverage,
    omissions: Vec<Omission>,
    scores: Vec<EvidenceScore>,
    contributions: Vec<RetrieverContribution>,
    page: PageState,
    execution: OperationReceipt,
    payload: Option<serde_json::Value>,
    /// Project-relative files the read touched, carried beside the packet
    /// onto the application envelope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    touched_files: Vec<String>,
}

impl DaemonFeedbackResult {
    /// Read-only views for the daemon's operation accounting. The fields stay
    /// private so the envelope can only be built from an application packet.
    #[hotpath::skip]
    pub const fn execution(&self) -> &OperationReceipt {
        &self.execution
    }

    #[hotpath::skip]
    pub const fn page(&self) -> &PageState {
        &self.page
    }

    pub fn from_application(packet: EvidencePacket<serde_json::Value>) -> Self {
        Self {
            temporal: packet.temporal,
            authority: packet.authority,
            evidence_authorities: packet.evidence_authorities,
            coverage: packet.coverage,
            omissions: packet.omissions,
            scores: packet.scores,
            contributions: packet.contributions,
            page: packet.page,
            execution: packet.execution,
            payload: packet.payload,
            touched_files: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_touched_files(mut self, touched_files: Vec<String>) -> Self {
        self.touched_files = touched_files;
        self
    }

    /// The evidence packet and the files the read touched.
    pub fn into_application(self) -> (EvidencePacket<serde_json::Value>, Vec<String>) {
        let packet = EvidencePacket {
            temporal: self.temporal,
            authority: self.authority,
            evidence_authorities: self.evidence_authorities,
            coverage: self.coverage,
            omissions: self.omissions,
            scores: self.scores,
            contributions: self.contributions,
            page: self.page,
            execution: self.execution,
            payload: self.payload,
        };
        (packet, self.touched_files)
    }
}
