use serde::Deserialize;
use tracedecay_session_memory::provider_usage::ProviderUsageCostSummaryV1;

#[derive(Deserialize)]
pub(crate) struct CostAdminPayload {
    pub(crate) summary: CostSummaryPayload,
    pub(crate) today: TodayCostPayload,
}

#[derive(Deserialize)]
pub(crate) struct CostSummaryPayload {
    pub(crate) provider_usage: ProviderUsageCostSummaryV1,
    pub(crate) tokens_saved: u64,
    pub(crate) efficiency_ratio: Option<f64>,
}

#[derive(Deserialize)]
pub(crate) struct TodayCostPayload {
    pub(crate) provider_usage: ProviderUsageCostSummaryV1,
}
