use super::{
    GraphPendingReplayDiscardOutcomeV1, GraphPendingReplayDiscardV1, GraphProjectionIdentityV1,
    GraphPublicationKeyV1, GraphPublicationOperationContextV1,
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

    /// Retire the replay that is currently the projection's verified head.
    ///
    /// Used only for code generations the code index has already deleted:
    /// under the per-generation code-graph namespace every code generation is
    /// the permanent head of its own projection, so ordinary [`Self::retire_replay`]
    /// (which protects the head) could never reclaim it. Compare-and-swap
    /// shaped: `expected_head` must be the exact current head and must name the
    /// same replay as `request.key`; a projection with a pending replay after
    /// the head, a changed head, or active inbound dependencies is refused with
    /// the same typed outcomes as [`Self::retire_replay`]. On success the head
    /// row is removed and the replay is tombstoned in one transaction.
    fn retire_verified_head_replay(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        expected_head: &GraphVerifiedHeadV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1>;

    /// Delete one exact pending journaled replay row that an interrupted
    /// publisher can never complete, so the journal position reopens for a
    /// fresh append of the same key. Compare-and-swap shaped: refuses when
    /// the row completed, was superseded, or moved to another sequence.
    fn discard_pending_replay(
        &mut self,
        request: &GraphPendingReplayDiscardV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPendingReplayDiscardOutcomeV1>;

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
