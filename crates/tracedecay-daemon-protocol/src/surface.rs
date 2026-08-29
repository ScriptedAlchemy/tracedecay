//! Wire DTOs that the daemon invocation contract carries but that cannot live
//! in `tracedecay-application` without inverting the crate DAG.
//!
//! `GitReadSurfaceRequest` embeds the usecases Git-read enum (application
//! cannot depend on usecases). Context Scout delivery carries the host-runtime
//! durable claim (application cannot depend on agent-hosts).

use serde::{Deserialize, Serialize};
use tracedecay_api::HttpApplicationOperation;
use tracedecay_domain::configuration::{ConfigurationIdempotencyKey, ConfigurationRevisionId};
use tracedecay_usecases::git_reads::GitReadRequestV1;

pub type ApplicationSurfaceOperation = HttpApplicationOperation;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitReadSurfaceRequest {
    pub request: GitReadRequestV1,
    pub max_entries: u32,
    pub max_bytes: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContextScoutClaimWindowSurfaceV1 {
    IdleWindow,
    OnRequest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutExactAddressSurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutRecentSurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
    #[serde(default = "default_context_scout_recent_limit")]
    pub limit: usize,
}

const fn default_context_scout_recent_limit() -> usize {
    8
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutControlSurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
    pub expected_revision: ConfigurationRevisionId,
    pub idempotency_key: ConfigurationIdempotencyKey,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutCancelSurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
    pub work: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutWorkV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutClaimSurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
    pub window: ContextScoutClaimWindowSurfaceV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutDeliverySurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
    pub claim: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutDurableClaimV1,
    pub receipt: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ContextScoutFeedbackSurfaceRequest {
    pub address: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1,
    pub receipt: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutDeliveryReceiptV1,
    pub feedback: tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutFeedbackV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation", content = "request")]
pub enum ContextScoutSurfaceRequest {
    Status(ContextScoutExactAddressSurfaceRequest),
    Recent(ContextScoutRecentSurfaceRequest),
    Explain(ContextScoutRecentSurfaceRequest),
    Capability(ContextScoutExactAddressSurfaceRequest),
    Budget(ContextScoutExactAddressSurfaceRequest),
    Pause(ContextScoutControlSurfaceRequest),
    Resume(ContextScoutControlSurfaceRequest),
    Cancel(ContextScoutCancelSurfaceRequest),
    Claim(ContextScoutClaimSurfaceRequest),
    Delivery(Box<ContextScoutDeliverySurfaceRequest>),
    Feedback(ContextScoutFeedbackSurfaceRequest),
}

impl ContextScoutSurfaceRequest {
    pub const fn address(
        &self,
    ) -> tracedecay_agent_hosts::agents::context_scout_v2::ContextScoutAddressV1 {
        match self {
            Self::Status(request) | Self::Capability(request) | Self::Budget(request) => {
                request.address
            }
            Self::Recent(request) | Self::Explain(request) => request.address,
            Self::Pause(request) | Self::Resume(request) => request.address,
            Self::Cancel(request) => request.address,
            Self::Claim(request) => request.address,
            Self::Delivery(request) => request.address,
            Self::Feedback(request) => request.address,
        }
    }

    pub const fn matches(&self, operation: ApplicationSurfaceOperation) -> bool {
        matches!(
            (self, operation),
            (
                Self::Status(_),
                ApplicationSurfaceOperation::ContextScoutStatus
            ) | (
                Self::Recent(_),
                ApplicationSurfaceOperation::ContextScoutRecent
            ) | (
                Self::Explain(_),
                ApplicationSurfaceOperation::ContextScoutExplain
            ) | (
                Self::Capability(_),
                ApplicationSurfaceOperation::ContextScoutCapability
            ) | (
                Self::Budget(_),
                ApplicationSurfaceOperation::ContextScoutBudget
            ) | (
                Self::Pause(_),
                ApplicationSurfaceOperation::ContextScoutPause
            ) | (
                Self::Resume(_),
                ApplicationSurfaceOperation::ContextScoutResume
            ) | (
                Self::Cancel(_),
                ApplicationSurfaceOperation::ContextScoutCancel
            ) | (
                Self::Claim(_),
                ApplicationSurfaceOperation::ContextScoutClaim
            ) | (
                Self::Delivery(_),
                ApplicationSurfaceOperation::ContextScoutDelivery
            ) | (
                Self::Feedback(_),
                ApplicationSurfaceOperation::ContextScoutFeedback
            )
        )
    }
}
