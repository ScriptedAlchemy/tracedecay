use tracedecay_contracts::retrieval::AdminCliCostSummaryV1;
use tracedecay_session_memory::provider_usage::ProviderUsageCostSummaryV1;

pub(crate) struct CostAdminPayload {
    pub(crate) summary: CostSummaryPayload,
    pub(crate) today: TodayCostPayload,
}

pub(crate) struct CostSummaryPayload {
    pub(crate) provider_usage: ProviderUsageCostSummaryV1,
    pub(crate) tokens_saved: u64,
    pub(crate) efficiency_ratio: Option<f64>,
}

pub(crate) struct TodayCostPayload {
    pub(crate) provider_usage: ProviderUsageCostSummaryV1,
}

/// The owner's cost summary with its provider-usage summaries decoded as the
/// session memory prices them.
impl TryFrom<AdminCliCostSummaryV1> for CostAdminPayload {
    type Error = serde_json::Error;

    fn try_from(cost: AdminCliCostSummaryV1) -> Result<Self, Self::Error> {
        Ok(Self {
            summary: CostSummaryPayload {
                provider_usage: serde_json::from_value(cost.summary.provider_usage)?,
                tokens_saved: cost.summary.tokens_saved,
                efficiency_ratio: cost.summary.efficiency_ratio,
            },
            today: TodayCostPayload {
                provider_usage: serde_json::from_value(cost.today.provider_usage)?,
            },
        })
    }
}
