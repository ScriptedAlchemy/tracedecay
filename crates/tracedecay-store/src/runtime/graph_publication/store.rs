use super::{
    GraphProjectionIdentityV1, GraphPublicationKeyV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationProjectionPageV1,
    GraphPublicationReplayLookupV1, GraphPublicationReplayPageRequestV1,
    GraphPublicationReplayPageV1, GraphPublicationReplayRecordV1,
    GraphPublicationReplayRetirementV1, GraphPublicationReplayV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
    GraphPublicationStoreResultV1, GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1,
};

pub trait GraphPublicationStoreV1 {
    fn append_replay(
        &mut self,
        publication: &GraphPublicationReplayV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayAppendOutcomeV1>;

    fn pending_replay(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>>;

    fn replay(
        &mut self,
        key: &GraphPublicationKeyV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayLookupV1>;

    fn replay_page(
        &mut self,
        request: &GraphPublicationReplayPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayPageV1>;

    fn projection_page(
        &mut self,
        request: &GraphPublicationProjectionPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationProjectionPageV1>;

    fn retire_replay(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1>;

    fn retired_cleanup_page(
        &mut self,
        request: &GraphPublicationRetiredCleanupPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationRetiredCleanupPageV1>;

    fn finalize_retired_replay_cleanup(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphRetiredReplayCleanupFinalizeOutcomeV1>;

    fn verified_head(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>>;

    fn compare_and_swap_verified_head(
        &mut self,
        request: &GraphVerifiedHeadCompareAndSwapV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadCasOutcomeV1>;
}
