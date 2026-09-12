use serde::{Deserialize, Serialize};
use tracedecay_contracts::{
    ApplicationOutcome, IssueTaskHandoffRequestV1, IssueTaskHandoffResultV1,
    ListTaskHandoffsRequestV1, ListTaskHandoffsResultV1, OpenInvestigationHandoffRequestV1,
    OpenInvestigationHandoffResultV1, OpenTaskHandoffRequestV1, OpenTaskHandoffResultV1,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum HandoffApplicationInvocationV1 {
    IssueTaskHandoff(IssueTaskHandoffRequestV1),
    ListTaskHandoffs(ListTaskHandoffsRequestV1),
    OpenInvestigationHandoff(OpenInvestigationHandoffRequestV1),
    OpenTaskHandoff(OpenTaskHandoffRequestV1),
}

impl HandoffApplicationInvocationV1 {
    #[hotpath::skip]
    pub const fn operation_key(&self) -> &'static str {
        match self {
            Self::IssueTaskHandoff(_) => "issue_task_handoff",
            Self::ListTaskHandoffs(_) => "list_task_handoffs",
            Self::OpenInvestigationHandoff(_) => "open_investigation_handoff",
            Self::OpenTaskHandoff(_) => "open_task_handoff",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "outcome", rename_all = "snake_case")]
pub enum HandoffApplicationOutcomeV1 {
    IssueTaskHandoff(ApplicationOutcome<IssueTaskHandoffResultV1>),
    ListTaskHandoffs(ApplicationOutcome<ListTaskHandoffsResultV1>),
    OpenInvestigationHandoff(ApplicationOutcome<OpenInvestigationHandoffResultV1>),
    OpenTaskHandoff(ApplicationOutcome<OpenTaskHandoffResultV1>),
}
