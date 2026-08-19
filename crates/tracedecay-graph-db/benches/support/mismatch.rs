use sha2::{Digest, Sha256};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, VerifiedGraphSnapshot,
};
use tracedecay_store::{
    GraphProjectionIdentityV1, GraphPublicationInputDigestV1, GraphPublicationOperationContextV1,
    GraphPublicationProjectionPageRequestV1, GraphPublicationProjectionPageV1,
    GraphPublicationReplayLookupV1, GraphPublicationReplayPageRequestV1,
    GraphPublicationReplayPageV1, GraphPublicationReplayRecordV1,
    GraphPublicationReplayRetirementV1, GraphPublicationReplayV1,
    GraphPublicationRetiredCleanupPageRequestV1, GraphPublicationRetiredCleanupPageV1,
    GraphPublicationStoreResultV1, GraphPublicationStoreV1, GraphRecoveredGenerationDigestV1,
    GraphReplayAppendOutcomeV1, GraphReplayRetirementOutcomeV1,
    GraphRetiredReplayCleanupFinalizeOutcomeV1, GraphVerifiedHeadCasOutcomeV1,
    GraphVerifiedHeadCompareAndSwapV1, GraphVerifiedHeadV1,
};

use crate::support::{PersistentBenchmarkGraph, operation_control};

/// A persistent graph whose next-generation replay survived a rejected
/// recovered-digest CAS. The verified baseline and native replacement are
/// retained; no destructive recovery is performed.
pub struct ExactMismatchReplay {
    graph: PersistentBenchmarkGraph,
    manifest: GraphGenerationManifest,
    key: tracedecay_store::GraphPublicationKeyV1,
}

impl ExactMismatchReplay {
    pub fn prepare(
        baseline_manifest: GraphGenerationManifest,
        manifest: GraphGenerationManifest,
    ) -> Self {
        let mut graph = PersistentBenchmarkGraph::new();
        drop(graph.publish(baseline_manifest, None));
        drop(graph.recover_snapshot());
        let prior_head = graph
            .latest_head
            .clone()
            .expect("benchmark baseline has a verified head");

        graph.sequence += 1;
        let (append_control, append_probe) = operation_control(graph.sequence);
        let append_context =
            GraphPublicationOperationContextV1::new(&append_control, &append_probe)
                .expect("benchmark replay append context is valid");
        let input_digest = GraphPublicationInputDigestV1::new(format!(
            "sha256:{}",
            hex::encode(Sha256::digest(
                serde_json::to_vec(&manifest).expect("benchmark manifest serializes")
            ))
        ))
        .expect("benchmark publication input digest is valid");
        let replay = manifest
            .relational_metadata_replay(
                graph.binding.shard_id.clone(),
                GraphIdempotencyKey::new("benchmark-publication:mismatch")
                    .expect("benchmark idempotency identity is valid"),
                input_digest,
                Some(prior_head.clone()),
                &|| Ok(()),
            )
            .expect("benchmark replay is valid");
        let key = replay.key.clone();
        graph
            .authority
            .append_replay(&replay, &append_context)
            .expect("benchmark replay persists");

        graph.sequence += 1;
        let (publish_control, publish_probe) = operation_control(graph.sequence);
        let publish_context =
            GraphPublicationOperationContextV1::new(&publish_control, &publish_probe)
                .expect("benchmark publication context is valid");
        let registration = graph.registration();
        let mut mismatch = ExactRecoveredDigestMismatch::new(&mut graph.authority);
        let error = graph
            .registry
            .publish_verified(
                registration,
                &mut mismatch,
                &publish_context,
                &key,
                Some(manifest.clone()),
            )
            .expect_err("the exact recovered-digest mismatch must reject publication");
        assert!(
            matches!(error, GraphDbError::GenerationMismatch { .. }),
            "the benchmark setup must fail through the typed mismatch path: {error}"
        );
        assert!(
            mismatch.injected,
            "the benchmark setup must reach the exact verified-head CAS"
        );
        assert_eq!(
            graph
                .authority
                .verified_head(&key.projection, &publish_context)
                .expect("benchmark verified head remains readable"),
            Some(prior_head),
            "a rejected recovered digest must preserve the verified baseline"
        );
        assert!(matches!(
            graph
                .authority
                .replay(&key, &publish_context)
                .expect("benchmark replay remains readable"),
            GraphPublicationReplayLookupV1::Active(_)
        ));
        assert!(
            graph
                .registry
                .close(&graph.registration())
                .expect("mismatched benchmark graph closes"),
            "mismatched benchmark graph must be persisted before timing replay"
        );

        Self {
            graph,
            manifest,
            key,
        }
    }

    pub fn replay(mut self) -> VerifiedGraphSnapshot {
        self.graph.sequence += 1;
        let (control, probe) = operation_control(self.graph.sequence);
        let context = GraphPublicationOperationContextV1::new(&control, &probe)
            .expect("benchmark mismatch replay context is valid");
        let registration = self.graph.registration();
        let commit = self
            .graph
            .registry
            .publish_verified(
                registration,
                &mut self.graph.authority,
                &context,
                &self.key,
                Some(self.manifest),
            )
            .expect("exact mismatch replay reopens and verifies");
        assert_eq!(commit.head.key, self.key);
        commit.snapshot
    }
}

struct ExactRecoveredDigestMismatch<'a> {
    inner: &'a mut dyn GraphPublicationStoreV1,
    injected: bool,
}

impl<'a> ExactRecoveredDigestMismatch<'a> {
    fn new(inner: &'a mut dyn GraphPublicationStoreV1) -> Self {
        Self {
            inner,
            injected: false,
        }
    }
}

impl GraphPublicationStoreV1 for ExactRecoveredDigestMismatch<'_> {
    fn append_replay(
        &mut self,
        publication: &GraphPublicationReplayV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayAppendOutcomeV1> {
        self.inner.append_replay(publication, context)
    }

    fn pending_replay(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<Option<GraphPublicationReplayRecordV1>> {
        self.inner.pending_replay(projection, context)
    }

    fn replay(
        &mut self,
        key: &tracedecay_store::GraphPublicationKeyV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayLookupV1> {
        self.inner.replay(key, context)
    }

    fn replay_page(
        &mut self,
        request: &GraphPublicationReplayPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationReplayPageV1> {
        self.inner.replay_page(request, context)
    }

    fn projection_page(
        &mut self,
        request: &GraphPublicationProjectionPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationProjectionPageV1> {
        self.inner.projection_page(request, context)
    }

    fn retire_replay(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphReplayRetirementOutcomeV1> {
        self.inner.retire_replay(request, context)
    }

    fn retired_cleanup_page(
        &mut self,
        request: &GraphPublicationRetiredCleanupPageRequestV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphPublicationRetiredCleanupPageV1> {
        self.inner.retired_cleanup_page(request, context)
    }

    fn finalize_retired_replay_cleanup(
        &mut self,
        request: &GraphPublicationReplayRetirementV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphRetiredReplayCleanupFinalizeOutcomeV1> {
        self.inner.finalize_retired_replay_cleanup(request, context)
    }

    fn verified_head(
        &mut self,
        projection: &GraphProjectionIdentityV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<Option<GraphVerifiedHeadV1>> {
        self.inner.verified_head(projection, context)
    }

    fn compare_and_swap_verified_head(
        &mut self,
        request: &GraphVerifiedHeadCompareAndSwapV1,
        context: &GraphPublicationOperationContextV1<'_>,
    ) -> GraphPublicationStoreResultV1<GraphVerifiedHeadCasOutcomeV1> {
        assert!(!self.injected, "the exact mismatch is injected once");
        self.injected = true;
        let mut mismatched = request.clone();
        let actual = request.recovered_digest.as_str();
        let replacement = if actual.as_bytes()[7] == b'0' {
            '1'
        } else {
            '0'
        };
        mismatched.recovered_digest =
            GraphRecoveredGenerationDigestV1::new(format!("sha256:{replacement}{}", &actual[8..]))
                .expect("benchmark mismatch digest is valid and differs exactly once");
        self.inner
            .compare_and_swap_verified_head(&mismatched, context)
    }
}
