use std::collections::{BTreeMap, HashSet, VecDeque};
use std::ops::Range;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::thread;
use std::vec::IntoIter;

use grafeo_core::graph::Direction;
use parking_lot::RwLockWriteGuard as ParkingRwLockWriteGuard;
use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

use crate::generation::verify_recovered_generation;
use crate::lease::{
    GenerationLocator, VerifiedGenerationLease, VerifiedGraphSnapshot, VerifiedTraversalResult,
    VerifiedTraversalVisit,
};
use crate::limits::{
    MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES, MAX_NATIVE_GENERATION_STAGE_MUTATIONS,
    MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES, MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
};
use crate::projection::graph_properties_live_bytes;
use crate::recovery::{
    checkpoint_recovered_database, is_database_fault, open_recovered_database,
    quarantine_transition_failure, requarantine_after_failed_checkpoint_verification,
    set_projection_quarantine,
};
use crate::runtime::{GraphBatchPlan, PreparedGraphBatch};
use crate::schema::{NAMESPACE_PROPERTY, relation_kind_from_type, required_string};
use crate::sealed_store::SealedStoreInstall;
use crate::state::{
    EndpointIdentityCache, latest_projection, load_relation, load_relation_by_edge_cached,
    projection_entity_deletion_page_checked, projection_relation_deletion_page_checked,
};
use crate::verified_marker::GenerationVerification;
use crate::{
    GraphBudgetKind, GraphCancellation, GraphCommit, GraphDb, GraphDbError, GraphEntityRef,
    GraphGenerationManifest, GraphGenerationManifestIdentity, GraphGenerationRelation,
    GraphIdempotencyKey, GraphMutation, GraphNamespace, GraphRelation, GraphRelationRef,
    GraphTraversalDirection, GraphWriteBatch, TraversalRequest, mutation,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GenerationStagePageKind {
    Entities,
    Relations,
}

impl GenerationStagePageKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Entities => "entities",
            Self::Relations => "relations",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GenerationStagePage {
    ordinal: usize,
    kind: GenerationStagePageKind,
    range: Range<usize>,
    live_bytes: usize,
}

impl GenerationStagePage {
    fn mutation_count(&self) -> usize {
        self.range.end - self.range.start
    }

    fn live_bytes(&self) -> usize {
        self.live_bytes
    }
}

struct GenerationStageContext {
    locator: GenerationLocator,
    physical_namespace: GraphNamespace,
    dependency_namespaces: BTreeMap<crate::GraphProjectionIdentity, GraphNamespace>,
    dependency_digest: tracedecay_store::runtime::GraphDependencyGenerationClosureDigestV1,
}

/// CPU-only page batch, built outside the exclusive snapshot gate so the next
/// page can construct while the current page applies.
struct PreparedGenerationStagePage {
    batch: GraphWriteBatch,
    endpoint_namespaces: mutation::RelationEndpointNamespaces,
    digest: String,
}

enum OwnedGenerationStagePage {
    Entities(Vec<crate::GraphEntity>),
    Relations(Vec<GraphGenerationRelation>),
}

struct OwnedGenerationStageRows {
    entities: IntoIter<crate::GraphEntity>,
    relations: IntoIter<GraphGenerationRelation>,
    entity_offset: usize,
    relation_offset: usize,
}

impl OwnedGenerationStageRows {
    fn new(entities: Vec<crate::GraphEntity>, relations: Vec<GraphGenerationRelation>) -> Self {
        Self {
            entities: entities.into_iter(),
            relations: relations.into_iter(),
            entity_offset: 0,
            relation_offset: 0,
        }
    }

    fn take_page(
        &mut self,
        page: &GenerationStagePage,
    ) -> Result<OwnedGenerationStagePage, GraphDbError> {
        let count = page.mutation_count();
        match page.kind {
            GenerationStagePageKind::Entities => {
                if page.range.start != self.entity_offset {
                    return Err(GraphDbError::conflict(
                        "generation_runtime.owned_stage_rows",
                    ));
                }
                let rows = self.entities.by_ref().take(count).collect::<Vec<_>>();
                if rows.len() != count {
                    return Err(GraphDbError::conflict(
                        "generation_runtime.owned_stage_rows",
                    ));
                }
                self.entity_offset =
                    self.entity_offset
                        .checked_add(count)
                        .ok_or(GraphDbError::conflict(
                            "generation_runtime.owned_stage_rows",
                        ))?;
                Ok(OwnedGenerationStagePage::Entities(rows))
            }
            GenerationStagePageKind::Relations => {
                if page.range.start != self.relation_offset {
                    return Err(GraphDbError::conflict(
                        "generation_runtime.owned_stage_rows",
                    ));
                }
                let rows = self.relations.by_ref().take(count).collect::<Vec<_>>();
                if rows.len() != count {
                    return Err(GraphDbError::conflict(
                        "generation_runtime.owned_stage_rows",
                    ));
                }
                self.relation_offset =
                    self.relation_offset
                        .checked_add(count)
                        .ok_or(GraphDbError::conflict(
                            "generation_runtime.owned_stage_rows",
                        ))?;
                Ok(OwnedGenerationStagePage::Relations(rows))
            }
        }
    }

    fn finish(self) -> Result<(), GraphDbError> {
        if self.entities.len() == 0 && self.relations.len() == 0 {
            Ok(())
        } else {
            Err(GraphDbError::conflict(
                "generation_runtime.owned_stage_rows",
            ))
        }
    }
}

pub(crate) enum GenerationStageOutcome {
    Applied(GraphCommit),
    Reseated(GraphCommit),
}

impl GenerationStageOutcome {
    pub(crate) fn commit(self) -> GraphCommit {
        match self {
            Self::Applied(commit) | Self::Reseated(commit) => commit,
        }
    }
}

#[derive(Clone, Copy)]
enum GenerationRetirementPageKind {
    Relations,
    Entities,
}

impl GraphDb {
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    #[hotpath::measure(label = "graph_db.generation.verify_in_place", impl_type = "GraphDb")]
    pub(crate) fn verify_generation_in_place(
        &self,
        manifest: &GraphGenerationManifest,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        check()?;
        let expected = manifest.expected_recovered_digest(check)?;
        let identity = manifest.identity();
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        verify_recovered_generation(database, &identity, &expected, check)
            .map(|(verified, _)| verified)
    }

    /// Establishes `expected` for an already-stored generation on an
    /// **activation** path, re-hashing only when it has to.
    ///
    /// This is the verify-once boundary. A verified-generation marker that was
    /// written against the exact container backing this open, and that records
    /// this exact expected digest, means the full proof has already run over
    /// these bytes and does not run again. Every other case -- no marker, a
    /// marker for other bytes, a marker that does not name this generation, a
    /// digest that differs by one character, or a container this process has
    /// since written to -- falls through to the full row-streaming proof.
    ///
    /// `expected` always comes from the relational authority, never from the
    /// marker, so a marker can only ever assert freshness. It cannot name
    /// which generation is served and it cannot make a wrong digest pass.
    /// Corruption remains a typed failure from the full proof: this returns
    /// `Err` exactly where the full proof would have.
    #[hotpath::measure(label = "graph_db.generation.verify_activated", impl_type = "GraphDb")]
    pub(crate) fn verify_activated_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphRecoveredGenerationDigestV1, GraphDbError> {
        check()?;
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        if let Some(canonical_bytes) = self.inner.markers.lookup(&locator, expected.as_str()) {
            // Carry the inherited proof into this open's published set, so a
            // daemon that only ever serves reads does not drop it at close.
            self.inner.markers.record_fresh(&locator);
            crate::hotpath_observe::record_generation_verification(
                GenerationVerification::VerifiedFresh,
                canonical_bytes,
            );
            return Ok(expected.clone());
        }
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let (verified, canonical_bytes) =
            verify_recovered_generation(database, identity, expected, check)?;
        drop(guard);
        self.inner
            .markers
            .record_proven(&locator, verified.as_str(), canonical_bytes);
        crate::hotpath_observe::record_generation_verification(
            GenerationVerification::Reverified,
            canonical_bytes,
        );
        Ok(verified)
    }

    /// Discards every marker admitted at open, so the next activation of each
    /// generation re-derives its digest from the stored rows.
    ///
    /// The explicit full re-verify hook. File identity standing in for content
    /// is an OS-integrity assumption, and this is how an operator or a
    /// scheduled audit drops it.
    pub(crate) fn forget_verified_markers(&self) {
        self.inner.markers.forget_admitted();
    }

    #[hotpath::measure(label = "graph_db.generation.verify_existing", impl_type = "GraphDb")]
    pub(crate) fn verify_existing_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        check()?;
        self.ensure_opened()?;
        let physical_namespace = identity.physical_namespace()?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let commit = latest_projection(
            database,
            &physical_namespace,
            &identity.projection.projection,
        )?
        .ok_or_else(|| {
            GraphDbError::unavailable(
                "metadata-only graph replay has no complete native generation rows",
            )
        })?
        .commit;
        let (recovered, canonical_bytes) =
            verify_recovered_generation(database, identity, expected, check)?;
        drop(guard);
        // First-publication verification is untouched -- it still streams every
        // row. Recording the proof it just established is what lets the *next*
        // open skip it.
        self.record_proven_generation(identity, &recovered, canonical_bytes);
        Ok((commit, recovered))
    }

    /// Files a completed full proof against the container's marker set.
    fn record_proven_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
        verified: &GraphRecoveredGenerationDigestV1,
        canonical_bytes: u64,
    ) {
        self.inner.markers.record_proven(
            &GenerationLocator::new(identity.projection.clone(), identity.generation.clone()),
            verified.as_str(),
            canonical_bytes,
        );
        crate::hotpath_observe::record_generation_verification(
            GenerationVerification::Reverified,
            canonical_bytes,
        );
    }

    /// Proves a generation durably before its relational head can advance.
    ///
    /// A sealed per-generation store is closed, reopened, and checked against
    /// `expected` before installation. When that proof is available, closing
    /// and reopening the accumulated staging database would prove the same
    /// rows a second time while checkpointing every older generation too.
    /// The staging database remains the WAL-backed replay and fallback
    /// authority; configurations without a sealed artifact retain the
    /// original close/reopen proof when `reopen_fallback` requires it.
    #[hotpath::measure(
        label = "graph_db.generation.publish.verify_proof",
        impl_type = "GraphDb"
    )]
    pub(crate) fn verify_generation_for_publication(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        row_counts: (usize, usize),
        reopen_fallback: bool,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        // Cheapest proof first: a marker filed by an earlier open of these
        // exact container bytes. `expected` still comes from the relational
        // authority, so a marker can only assert freshness - it can neither
        // name which generation is served nor make a wrong digest pass, and
        // corruption stays a typed failure from the full proof below. This is
        // the mount point for the verify-once path; without it the marker set
        // is written at close and never consulted, because publication routes
        // through here rather than through `verify_activated_generation`.
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        if let Some(canonical_bytes) = self.inner.markers.lookup(&locator, expected.as_str()) {
            check()?;
            let physical_namespace = identity.physical_namespace()?;
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            let commit = latest_projection(
                database,
                &physical_namespace,
                &identity.projection.projection,
            )?
            .ok_or_else(|| {
                GraphDbError::unavailable(
                    "verified graph generation has no complete native generation rows",
                )
            })?
            .commit;
            drop(guard);
            // Carry the inherited proof into this open's published set, so a
            // daemon that only serves reads does not drop it at close.
            self.inner.markers.record_fresh(&locator);
            crate::hotpath_observe::record_generation_verification(
                GenerationVerification::VerifiedFresh,
                canonical_bytes,
            );
            return Ok((commit, expected.clone()));
        }
        if let SealedStoreInstall::Installed { staging_proof } =
            self.ensure_sealed_generation_store(identity, expected, check)?
        {
            check()?;
            let physical_namespace = identity.physical_namespace()?;
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            let commit = latest_projection(
                database,
                &physical_namespace,
                &identity.projection.projection,
            )?
            .ok_or_else(|| {
                GraphDbError::unavailable(
                    "sealed graph publication has no complete native generation rows",
                )
            })?
            .commit;
            drop(guard);
            // A sealed *build* enumerated this container's rows and its
            // reopen digest matched the authority, which is the same proof
            // the staging close/reopen path files: record it so the close
            // writes the verify-once marker and the next open of these bytes
            // skips the full proof. An adopted artifact proved only itself,
            // so the container earns its marker on its next full proof.
            if let Some(canonical_bytes) = staging_proof {
                self.record_proven_generation(identity, expected, canonical_bytes);
            }
            return Ok((commit, expected.clone()));
        }
        if reopen_fallback {
            self.reopen_and_verify_existing_generation(identity, expected, row_counts, check)
        } else {
            self.verify_existing_generation(identity, expected, check)
        }
    }

    /// Stages one generation in bounded, durably receipted native pages.
    ///
    /// Entity pages precede relation pages, so every local endpoint exists
    /// before its edge is staged. A final empty batch binds dependency
    /// metadata only after every page receipt is present. None of these rows
    /// become serveable until the caller's recovered-digest proof and
    /// relational verified-head compare-and-swap succeed.
    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn apply_generation_unverified(
        &self,
        manifest: Arc<GraphGenerationManifest>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        let expected = manifest.expected_recovered_digest(check)?;
        self.apply_generation_unverified_with_digest(manifest, &expected, check)
    }

    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn apply_generation_unverified_with_digest(
        &self,
        manifest: Arc<GraphGenerationManifest>,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        self.apply_generation_unverified_with_digest_observed(manifest, expected, check)
            .map(GenerationStageOutcome::commit)
    }

    /// Stages `manifest`, consuming it.
    ///
    /// The manifest is taken by value: once the last page transaction and its
    /// receipt are durable the staged rows are recoverable from the database
    /// alone, so this releases them before the empty finalization batch runs.
    /// On a first index that reclaims gigabytes at exactly the point where the
    /// caller is about to close, reopen, and rebuild the in-RAM store -- the
    /// overlap that made publication the peak-RSS moment. Every later stage
    /// reads only the identity, which the caller keeps.
    #[hotpath::measure(label = "graph_db.generation.stage", impl_type = "GraphDb")]
    pub(crate) fn apply_generation_unverified_with_digest_observed(
        &self,
        manifest: Arc<GraphGenerationManifest>,
        expected: &GraphRecoveredGenerationDigestV1,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GenerationStageOutcome, GraphDbError> {
        check()?;
        manifest.validate_checked(check)?;
        let identity = manifest.identity();
        let context = GenerationStageContext {
            locator: GenerationLocator::new(
                identity.projection.clone(),
                identity.generation.clone(),
            ),
            physical_namespace: identity.physical_namespace()?,
            dependency_namespaces: self.require_exact_dependencies(&identity)?,
            dependency_digest: identity.dependency_closure_digest(check)?,
        };
        if let Some(commit) = self.reseat_complete_staged_generation(&identity, &context)? {
            // A complete durable generation is already seated; these rows were
            // never needed.
            drop(manifest);
            return Ok(GenerationStageOutcome::Reseated(commit));
        }
        let pages = generation_stage_pages(&manifest)?;
        let adopt_legacy_partial = self.has_exact_legacy_stage_prefix(
            &manifest,
            &identity,
            expected,
            &context,
            pages.first(),
        )?;
        #[cfg(feature = "hotpath")]
        {
            let generation_bytes = pages.iter().map(GenerationStagePage::live_bytes).sum();
            let (entities, relations) = manifest.row_counts();
            crate::hotpath_observe::record_counts(entities, relations, 0, generation_bytes);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Staged,
            );
        }
        match Arc::try_unwrap(manifest) {
            Ok(mut manifest) => {
                let entities = std::mem::take(&mut manifest.entities);
                let relations = std::mem::take(&mut manifest.relations);
                // Drop metadata and digest memo before staging. The separately
                // owned identity carries every value later phases need.
                drop(manifest);
                self.stage_owned_generation_pages(
                    entities,
                    relations,
                    &identity,
                    expected,
                    &context,
                    &pages,
                    adopt_legacy_partial,
                    check,
                )?;
            }
            Err(manifest) => {
                self.stage_generation_pages(
                    &manifest,
                    &identity,
                    expected,
                    &context,
                    &pages,
                    adopt_legacy_partial,
                    check,
                )?;
                // Shared supplied manifests cannot donate their rows, but the
                // staging owner still releases its Arc at the exact boundary.
                drop(manifest);
            }
        }
        self.finalize_staged_generation(&identity, expected, &context, pages.last(), check)
            .map(GenerationStageOutcome::Applied)
    }

    fn reseat_complete_staged_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
        context: &GenerationStageContext,
    ) -> Result<Option<GraphCommit>, GraphDbError> {
        let _snapshot_gate = self.wait_snapshot_gate_upgradable();
        let existing = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            latest_projection(
                database,
                &context.physical_namespace,
                &identity.projection.projection,
            )?
        };
        let Some(existing) = existing else {
            return Ok(None);
        };
        if existing.commit.source_generation != identity.source_generation
            || existing.commit.watermark != identity.watermark
            || existing.commit.generation_dependency_digest.as_ref()
                != Some(&context.dependency_digest)
        {
            return Ok(None);
        }
        let mut verified = self.wait_verified_generations_write()?;
        verified.collected.remove(&context.locator);
        verified.stored.insert(
            context.locator.clone(),
            generation_dependency_locators(identity),
        );
        Ok(Some(existing.commit))
    }

    /// Constructs page N+1 while page N holds the exclusive apply gate.
    /// Receipted pages skip construct so wedge-retry replay stays a peek.
    #[hotpath::measure(label = "graph_db.generation.page_pipeline", impl_type = "GraphDb")]
    fn stage_generation_pages(
        &self,
        manifest: &GraphGenerationManifest,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        pages: &[GenerationStagePage],
        adopt_legacy_partial: bool,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let mut prepared_next = None;
        for (index, page) in pages.iter().enumerate() {
            check()?;
            let first_page_blocked = index == 0
                && self.generation_stage_first_page_blocked_without_legacy(
                    identity,
                    context,
                    adopt_legacy_partial,
                )?;
            let current = if first_page_blocked {
                None
            } else {
                match prepared_next.take() {
                    Some(prepared) => Some(prepared),
                    None => self.construct_generation_stage_page_if_needed(
                        manifest, identity, expected, context, page, check,
                    )?,
                }
            };
            let successor = pages.get(index + 1);
            let successor_needs_construct = match successor {
                Some(next_page) if !first_page_blocked => !self
                    .generation_stage_page_already_applied(
                        identity, expected, context, next_page,
                    )?,
                Some(_) | None => false,
            };
            prepared_next = thread::scope(|scope| {
                let successor_handle =
                    successor
                        .filter(|_| successor_needs_construct)
                        .map(|next_page| {
                            scope.spawn(|| {
                                construct_generation_stage_page(
                                    manifest,
                                    identity,
                                    context,
                                    next_page,
                                    &|| Ok(()),
                                )
                            })
                        });
                self.apply_prepared_generation_stage_page(
                    Some(manifest),
                    identity,
                    expected,
                    context,
                    index.checked_sub(1).and_then(|prior| pages.get(prior)),
                    page,
                    adopt_legacy_partial && index == 0,
                    current,
                    check,
                )?;
                // This is the exact cancellation boundary: the page transaction
                // and receipt are durable, while no verified lease/head exists.
                check()?;
                successor_handle
                    .map(|handle| {
                        handle
                            .join()
                            .unwrap_or_else(|payload| panic::resume_unwind(payload))
                    })
                    .transpose()
            })?;
        }
        Ok(())
    }

    /// The sole-owned manifest path moves rows into each native page instead
    /// of cloning them. Only the current prepared batch and one bounded
    /// lookahead batch own row payloads; all later rows remain in the source
    /// iterators and every committed page drops before its successor applies.
    #[hotpath::measure(
        label = "graph_db.generation.page_pipeline_owned",
        impl_type = "GraphDb"
    )]
    #[allow(clippy::too_many_arguments)]
    fn stage_owned_generation_pages(
        &self,
        entities: Vec<crate::GraphEntity>,
        relations: Vec<GraphGenerationRelation>,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        pages: &[GenerationStagePage],
        adopt_legacy_partial: bool,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let mut rows = OwnedGenerationStageRows::new(entities, relations);
        let Some(first_page) = pages.first() else {
            return rows.finish();
        };
        let first_page_blocked = self.generation_stage_first_page_blocked_without_legacy(
            identity,
            context,
            adopt_legacy_partial,
        )?;
        let first_rows = rows.take_page(first_page)?;
        let mut current = if first_page_blocked
            || self
                .generation_stage_page_already_applied(identity, expected, context, first_page)?
        {
            drop(first_rows);
            None
        } else {
            Some(construct_owned_generation_stage_page(
                identity, context, first_page, first_rows, check,
            )?)
        };

        for (index, page) in pages.iter().enumerate() {
            check()?;
            let successor = pages.get(index + 1);
            let successor_rows = successor
                .map(|next_page| rows.take_page(next_page))
                .transpose()?;
            let successor_needs_construct = match successor {
                Some(next_page) if !first_page_blocked => !self
                    .generation_stage_page_already_applied(
                        identity, expected, context, next_page,
                    )?,
                Some(_) | None => false,
            };
            current = thread::scope(|scope| {
                let successor_handle = match (successor, successor_rows) {
                    (Some(next_page), Some(next_rows)) if successor_needs_construct => {
                        Some(scope.spawn(move || {
                            construct_owned_generation_stage_page(
                                identity,
                                context,
                                next_page,
                                next_rows,
                                &|| Ok(()),
                            )
                        }))
                    }
                    (Some(_), Some(next_rows)) => {
                        drop(next_rows);
                        None
                    }
                    (None, None) => None,
                    (Some(_), None) | (None, Some(_)) => {
                        return Err(GraphDbError::conflict(
                            "generation_runtime.stage_generation_pages_owned",
                        ));
                    }
                };
                self.apply_prepared_generation_stage_page(
                    None,
                    identity,
                    expected,
                    context,
                    index.checked_sub(1).and_then(|prior| pages.get(prior)),
                    page,
                    adopt_legacy_partial && index == 0,
                    current.take(),
                    check,
                )?;
                check()?;
                successor_handle
                    .map(|handle| {
                        handle
                            .join()
                            .unwrap_or_else(|payload| panic::resume_unwind(payload))
                    })
                    .transpose()
            })?;
        }
        rows.finish()
    }

    fn construct_generation_stage_page_if_needed(
        &self,
        manifest: &GraphGenerationManifest,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        page: &GenerationStagePage,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<Option<PreparedGenerationStagePage>, GraphDbError> {
        if self.generation_stage_page_already_applied(identity, expected, context, page)? {
            return Ok(None);
        }
        construct_generation_stage_page(manifest, identity, context, page, check).map(Some)
    }

    fn generation_stage_first_page_blocked_without_legacy(
        &self,
        identity: &GraphGenerationManifestIdentity,
        context: &GenerationStageContext,
        adopt_legacy_partial: bool,
    ) -> Result<bool, GraphDbError> {
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let Some(existing) = latest_projection(
            database,
            &context.physical_namespace,
            &identity.projection.projection,
        )?
        else {
            return Ok(false);
        };
        let exact_incomplete_legacy = adopt_legacy_partial
            && existing.commit.source_generation == identity.source_generation
            && existing.commit.watermark == identity.watermark
            && existing.commit.generation_dependency_digest.is_none();
        Ok(!exact_incomplete_legacy)
    }

    fn generation_stage_page_already_applied(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        page: &GenerationStagePage,
    ) -> Result<bool, GraphDbError> {
        let (idempotency_key, input_digest) =
            generation_stage_page_receipt(identity, expected, page)?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        Ok(
            match crate::state::publication(
                database,
                &context.physical_namespace,
                &idempotency_key,
            )? {
                Some(existing)
                    if existing.input_digest == input_digest
                        && existing.commit.source_generation == identity.source_generation
                        && existing.commit.watermark == identity.watermark =>
                {
                    true
                }
                Some(_) | None => false,
            },
        )
    }

    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(label = "graph_db.generation.page_apply", impl_type = "GraphDb")]
    fn apply_generation_stage_page_with_context(
        &self,
        manifest: &GraphGenerationManifest,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        predecessor: Option<&GenerationStagePage>,
        page: &GenerationStagePage,
        adopt_legacy_partial: bool,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        self.apply_prepared_generation_stage_page(
            Some(manifest),
            identity,
            expected,
            context,
            predecessor,
            page,
            adopt_legacy_partial,
            None,
            check,
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[hotpath::measure(
        label = "graph_db.generation.page_apply_prepared",
        impl_type = "GraphDb"
    )]
    fn apply_prepared_generation_stage_page(
        &self,
        manifest: Option<&GraphGenerationManifest>,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        predecessor: Option<&GenerationStagePage>,
        page: &GenerationStagePage,
        adopt_legacy_partial: bool,
        prepared: Option<PreparedGenerationStagePage>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        hotpath::gauge!("graph_db.generation.page_apply.bytes").set(page.live_bytes() as f64);
        let (idempotency_key, input_digest) =
            generation_stage_page_receipt(identity, expected, page)?;
        self.run_gated_batch(
            check,
            |database| {
                if let Some(existing) = crate::state::publication(
                    database,
                    &context.physical_namespace,
                    &idempotency_key,
                )? {
                    if existing.input_digest == input_digest
                        && existing.commit.source_generation == identity.source_generation
                        && existing.commit.watermark == identity.watermark
                    {
                        return Ok(GraphBatchPlan::Settled(existing.commit, ()));
                    }
                    return Err(self.sealed_write_refusal(&context.locator).unwrap_or(
                        GraphDbError::conflict(
                            "generation_runtime.apply_generation_stage_page_with_context",
                        ),
                    ));
                }
                if let Some(predecessor) = predecessor {
                    let (prior_key, prior_input) =
                        generation_stage_page_receipt(identity, expected, predecessor)?;
                    let prior = crate::state::publication(
                        database,
                        &context.physical_namespace,
                        &prior_key,
                    )?
                    .ok_or_else(|| {
                        GraphDbError::unavailable(
                            "graph generation stage predecessor is not applied",
                        )
                    })?;
                    if prior.input_digest != prior_input {
                        return Err(GraphDbError::conflict(
                            "generation_runtime.apply_generation_stage_page_with_context",
                        ));
                    }
                } else if let Some(existing) = latest_projection(
                    database,
                    &context.physical_namespace,
                    &identity.projection.projection,
                )? {
                    // A finalized generation always carries its dependency
                    // digest. Only an exact unfinished legacy stage may let
                    // the wider first page replace its old prefix.
                    let exact_incomplete_legacy = adopt_legacy_partial
                        && existing.commit.source_generation == identity.source_generation
                        && existing.commit.watermark == identity.watermark
                        && existing.commit.generation_dependency_digest.is_none();
                    if !exact_incomplete_legacy {
                        return Err(self.sealed_write_refusal(&context.locator).unwrap_or(
                            GraphDbError::conflict(
                                "generation_runtime.apply_generation_stage_page_with_context",
                            ),
                        ));
                    }
                }
                let (batch, endpoint_namespaces, digest) = match prepared {
                    Some(prepared) => (
                        prepared.batch,
                        prepared.endpoint_namespaces,
                        prepared.digest,
                    ),
                    None => {
                        let manifest = manifest.ok_or_else(|| {
                            GraphDbError::unavailable(
                                "owned generation stage page was not prepared",
                            )
                        })?;
                        let (batch, endpoint_namespaces) = prepare_generation_stage_batch(
                            manifest, identity, context, page, check,
                        )?;
                        let digest = batch.canonical_digest_checked(check)?;
                        (batch, endpoint_namespaces, digest)
                    }
                };
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata {
                            digest: digest.clone(),
                            generation_dependency_digest: None,
                            publication_record: Some((
                                idempotency_key.clone(),
                                digest,
                                input_digest.clone(),
                            )),
                        },
                        endpoint_namespaces,
                        ensure_page_vector_indexes: true,
                    },
                    (),
                ))
            },
            |_database, commit, ()| Ok(commit),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn has_exact_legacy_stage_prefix(
        &self,
        manifest: &GraphGenerationManifest,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        native_first: Option<&GenerationStagePage>,
    ) -> Result<bool, GraphDbError> {
        let legacy_first = first_generation_stage_page_with_limits(
            manifest,
            MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
            MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
        )?;
        let Some(legacy_first) = legacy_first.as_ref() else {
            return Ok(false);
        };
        if native_first == Some(legacy_first) {
            return Ok(false);
        }
        // The legacy receipt binds the exact manifest identity, recovered
        // digest, page range, and live-byte count. Its presence is the durable
        // proof that replacing the obsolete prefix does not adopt foreign rows.
        let (legacy_key, legacy_input) =
            generation_stage_page_receipt(identity, expected, legacy_first)?;
        let guard = self.read_guard()?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let Some(existing) =
            crate::state::publication(database, &context.physical_namespace, &legacy_key)?
        else {
            return Ok(false);
        };
        if existing.input_digest != legacy_input
            || existing.commit.source_generation != identity.source_generation
            || existing.commit.watermark != identity.watermark
        {
            return Err(GraphDbError::conflict(
                "generation_runtime.has_exact_legacy_stage_prefix",
            ));
        }
        Ok(true)
    }

    /// Binds the dependency metadata in one empty batch, after every page
    /// receipt is durable. Reads only the identity, so the staged rows are
    /// already released by the time this runs.
    #[hotpath::measure(label = "graph_db.generation.finalize", impl_type = "GraphDb")]
    fn finalize_staged_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        context: &GenerationStageContext,
        last_page: Option<&GenerationStagePage>,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        let (idempotency_key, input_digest) =
            generation_stage_finalization_receipt(identity, expected)?;
        self.run_gated_batch(
            check,
            |database| {
                if let Some(existing) = crate::state::publication(
                    database,
                    &context.physical_namespace,
                    &idempotency_key,
                )? {
                    if existing.input_digest == input_digest
                        && existing.commit.source_generation == identity.source_generation
                        && existing.commit.watermark == identity.watermark
                        && existing.commit.generation_dependency_digest.as_ref()
                            == Some(&context.dependency_digest)
                    {
                        return Ok(GraphBatchPlan::Settled(existing.commit, ()));
                    }
                    return Err(self.sealed_write_refusal(&context.locator).unwrap_or(
                        GraphDbError::conflict("generation_runtime.finalize_staged_generation"),
                    ));
                }
                if let Some(last_page) = last_page {
                    let (last_key, last_input) =
                        generation_stage_page_receipt(identity, expected, last_page)?;
                    let last = crate::state::publication(
                        database,
                        &context.physical_namespace,
                        &last_key,
                    )?
                    .ok_or_else(|| {
                        GraphDbError::unavailable("graph generation final page is not applied")
                    })?;
                    if last.input_digest != last_input {
                        return Err(GraphDbError::conflict(
                            "generation_runtime.finalize_staged_generation",
                        ));
                    }
                }
                let batch = GraphWriteBatch::new_canonical_checked(
                    context.physical_namespace.clone(),
                    identity.projection.projection.clone(),
                    identity.source_generation.clone(),
                    identity.watermark.clone(),
                    Vec::new(),
                    check,
                )?;
                let digest = batch.canonical_digest_checked(check)?;
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata {
                            digest: digest.clone(),
                            generation_dependency_digest: Some(context.dependency_digest.clone()),
                            publication_record: Some((
                                idempotency_key.clone(),
                                digest,
                                input_digest.clone(),
                            )),
                        },
                        endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
                        ensure_page_vector_indexes: false,
                    },
                    (),
                ))
            },
            |_database, commit, ()| {
                let mut verified = self.wait_verified_generations_write()?;
                verified.collected.remove(&context.locator);
                verified.stored.insert(
                    context.locator.clone(),
                    generation_dependency_locators(identity),
                );
                Ok(commit)
            },
        )
    }

    /// Closes, reopens, and proves the persisted generation against
    /// `expected`.
    ///
    /// Takes only the identity: the proof streams rows out of the reopened
    /// database, so this no longer overlaps the in-memory manifest rows with
    /// the reopen's file bytes, decoded snapshot, and rebuilt store.
    /// `row_counts` carries the manifest's `(entities, relations)` lengths for
    /// observability alone, so the rows themselves need not be kept alive.
    #[hotpath::measure(label = "graph_db.generation.reopen", impl_type = "GraphDb")]
    pub(crate) fn reopen_and_verify_existing_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
        expected: &GraphRecoveredGenerationDigestV1,
        row_counts: (usize, usize),
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, GraphRecoveredGenerationDigestV1), GraphDbError> {
        check()?;
        let physical_namespace = identity.physical_namespace()?;
        let projection = identity.projection.projection.clone();
        let quarantine_key = (physical_namespace.clone(), projection.clone());
        let reopen = self.inner.reopen.clone().ok_or_else(|| {
            GraphDbError::invalid(
                "recovered generation verification requires a persistent graph database",
            )
        })?;
        // The exclusive snapshot-gate claim covers only the physical
        // close/reopen swap. The recovered-digest proof below is read-only,
        // so it runs behind an upgradable claim: snapshot readers proceed
        // while every writer still queues behind this guard, keeping the
        // reopened rows stable for the digest.
        let snapshot_gate = self.wait_snapshot_gate_write();
        {
            let mut database_guard = crate::hotpath_observe::wait_lock(
                crate::hotpath_observe::LOCK_WAIT_DATABASE_WRITE,
                || self.inner.database.write(),
            )
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
            self.inner.identity_indexes.invalidate();
            self.ensure_available()?;
            check()?;
            let mut state_guard = self.state_write_guard()?;
            let mut quarantined_guard = self
                .inner
                .quarantined_projections
                .write()
                .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
            let database = database_guard.take().ok_or(GraphDbError::Closed)?;
            if let Err(error) =
                hotpath::measure_block!("graph_db.generation.reopen.close", database.close())
            {
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message: format!(
                        "Grafeo close failed before recovered generation verification: {error}"
                    ),
                });
            }
            let (recovered, recovered_state, quarantined) = match open_recovered_database(&reopen) {
                Ok(recovered) => recovered,
                Err(error) => {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(error);
                }
            };
            *state_guard = Some(recovered_state);
            *quarantined_guard = quarantined;
            *database_guard = Some(recovered);
        }
        let snapshot_gate = ParkingRwLockWriteGuard::downgrade_to_upgradable(snapshot_gate);
        check()?;
        let commit = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            match latest_projection(database, &physical_namespace, &projection) {
                Ok(Some(existing)) => existing.commit,
                Ok(None) => {
                    let error = GraphDbError::GenerationMismatch {
                        namespace: identity.projection.namespace.to_string(),
                        projection: projection.to_string(),
                        generation: identity.generation.to_string(),
                        message: "recovered generation is missing".to_owned(),
                    };
                    drop(guard);
                    drop(snapshot_gate);
                    self.quarantine_generation(identity)?;
                    return Err(error);
                }
                Err(error) => {
                    if is_database_fault(&error) {
                        self.inner.poisoned.store(true, Ordering::Release);
                    }
                    return Err(error);
                }
            }
        };
        let verified = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            match verify_recovered_generation(database, identity, expected, check) {
                Ok((verified, canonical_bytes)) => {
                    self.record_proven_generation(identity, &verified, canonical_bytes);
                    verified
                }
                Err(error @ GraphDbError::GenerationMismatch { .. }) => {
                    drop(guard);
                    drop(snapshot_gate);
                    self.quarantine_generation(identity)?;
                    return Err(error);
                }
                Err(error) => {
                    if is_database_fault(&error) {
                        self.inner.poisoned.store(true, Ordering::Release);
                    }
                    return Err(error);
                }
            }
        };
        let was_quarantined = self
            .inner
            .quarantined_projections
            .read()
            .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?
            .contains(&quarantine_key);
        if !was_quarantined {
            return Ok((commit, verified));
        }
        // Quarantine repair: the exclusive claim covers only the durable
        // marker clear and the checkpoint transition (both rewrite the
        // database file). The re-verification afterwards is read-only again,
        // so the gate downgrades back to upgradable and snapshot readers are
        // admitted while the repaired rows stream through the proof.
        let write_gate = self.wait_snapshot_gate_upgrade(snapshot_gate);
        {
            let mut database_guard = crate::hotpath_observe::wait_lock(
                crate::hotpath_observe::LOCK_WAIT_DATABASE_WRITE,
                || self.inner.database.write(),
            )
            .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
            self.inner.identity_indexes.invalidate();
            let mut state_guard = self.state_write_guard()?;
            let mut quarantined_guard = self
                .inner
                .quarantined_projections
                .write()
                .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
            {
                let database = database_guard.as_ref().ok_or(GraphDbError::Closed)?;
                if let Err(error) =
                    set_projection_quarantine(database, &physical_namespace, &projection, false)
                        .and_then(|()| crate::runtime::sync_wal(database))
                {
                    self.inner.poisoned.store(true, Ordering::Release);
                    return Err(quarantine_transition_failure(
                        "clear recovered generation quarantine",
                        error,
                    ));
                }
            }
            let database = database_guard.take().ok_or(GraphDbError::Closed)?;
            let (recovered, recovered_state, quarantined) =
                match checkpoint_recovered_database(database, &reopen) {
                    Ok(recovered) => recovered,
                    Err(error) => {
                        self.inner.poisoned.store(true, Ordering::Release);
                        return Err(error);
                    }
                };
            let still_quarantined = quarantined.contains(&quarantine_key);
            *state_guard = Some(recovered_state);
            *quarantined_guard = quarantined;
            *database_guard = Some(recovered);
            if still_quarantined {
                self.inner.poisoned.store(true, Ordering::Release);
                return Err(GraphDbError::DurabilityUncertain {
                    message: "recovered generation quarantine remained after checkpoint".to_owned(),
                });
            }
        }
        let snapshot_gate = ParkingRwLockWriteGuard::downgrade_to_upgradable(write_gate);
        let verify_result = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            verify_recovered_generation(database, identity, expected, check)
        };
        match verify_result {
            Ok((verified, canonical_bytes)) => {
                self.record_proven_generation(identity, &verified, canonical_bytes);
                let (entities, relations) = row_counts;
                crate::hotpath_observe::record_counts(entities, relations, 0, 0);
                crate::hotpath_observe::record_hydration_source(
                    crate::hotpath_observe::HydrationSource::Recovered,
                );
                Ok((commit, verified))
            }
            Err(error) => {
                // Restoring the durable quarantine marker rewrites the file,
                // so the failure path re-takes the exclusive claim.
                let _write_gate = self.wait_snapshot_gate_upgrade(snapshot_gate);
                let mut database_guard = crate::hotpath_observe::wait_lock(
                    crate::hotpath_observe::LOCK_WAIT_DATABASE_WRITE,
                    || self.inner.database.write(),
                )
                .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
                self.inner.identity_indexes.invalidate();
                let mut state_guard = self.state_write_guard()?;
                let mut quarantined_guard =
                    self.inner.quarantined_projections.write().map_err(|_| {
                        GraphDbError::unavailable("graph quarantine lock is poisoned")
                    })?;
                let database = database_guard.take().ok_or(GraphDbError::Closed)?;
                let (recovered, recovered_state, quarantined) =
                    match requarantine_after_failed_checkpoint_verification(
                        database,
                        &reopen,
                        &physical_namespace,
                        &projection,
                        &error,
                    ) {
                        Ok(recovered) => recovered,
                        Err(restore_error) => {
                            self.inner.poisoned.store(true, Ordering::Release);
                            return Err(restore_error);
                        }
                    };
                if is_database_fault(&error) {
                    self.inner.poisoned.store(true, Ordering::Release);
                }
                *state_guard = Some(recovered_state);
                *quarantined_guard = quarantined;
                *database_guard = Some(recovered);
                Err(error)
            }
        }
    }

    pub(crate) fn install_verified_generation(
        &self,
        lease: std::sync::Arc<VerifiedGenerationLease>,
    ) -> Result<Option<std::sync::Arc<VerifiedGenerationLease>>, GraphDbError> {
        let mut state = self.wait_verified_generations_write()?;
        state.install(lease)
    }

    pub(crate) fn remember_verified_generation(
        &self,
        lease: &std::sync::Arc<VerifiedGenerationLease>,
    ) -> Result<(), GraphDbError> {
        let mut state = self.wait_verified_generations_write()?;
        state.remember(lease)
    }

    #[hotpath::measure(label = "graph_db.generation.delete", impl_type = "GraphDb")]
    pub(crate) fn delete_generation_contents(
        &self,
        locator: &GenerationLocator,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        check()?;
        let namespace = locator.physical_namespace()?;
        let commit = {
            let guard = self.read_guard()?;
            let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
            latest_projection(database, &namespace, &locator.projection.projection)?
                .map(|projection| projection.commit)
        };
        let Some(commit) = commit else {
            let mut state = self.wait_verified_generations_write()?;
            state.known.remove(locator);
            state.quarantined.remove(locator);
            state.stored.remove(locator);
            state.retiring.remove(locator);
            state.collected.insert(locator.clone());
            drop(state);
            self.retire_sealed_generation_store(locator);
            return Ok(());
        };
        self.delete_projection_checked(
            namespace,
            locator.projection.projection.clone(),
            commit.source_generation,
            commit.watermark,
            check,
        )?;
        let mut state = self.wait_verified_generations_write()?;
        state.known.remove(locator);
        state.quarantined.remove(locator);
        state.stored.remove(locator);
        state.retiring.remove(locator);
        state.collected.insert(locator.clone());
        drop(state);
        self.retire_sealed_generation_store(locator);
        Ok(())
    }

    fn delete_projection_checked(
        &self,
        namespace: GraphNamespace,
        projection: crate::GraphProjectionId,
        source_generation: crate::SourceGeneration,
        watermark: crate::GraphWatermark,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphCommit, GraphDbError> {
        check()?;
        let mut last_commit = None;
        for kind in [
            GenerationRetirementPageKind::Relations,
            GenerationRetirementPageKind::Entities,
        ] {
            loop {
                let (commit, applied) = self.delete_projection_page_checked(
                    &namespace,
                    &projection,
                    &source_generation,
                    &watermark,
                    kind,
                    check,
                )?;
                last_commit = Some(commit);
                if !applied {
                    break;
                }
                // A committed page stays durable. Cancellation at this exact
                // boundary leaves the remaining rows for an idempotent retry.
                check()?;
            }
        }
        last_commit.ok_or_else(|| GraphDbError::unavailable("graph projection disappeared"))
    }

    #[allow(clippy::too_many_arguments)]
    fn delete_projection_page_checked(
        &self,
        namespace: &GraphNamespace,
        projection: &crate::GraphProjectionId,
        source_generation: &crate::SourceGeneration,
        watermark: &crate::GraphWatermark,
        kind: GenerationRetirementPageKind,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<(GraphCommit, bool), GraphDbError> {
        self.run_gated_batch(
            check,
            |database| {
                let mutations = match kind {
                    GenerationRetirementPageKind::Relations => {
                        projection_relation_deletion_page_checked(
                            database, namespace, projection, check,
                        )?
                    }
                    GenerationRetirementPageKind::Entities => {
                        projection_entity_deletion_page_checked(
                            database, namespace, projection, check,
                        )?
                    }
                };
                if mutations.is_empty() {
                    let commit = latest_projection(database, namespace, projection)?
                        .ok_or_else(|| GraphDbError::unavailable("graph projection disappeared"))?
                        .commit;
                    return Ok(GraphBatchPlan::Settled(commit, false));
                }
                let batch = GraphWriteBatch::new_canonical_checked(
                    namespace.clone(),
                    projection.clone(),
                    source_generation.clone(),
                    watermark.clone(),
                    mutations,
                    check,
                )?;
                let digest = batch.canonical_digest_checked(check)?;
                Ok(GraphBatchPlan::Apply(
                    PreparedGraphBatch {
                        batch,
                        metadata: mutation::CommitMetadata::for_digest(digest),
                        endpoint_namespaces: mutation::RelationEndpointNamespaces::new(),
                        ensure_page_vector_indexes: false,
                    },
                    true,
                ))
            },
            |_database, commit, applied| Ok((commit, applied)),
        )
    }

    #[hotpath::measure(label = "graph_db.read.generation_relation", impl_type = "GraphDb")]
    pub(crate) fn generation_relation(
        &self,
        snapshot: &VerifiedGraphSnapshot,
        reference: &GraphRelationRef,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphGenerationRelation>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let namespace_projection = snapshot.namespace_projection_map()?;
        let guard = self.read_database(cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let relation_lease = snapshot.lease_for_projection(&reference.projection)?;
        let relation_namespace = relation_lease.locator.physical_namespace()?;
        let Some(stored) = load_relation(database, &relation_namespace, &reference.identity)?
        else {
            return Ok(None);
        };
        let mut endpoints = EndpointIdentityCache::default();
        let from = typed_entity_ref(
            database,
            stored.source,
            &namespace_projection,
            &mut endpoints,
        )?;
        let to = typed_entity_ref(
            database,
            stored.target,
            &namespace_projection,
            &mut endpoints,
        )?;
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        GraphGenerationRelation::new(
            stored.relation.identity,
            from,
            to,
            stored.relation.kind,
            stored.relation.properties,
        )
        .map(Some)
    }

    #[hotpath::measure(label = "graph_db.traversal.verified", impl_type = "GraphDb")]
    pub(crate) fn traverse_generation(
        &self,
        snapshot: &VerifiedGraphSnapshot,
        request: TraversalRequest,
    ) -> Result<VerifiedTraversalResult, GraphDbError> {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if request.max_visits == 0 {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Read,
                request.max_visits,
            ));
        }
        if request.max_results == 0 {
            return Ok(VerifiedTraversalResult { visits: Vec::new() });
        }
        let namespace_projection = snapshot.namespace_projection_map()?;
        let guard = self.read_database(request.cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let head_namespace = crate::generation::physical_namespace(
            &snapshot.projection().namespace,
            &snapshot.projection().projection,
            snapshot.generation(),
        )?;
        let start = crate::state::load_entity(database, &head_namespace, &request.start)?
            .ok_or_else(|| GraphDbError::invalid("traversal start entity does not exist"))?;
        let store = database.graph_store();
        let mut endpoints = EndpointIdentityCache::default();
        let mut queue = VecDeque::from([(start.node, 0_usize, None)]);
        let mut discovered = HashSet::from([start.node]);
        let mut visits = Vec::new();
        while let Some((node, depth, via_relation)) = queue.pop_front() {
            if request.cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if visits.len() >= request.max_visits {
                return Err(GraphDbError::budget_exhausted_count(
                    GraphBudgetKind::Read,
                    request.max_visits,
                ));
            }
            visits.push(VerifiedTraversalVisit {
                entity: typed_entity_ref(database, node, &namespace_projection, &mut endpoints)?,
                depth,
                via_relation,
            });
            if visits.len() >= request.max_results || depth >= request.max_depth {
                continue;
            }
            let directions: &[Direction] = match request.direction {
                GraphTraversalDirection::Outgoing => &[Direction::Outgoing],
                GraphTraversalDirection::Incoming => &[Direction::Incoming],
                GraphTraversalDirection::Both => &[Direction::Outgoing, Direction::Incoming],
            };
            let mut adjacent = Vec::new();
            for direction in directions {
                for (neighbor, edge_id) in store.edges_from(node, *direction) {
                    if request.cancellation.is_cancelled() {
                        return Err(GraphDbError::Cancelled);
                    }
                    let edge = store
                        .get_edge(edge_id)
                        .ok_or_else(|| GraphDbError::Corrupt {
                            message: "verified traversal relation edge is missing".to_owned(),
                        })?;
                    let kind = relation_kind_from_type(edge.edge_type.as_str())?;
                    if !request.relation_kinds.is_empty() && !request.relation_kinds.contains(&kind)
                    {
                        continue;
                    }
                    let relation_namespace = GraphNamespace::new(required_string(
                        edge.get_property(NAMESPACE_PROPERTY),
                        "verified traversal relation namespace",
                    )?)
                    .map_err(|error| GraphDbError::Corrupt {
                        message: format!(
                            "verified traversal relation namespace is invalid: {error}"
                        ),
                    })?;
                    let Some(relation_projection) =
                        namespace_projection.get(&relation_namespace).cloned()
                    else {
                        continue;
                    };
                    let stored = load_relation_by_edge_cached(database, edge_id, &mut endpoints)?
                        .ok_or_else(|| GraphDbError::Corrupt {
                        message: "verified traversal edge has no typed relation locator".to_owned(),
                    })?;
                    let (neighbor_namespace, neighbor_identity) =
                        endpoints.identity(database.graph_store().as_ref(), neighbor)?;
                    let Some(entity_projection) =
                        namespace_projection.get(&neighbor_namespace).cloned()
                    else {
                        continue;
                    };
                    let entity = GraphEntityRef::new(entity_projection, neighbor_identity);
                    adjacent.push((
                        GraphRelationRef::new(relation_projection, stored.relation.identity),
                        entity,
                        neighbor,
                    ));
                }
            }
            adjacent.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
            adjacent.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
            for (relation, _, neighbor) in adjacent {
                if discovered.insert(neighbor) {
                    let next_depth = depth.checked_add(1).ok_or_else(|| {
                        GraphDbError::budget_exhausted_count(
                            GraphBudgetKind::Read,
                            request.max_depth,
                        )
                    })?;
                    queue.push_back((neighbor, next_depth, Some(relation)));
                }
            }
        }
        #[cfg(feature = "hotpath")]
        {
            let edges = visits
                .iter()
                .filter(|visit| visit.via_relation.is_some())
                .count();
            crate::hotpath_observe::record_counts(visits.len(), edges, 0, 0);
            crate::hotpath_observe::record_hydration_source(
                crate::hotpath_observe::HydrationSource::Snapshot,
            );
        }
        Ok(VerifiedTraversalResult { visits })
    }

    pub(crate) fn verified_generation(
        &self,
        locator: &GenerationLocator,
    ) -> Result<Option<std::sync::Arc<VerifiedGenerationLease>>, GraphDbError> {
        let state = self.inner.verified_generations.read().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        if state.quarantined.contains(locator) {
            return Err(GraphDbError::GenerationMismatch {
                namespace: locator.projection.namespace.to_string(),
                projection: locator.projection.projection.to_string(),
                generation: locator.generation.to_string(),
                message: "generation remains quarantined after recovery mismatch".to_owned(),
            });
        }
        if state.retiring.contains(locator) || state.collected.contains(locator) {
            return Err(GraphDbError::conflict(
                "generation_runtime.verified_generation",
            ));
        }
        if let Some(installed) = state.heads.get(&locator.projection)
            && installed.locator == *locator
        {
            return Ok(Some(std::sync::Arc::clone(installed)));
        }
        Ok(state.known.get(locator).and_then(std::sync::Weak::upgrade))
    }

    /// The verified-generation cache as seen by a publisher that can re-prove
    /// the generation from its canonical replay inputs.
    ///
    /// A quarantined locator answers `None` instead of the typed refusal:
    /// the quarantine records that the stored rows failed their last digest
    /// proof, which is exactly the state a full republication heals — the
    /// publish path re-projects the rows, re-proves the recovered digest,
    /// and `remember`/`install` clear the marker with the fresh proof.
    /// Read-side recovery keeps the strict [`Self::verified_generation`]
    /// refusal because it can only re-run the proof over the same rows.
    pub(crate) fn republishable_verified_generation(
        &self,
        locator: &GenerationLocator,
    ) -> Result<Option<std::sync::Arc<VerifiedGenerationLease>>, GraphDbError> {
        {
            let state = self.inner.verified_generations.read().map_err(|_| {
                GraphDbError::unavailable("verified graph generation state lock is poisoned")
            })?;
            if state.quarantined.contains(locator) {
                return Ok(None);
            }
        }
        self.verified_generation(locator)
    }

    fn require_exact_dependencies(
        &self,
        identity: &GraphGenerationManifestIdentity,
    ) -> Result<BTreeMap<crate::GraphProjectionIdentity, GraphNamespace>, GraphDbError> {
        let state = self.inner.verified_generations.read().map_err(|_| {
            GraphDbError::unavailable("verified graph generation state lock is poisoned")
        })?;
        let mut namespaces = BTreeMap::new();
        for dependency in &identity.dependencies {
            let locator = GenerationLocator::new(
                dependency.projection.clone(),
                dependency.generation.clone(),
            );
            if state.retiring.contains(&locator) || state.collected.contains(&locator) {
                return Err(GraphDbError::conflict(
                    "generation_runtime.require_exact_dependencies",
                ));
            }
            let Some(head) = state.known.get(&locator).and_then(std::sync::Weak::upgrade) else {
                return Err(GraphDbError::conflict(
                    "generation_runtime.require_exact_dependencies",
                ));
            };
            namespaces.insert(
                dependency.projection.clone(),
                head.locator.physical_namespace()?,
            );
        }
        Ok(namespaces)
    }

    #[hotpath::measure(label = "graph_db.generation.quarantine", impl_type = "GraphDb")]
    pub(crate) fn quarantine_generation(
        &self,
        identity: &GraphGenerationManifestIdentity,
    ) -> Result<(), GraphDbError> {
        self.ensure_opened()?;
        let locator =
            GenerationLocator::new(identity.projection.clone(), identity.generation.clone());
        let physical_namespace = locator.physical_namespace()?;
        let _snapshot_gate = self.wait_snapshot_gate_write();
        let mut database_guard = crate::hotpath_observe::wait_lock(
            crate::hotpath_observe::LOCK_WAIT_DATABASE_WRITE,
            || self.inner.database.write(),
        )
        .map_err(|_| GraphDbError::unavailable("graph database write lock is poisoned"))?;
        self.inner.identity_indexes.invalidate();
        let mut format_state = self.state_write_guard()?;
        let mut projection_quarantine = self
            .inner
            .quarantined_projections
            .write()
            .map_err(|_| GraphDbError::unavailable("graph quarantine lock is poisoned"))?;
        let reopen = self.inner.reopen.clone().ok_or_else(|| {
            GraphDbError::invalid("generation quarantine requires a persistent graph database")
        })?;
        {
            let database = database_guard.as_ref().ok_or(GraphDbError::Closed)?;
            crate::recovery::set_projection_quarantine(
                database,
                &physical_namespace,
                &identity.projection.projection,
                true,
            )
            .and_then(|()| crate::runtime::sync_wal(database))
            .inspect_err(|_| {
                self.inner.poisoned.store(true, Ordering::Release);
            })?;
        }
        let database = database_guard.take().ok_or(GraphDbError::Closed)?;
        let (recovered, recovered_state, recovered_quarantine) =
            crate::recovery::checkpoint_recovered_database(database, &reopen).inspect_err(
                |_| {
                    self.inner.poisoned.store(true, Ordering::Release);
                },
            )?;
        if !recovered_quarantine.contains(&(
            physical_namespace.clone(),
            identity.projection.projection.clone(),
        )) {
            self.inner.poisoned.store(true, Ordering::Release);
            return Err(GraphDbError::DurabilityUncertain {
                message: "generation quarantine disappeared after durable checkpoint".to_owned(),
            });
        }
        *format_state = Some(recovered_state);
        *projection_quarantine = recovered_quarantine;
        *database_guard = Some(recovered);
        let mut state = self.wait_verified_generations_write()?;
        state.quarantine(locator.clone());
        drop(state);
        // A quarantined generation's rows are in doubt; its derived sealed
        // artifact must not outlive that doubt.
        self.retire_sealed_generation_store(&locator);
        Ok(())
    }
}

fn typed_entity_ref(
    database: &grafeo_engine::GrafeoDB,
    node: grafeo_common::types::NodeId,
    namespace_projection: &BTreeMap<GraphNamespace, crate::GraphProjectionIdentity>,
    cache: &mut EndpointIdentityCache,
) -> Result<GraphEntityRef, GraphDbError> {
    let (namespace, identity) = cache.identity(database.graph_store().as_ref(), node)?;
    let projection = namespace_projection
        .get(&namespace)
        .cloned()
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "verified graph entity escapes snapshot dependency closure".to_owned(),
        })?;
    Ok(GraphEntityRef::new(projection, identity))
}

fn generation_stage_pages(
    manifest: &GraphGenerationManifest,
) -> Result<Vec<GenerationStagePage>, GraphDbError> {
    generation_stage_pages_with_limits(
        manifest,
        MAX_NATIVE_GENERATION_STAGE_MUTATIONS,
        MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
    )
}

fn generation_stage_pages_with_limits(
    manifest: &GraphGenerationManifest,
    maximum_mutations: usize,
    maximum_live_bytes: usize,
) -> Result<Vec<GenerationStagePage>, GraphDbError> {
    let mut pages = Vec::new();
    append_generation_stage_pages_with_limits(
        &mut pages,
        GenerationStagePageKind::Entities,
        manifest.entities.len(),
        |index| generation_entity_live_bytes(&manifest.entities[index]),
        maximum_mutations,
        maximum_live_bytes,
    )?;
    append_generation_stage_pages_with_limits(
        &mut pages,
        GenerationStagePageKind::Relations,
        manifest.relations.len(),
        |index| generation_relation_live_bytes(&manifest.relations[index]),
        maximum_mutations,
        maximum_live_bytes,
    )?;
    Ok(pages)
}

fn first_generation_stage_page_with_limits(
    manifest: &GraphGenerationManifest,
    maximum_mutations: usize,
    maximum_live_bytes: usize,
) -> Result<Option<GenerationStagePage>, GraphDbError> {
    let mut pages = Vec::with_capacity(2);
    if manifest.entities.is_empty() {
        append_generation_stage_pages_with_limits(
            &mut pages,
            GenerationStagePageKind::Relations,
            manifest
                .relations
                .len()
                .min(maximum_mutations.saturating_add(1)),
            |index| generation_relation_live_bytes(&manifest.relations[index]),
            maximum_mutations,
            maximum_live_bytes,
        )?;
    } else {
        append_generation_stage_pages_with_limits(
            &mut pages,
            GenerationStagePageKind::Entities,
            manifest
                .entities
                .len()
                .min(maximum_mutations.saturating_add(1)),
            |index| generation_entity_live_bytes(&manifest.entities[index]),
            maximum_mutations,
            maximum_live_bytes,
        )?;
    }
    Ok(pages.into_iter().next())
}

fn generation_entity_live_bytes(entity: &crate::GraphEntity) -> Result<usize, GraphDbError> {
    entity
        .labels
        .iter()
        .try_fold(entity.identity.as_str().len(), |bytes, label| {
            bytes
                .checked_add(label.as_str().len())
                .ok_or_else(stage_live_bytes_exhausted)
        })?
        .checked_add(graph_properties_live_bytes(&entity.properties)?)
        .ok_or_else(stage_live_bytes_exhausted)
}

fn generation_relation_live_bytes(
    relation: &GraphGenerationRelation,
) -> Result<usize, GraphDbError> {
    [
        relation.identity.as_str().len(),
        relation.from.identity.as_str().len(),
        relation.from.projection.namespace.as_str().len(),
        relation.from.projection.projection.as_str().len(),
        relation.to.identity.as_str().len(),
        relation.to.projection.namespace.as_str().len(),
        relation.to.projection.projection.as_str().len(),
        relation.kind.as_str().len(),
        graph_properties_live_bytes(&relation.properties)?,
    ]
    .into_iter()
    .try_fold(0usize, |bytes, next| {
        bytes
            .checked_add(next)
            .ok_or_else(stage_live_bytes_exhausted)
    })
}

fn stage_live_bytes_exhausted() -> GraphDbError {
    GraphDbError::budget_exhausted_count(
        GraphBudgetKind::Write,
        MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
    )
}

fn append_generation_stage_pages_with_limits(
    pages: &mut Vec<GenerationStagePage>,
    kind: GenerationStagePageKind,
    count: usize,
    property_bytes: impl Fn(usize) -> Result<usize, GraphDbError>,
    maximum_mutations: usize,
    maximum_live_bytes: usize,
) -> Result<(), GraphDbError> {
    let mut start = 0usize;
    let mut live_bytes = 0usize;
    for index in 0..count {
        let next_bytes = property_bytes(index)?;
        if next_bytes > maximum_live_bytes {
            return Err(GraphDbError::budget_exhausted_count(
                GraphBudgetKind::Write,
                maximum_live_bytes,
            ));
        }
        let page_is_full = index - start == maximum_mutations;
        let bytes_would_overflow = live_bytes
            .checked_add(next_bytes)
            .is_none_or(|bytes| bytes > maximum_live_bytes);
        if index > start && (page_is_full || bytes_would_overflow) {
            pages.push(GenerationStagePage {
                ordinal: pages.len(),
                kind,
                range: start..index,
                live_bytes,
            });
            start = index;
            live_bytes = 0;
        }
        live_bytes = live_bytes.checked_add(next_bytes).ok_or_else(|| {
            GraphDbError::budget_exhausted_count(GraphBudgetKind::Write, maximum_live_bytes)
        })?;
    }
    if start < count {
        pages.push(GenerationStagePage {
            ordinal: pages.len(),
            kind,
            range: start..count,
            live_bytes,
        });
    }
    Ok(())
}

fn generation_stage_page_receipt(
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
    page: &GenerationStagePage,
) -> Result<(GraphIdempotencyKey, String), GraphDbError> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.graph-generation-native-page.v1",
        &identity.projection,
        &identity.generation,
        &identity.source_generation,
        &identity.watermark,
        expected.as_str(),
        page.ordinal,
        page.kind.as_str(),
        page.range.start,
        page.range.end,
        page.live_bytes,
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    Ok((
        GraphIdempotencyKey::new(format!("generation-page:{}", digest.as_str()))?,
        digest.as_str().to_owned(),
    ))
}

fn generation_stage_finalization_receipt(
    identity: &GraphGenerationManifestIdentity,
    expected: &GraphRecoveredGenerationDigestV1,
) -> Result<(GraphIdempotencyKey, String), GraphDbError> {
    let digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.graph-generation-native-finalization.v1",
        &identity.projection,
        &identity.generation,
        &identity.source_generation,
        &identity.watermark,
        expected.as_str(),
    ))
    .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    Ok((
        GraphIdempotencyKey::new(format!("generation-finalize:{}", digest.as_str()))?,
        digest.as_str().to_owned(),
    ))
}

fn prepare_generation_stage_batch(
    manifest: &GraphGenerationManifest,
    identity: &GraphGenerationManifestIdentity,
    context: &GenerationStageContext,
    page: &GenerationStagePage,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphWriteBatch, mutation::RelationEndpointNamespaces), GraphDbError> {
    if page.mutation_count() > MAX_NATIVE_GENERATION_STAGE_MUTATIONS
        || page.live_bytes() > MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES
    {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Write,
            MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
        ));
    }
    let mut endpoint_namespaces = mutation::RelationEndpointNamespaces::new();
    let mutations = match page.kind {
        GenerationStagePageKind::Entities => manifest
            .entities
            .get(page.range.clone())
            .ok_or(GraphDbError::conflict(
                "generation_runtime.prepare_generation_stage_batch",
            ))?
            .iter()
            .map(|entity| {
                check()?;
                Ok(GraphMutation::UpsertEntity(entity.clone()))
            })
            .collect::<Result<Vec<_>, GraphDbError>>()?,
        GenerationStagePageKind::Relations => manifest
            .relations
            .get(page.range.clone())
            .ok_or(GraphDbError::conflict(
                "generation_runtime.prepare_generation_stage_batch",
            ))?
            .iter()
            .map(|relation| {
                check()?;
                endpoint_namespaces.insert(
                    relation.identity.clone(),
                    (
                        endpoint_namespace(
                            identity,
                            &context.physical_namespace,
                            &context.dependency_namespaces,
                            &relation.from.projection,
                        )?,
                        endpoint_namespace(
                            identity,
                            &context.physical_namespace,
                            &context.dependency_namespaces,
                            &relation.to.projection,
                        )?,
                    ),
                );
                Ok(GraphMutation::UpsertRelation(relation.storage_relation()?))
            })
            .collect::<Result<Vec<_>, GraphDbError>>()?,
    };
    let batch = GraphWriteBatch::new_canonical_checked(
        context.physical_namespace.clone(),
        identity.projection.projection.clone(),
        identity.source_generation.clone(),
        identity.watermark.clone(),
        mutations,
        check,
    )?;
    Ok((batch, endpoint_namespaces))
}

#[hotpath::measure(label = "graph_db.generation.page_construct")]
fn construct_generation_stage_page(
    manifest: &GraphGenerationManifest,
    identity: &GraphGenerationManifestIdentity,
    context: &GenerationStageContext,
    page: &GenerationStagePage,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<PreparedGenerationStagePage, GraphDbError> {
    let (batch, endpoint_namespaces) =
        prepare_generation_stage_batch(manifest, identity, context, page, check)?;
    let digest = batch.canonical_digest_checked(check)?;
    Ok(PreparedGenerationStagePage {
        batch,
        endpoint_namespaces,
        digest,
    })
}

#[hotpath::measure(label = "graph_db.generation.page_construct_owned")]
fn construct_owned_generation_stage_page(
    identity: &GraphGenerationManifestIdentity,
    context: &GenerationStageContext,
    page: &GenerationStagePage,
    rows: OwnedGenerationStagePage,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<PreparedGenerationStagePage, GraphDbError> {
    if page.mutation_count() > MAX_NATIVE_GENERATION_STAGE_MUTATIONS
        || page.live_bytes() > MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES
    {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Write,
            MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
        ));
    }
    let mut endpoint_namespaces = mutation::RelationEndpointNamespaces::new();
    let mutations = match rows {
        OwnedGenerationStagePage::Entities(entities) => entities
            .into_iter()
            .map(|entity| {
                check()?;
                Ok(GraphMutation::UpsertEntity(entity))
            })
            .collect::<Result<Vec<_>, GraphDbError>>()?,
        OwnedGenerationStagePage::Relations(relations) => relations
            .into_iter()
            .map(|relation| {
                check()?;
                endpoint_namespaces.insert(
                    relation.identity.clone(),
                    (
                        endpoint_namespace(
                            identity,
                            &context.physical_namespace,
                            &context.dependency_namespaces,
                            &relation.from.projection,
                        )?,
                        endpoint_namespace(
                            identity,
                            &context.physical_namespace,
                            &context.dependency_namespaces,
                            &relation.to.projection,
                        )?,
                    ),
                );
                Ok(GraphMutation::UpsertRelation(GraphRelation::new(
                    relation.identity,
                    relation.from.identity,
                    relation.to.identity,
                    relation.kind,
                    relation.properties,
                )?))
            })
            .collect::<Result<Vec<_>, GraphDbError>>()?,
    };
    let batch = GraphWriteBatch::new_canonical_checked(
        context.physical_namespace.clone(),
        identity.projection.projection.clone(),
        identity.source_generation.clone(),
        identity.watermark.clone(),
        mutations,
        check,
    )?;
    let digest = batch.canonical_digest_checked(check)?;
    Ok(PreparedGenerationStagePage {
        batch,
        endpoint_namespaces,
        digest,
    })
}

fn endpoint_namespace(
    identity: &GraphGenerationManifestIdentity,
    candidate_namespace: &GraphNamespace,
    dependencies: &BTreeMap<crate::GraphProjectionIdentity, GraphNamespace>,
    projection: &crate::GraphProjectionIdentity,
) -> Result<GraphNamespace, GraphDbError> {
    if projection == &identity.projection {
        return Ok(candidate_namespace.clone());
    }
    dependencies
        .get(projection)
        .cloned()
        .ok_or_else(|| GraphDbError::invalid("relation endpoint dependency is not verified"))
}

fn generation_dependency_locators(
    identity: &GraphGenerationManifestIdentity,
) -> Vec<GenerationLocator> {
    identity
        .dependencies
        .iter()
        .map(|dependency| {
            GenerationLocator::new(dependency.projection.clone(), dependency.generation.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// Staging now consumes the manifest, while test call sites keep an
    /// owned one for later assertions, so they hand over a private clone.
    fn arc_manifest(manifest: &GraphGenerationManifest) -> Arc<GraphGenerationManifest> {
        Arc::new(manifest.clone())
    }

    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::hint::black_box;
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    use tempfile::TempDir;
    use tracedecay_store::runtime::GraphRecoveredGenerationDigestV1;

    use crate::generation::{
        canonical_buffer_allocation_growths, manifest_canonicalizations,
        recovered_generation_enumerations, reset_canonical_buffer_allocation_growths,
        reset_manifest_canonicalizations, reset_recovered_generation_enumerations,
    };
    use crate::limits::{
        MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES, MAX_NATIVE_GENERATION_STAGE_MUTATIONS,
    };
    use crate::projection::{
        batch_canonicalizations, max_canonical_batch_mutations, reset_batch_canonicalizations,
    };
    use crate::{
        GraphDbError, GraphDbLocation, GraphDbOpenOptions, GraphDbOwner, GraphDurability,
        GraphEntity, GraphEntityId, GraphFormatVersion, GraphGenerationDependency,
        GraphGenerationId, GraphGenerationManifest, GraphIdempotencyKey, GraphLabel,
        GraphNamespace, GraphProjectionId, GraphProjectionIdentity, GraphProperty,
        GraphPropertyName, GraphVector, GraphVectorIndexRequest, GraphVectorIndexStatus,
        GraphWatermark, MAX_VERIFIED_GENERATION_BATCH_MUTATIONS, NeverCancelled, SourceGeneration,
        VectorMetric,
    };

    use super::{GenerationLocator, GenerationStageOutcome, generation_stage_pages};

    fn manifest(source: &str, watermark: &str) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("recovered-commit").unwrap(),
                GraphProjectionId::new("metadata").unwrap(),
            ),
            GraphGenerationId::new("generation").unwrap(),
            SourceGeneration::new(source).unwrap(),
            GraphWatermark::new(watermark).unwrap(),
            vec![],
            vec![],
            vec![],
        )
        .unwrap()
    }

    fn persistent_database(temp: &TempDir) -> (GraphDbOwner, crate::GraphDbLeaseV1) {
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(temp.path().join("commit-metadata.grafeo")),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        (owner, database)
    }

    fn sealed_digest(
        manifest: &GraphGenerationManifest,
    ) -> tracedecay_store::runtime::GraphRecoveredGenerationDigestV1 {
        manifest.expected_recovered_digest(&|| Ok(())).unwrap()
    }

    #[test]
    fn recovered_generation_rejects_stale_persisted_source_generation() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(
                arc_manifest(&manifest("source:old", "watermark:one")),
                &|| Ok(()),
            )
            .unwrap();

        let changed = manifest("source:new", "watermark:one");
        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &changed.identity(),
                &sealed_digest(&changed),
                changed.row_counts(),
                &|| { Ok(()) }
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn recovered_generation_rejects_stale_persisted_watermark() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(
                arc_manifest(&manifest("source:one", "watermark:old")),
                &|| Ok(()),
            )
            .unwrap();

        let changed = manifest("source:one", "watermark:new");
        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &changed.identity(),
                &sealed_digest(&changed),
                changed.row_counts(),
                &|| { Ok(()) }
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn recovered_generation_rejects_stale_persisted_dependency_metadata() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        let original = manifest("source:one", "watermark:one");
        database
            .apply_generation_unverified(arc_manifest(&original), &|| Ok(()))
            .unwrap();
        let mut changed = original;
        changed.dependencies.push(GraphGenerationDependency::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("dependency").unwrap(),
                GraphProjectionId::new("metadata").unwrap(),
            ),
            GraphGenerationId::new("dependency-generation").unwrap(),
            GraphIdempotencyKey::new("dependency-publication").unwrap(),
        ));

        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &changed.identity(),
                &sealed_digest(&changed),
                changed.row_counts(),
                &|| { Ok(()) }
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));
    }

    #[test]
    fn persistent_generation_reopen_enumerates_large_projection_once() {
        let temp = TempDir::new().unwrap();
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(temp.path().join("one-pass.grafeo")),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        let manifest = GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("one-pass").unwrap(),
                GraphProjectionId::new("large").unwrap(),
            ),
            GraphGenerationId::new("generation-large").unwrap(),
            SourceGeneration::new("source-large").unwrap(),
            GraphWatermark::new("watermark-large").unwrap(),
            vec![],
            (0..5_000)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap();
        database
            .apply_generation_unverified(arc_manifest(&manifest), &|| Ok(()))
            .unwrap();

        let sealed = sealed_digest(&manifest);
        reset_recovered_generation_enumerations();
        reset_canonical_buffer_allocation_growths();
        database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &|| Ok(()),
            )
            .unwrap();

        assert_eq!(recovered_generation_enumerations(), 1);
        let allocation_growths = canonical_buffer_allocation_growths();
        assert!(
            allocation_growths <= 8,
            "5,000 stored rows caused {allocation_growths} canonical-buffer allocation growths"
        );
        owner.close().unwrap();
    }

    fn large_persistent_generation(
        temp: &TempDir,
        namespace: &str,
    ) -> (GraphDbOwner, crate::GraphDbLeaseV1, GraphGenerationManifest) {
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Persistent(temp.path().join(format!("{namespace}.grafeo"))),
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::WalSync,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        let manifest = large_manifest(namespace);
        reset_batch_canonicalizations();
        database
            .apply_generation_unverified(arc_manifest(&manifest), &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            2,
            "one bounded native row page plus final metadata bind are hashed once each"
        );
        (owner, database, manifest)
    }

    fn large_manifest(namespace: &str) -> GraphGenerationManifest {
        GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new(namespace).unwrap(),
                GraphProjectionId::new("large").unwrap(),
            ),
            GraphGenerationId::new("generation-large").unwrap(),
            SourceGeneration::new("source-large").unwrap(),
            GraphWatermark::new("watermark-large").unwrap(),
            vec![],
            (0..5_000)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap()
    }

    fn foreign_recovered_digest() -> GraphRecoveredGenerationDigestV1 {
        GraphRecoveredGenerationDigestV1::new(format!("sha256:{}", "0".repeat(64))).unwrap()
    }

    #[test]
    fn reopen_verification_streams_digest_without_blocking_snapshot_readers() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "reader-admission");

        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let reader = {
            let database = database.clone();
            std::thread::spawn(move || {
                start_rx.recv().unwrap();
                drop(database.snapshot().unwrap());
                done_tx.send(()).unwrap();
            })
        };
        let gate = Arc::clone(&database.inner.snapshot_gate);
        let admitted = Cell::new(0usize);
        let refused = Cell::new(0usize);
        let signalled = Cell::new(false);
        let reader_completed = Cell::new(None::<bool>);
        let check = || {
            if recovered_generation_enumerations() == 0 {
                return Ok(());
            }
            if gate.try_read().is_some() {
                admitted.set(admitted.get() + 1);
            } else {
                refused.set(refused.get() + 1);
            }
            if !signalled.get() {
                signalled.set(true);
                start_tx.send(()).unwrap();
                reader_completed.set(Some(done_rx.recv_timeout(Duration::from_secs(30)).is_ok()));
            }
            Ok(())
        };
        let sealed = sealed_digest(&manifest);
        reset_recovered_generation_enumerations();
        database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &check,
            )
            .unwrap();
        reader.join().unwrap();

        assert!(
            admitted.get() > 0,
            "the recovered-digest stream must run outside the exclusive snapshot gate"
        );
        assert_eq!(
            refused.get(),
            0,
            "no part of the recovered-digest stream may hold the snapshot gate exclusively"
        );
        assert_eq!(
            reader_completed.get(),
            Some(true),
            "a concurrent snapshot reader must complete while the digest streams"
        );
        owner.close().unwrap();
    }

    #[test]
    fn sealed_digest_reopen_skips_manifest_recanonicalization() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "sealed-reuse");
        let sealed = sealed_digest(&manifest);

        reset_recovered_generation_enumerations();
        reset_manifest_canonicalizations();
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(recovered, sealed);
        assert_eq!(
            manifest_canonicalizations(),
            0,
            "the sealed digest replaces every full-manifest re-stream during hydrate"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "the recovered-digest proof still streams the reopened rows"
        );
        owner.close().unwrap();
    }

    #[test]
    fn foreign_sealed_digest_still_fails_recovered_proof_then_repairs() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest) = large_persistent_generation(&temp, "sealed-mismatch");
        let sealed = sealed_digest(&manifest);

        assert!(matches!(
            database.reopen_and_verify_existing_generation(
                &manifest.identity(),
                &foreign_recovered_digest(),
                manifest.row_counts(),
                &|| Ok(())
            ),
            Err(GraphDbError::GenerationMismatch { .. })
        ));

        // The mismatch quarantined the generation. Hydrating with the exact
        // sealed digest clears the durable marker through the checkpoint
        // transition and re-verifies the repaired rows; that second (repair)
        // enumeration must also admit snapshot readers instead of holding
        // the gate exclusively.
        let (start_tx, start_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let reader = {
            let database = database.clone();
            std::thread::spawn(move || {
                start_rx.recv().unwrap();
                drop(database.snapshot().unwrap());
                done_tx.send(()).unwrap();
            })
        };
        let gate = Arc::clone(&database.inner.snapshot_gate);
        let admitted = Cell::new(0usize);
        let refused = Cell::new(0usize);
        let signalled = Cell::new(false);
        let reader_completed = Cell::new(None::<bool>);
        let check = || {
            // The repair path enumerates twice: the pre-repair proof, then
            // the post-checkpoint re-verification. Sample the gate during
            // the repair enumeration specifically.
            if recovered_generation_enumerations() < 2 {
                return Ok(());
            }
            if gate.try_read().is_some() {
                admitted.set(admitted.get() + 1);
            } else {
                refused.set(refused.get() + 1);
            }
            if !signalled.get() {
                signalled.set(true);
                start_tx.send(()).unwrap();
                reader_completed.set(Some(done_rx.recv_timeout(Duration::from_secs(30)).is_ok()));
            }
            Ok(())
        };
        reset_recovered_generation_enumerations();
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &check,
            )
            .unwrap();
        reader.join().unwrap();
        assert_eq!(recovered, sealed);
        assert_eq!(
            recovered_generation_enumerations(),
            2,
            "quarantine repair re-verifies the checkpointed rows"
        );
        assert!(
            admitted.get() > 0,
            "the repair re-verification must run outside the exclusive snapshot gate"
        );
        assert_eq!(
            refused.get(),
            0,
            "no part of the repair re-verification may hold the snapshot gate exclusively"
        );
        assert_eq!(
            reader_completed.get(),
            Some(true),
            "a concurrent snapshot reader must complete while the repair digest streams"
        );
        owner.close().unwrap();
    }

    #[test]
    fn retry_admission_apply_then_reopen_streams_rows_once() {
        let temp = TempDir::new().unwrap();
        let (owner, database) = persistent_database(&temp);
        let manifest = large_manifest("retry-admission");
        let sealed = sealed_digest(&manifest);
        let first = database
            .apply_generation_unverified_with_digest_observed(
                arc_manifest(&manifest),
                &sealed,
                &|| Ok(()),
            )
            .unwrap();
        let GenerationStageOutcome::Applied(first) = first else {
            panic!("a fresh generation must report durable native staging");
        };

        // The live retry admission sequence is publish's apply-then-reopen.
        // The re-seat apply is bookkeeping only; the mandatory close/reopen
        // recovered-digest proof is the one and only row stream.
        reset_recovered_generation_enumerations();
        reset_manifest_canonicalizations();
        reset_batch_canonicalizations();
        let reapplied = database
            .apply_generation_unverified_with_digest_observed(
                arc_manifest(&manifest),
                &sealed,
                &|| Ok(()),
            )
            .unwrap();
        let GenerationStageOutcome::Reseated(reapplied) = reapplied else {
            panic!("an exact retry must resume from durable staging receipts");
        };
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &|| Ok(()),
            )
            .unwrap();

        assert_eq!(reapplied.sequence, first.sequence);
        assert_eq!(reapplied.digest, first.digest);
        assert_eq!(recovered, sealed);
        assert_eq!(
            batch_canonicalizations(),
            0,
            "a retry admission must not rebuild or hash the canonical batch"
        );
        assert_eq!(
            manifest_canonicalizations(),
            0,
            "a retry admission must not re-canonicalize the manifest"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "the whole retry admission streams the rows exactly once, at the reopen proof"
        );
        owner.close().unwrap();
    }

    #[test]
    fn second_generation_apply_writes_instead_of_reseating_prior() {
        let temp = TempDir::new().unwrap();
        let (owner, database, manifest_a) = large_persistent_generation(&temp, "two-generations");
        let mut entities_b = manifest_a.entities.clone();
        entities_b.push(
            GraphEntity::new(
                GraphEntityId::new("entity:only-in-b").unwrap(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap(),
        );
        let manifest_b = GraphGenerationManifest::new(
            manifest_a.projection.clone(),
            GraphGenerationId::new("generation-b").unwrap(),
            SourceGeneration::new("source-b").unwrap(),
            GraphWatermark::new("watermark-b").unwrap(),
            vec![],
            entities_b,
            vec![],
        )
        .unwrap();

        reset_batch_canonicalizations();
        let commit_b = database
            .apply_generation_unverified(arc_manifest(&manifest_b), &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            2,
            "a different generation must stage one native page and its final metadata bind"
        );
        assert_eq!(commit_b.source_generation.as_str(), "source-b");
        assert_eq!(commit_b.watermark.as_str(), "watermark-b");

        // B's rows are really stored: the close/reopen recovered-digest
        // proof over B's generation (including the entity A never had)
        // seats B's sealed digest.
        let sealed_b = sealed_digest(&manifest_b);
        let (reopened_b, recovered_b) = database
            .reopen_and_verify_existing_generation(
                &manifest_b.identity(),
                &sealed_b,
                manifest_b.row_counts(),
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(recovered_b, sealed_b);
        assert_eq!(reopened_b.source_generation.as_str(), "source-b");

        // A same-generation retry of A still takes the cheap re-seat.
        reset_batch_canonicalizations();
        let retried_a = database
            .apply_generation_unverified(arc_manifest(&manifest_a), &|| Ok(()))
            .unwrap();
        assert_eq!(batch_canonicalizations(), 0);
        assert_eq!(retried_a.source_generation.as_str(), "source-large");
        assert_ne!(commit_b.digest, retried_a.digest);
        assert_ne!(
            commit_b.source_generation, retried_a.source_generation,
            "generation B's commit must never alias generation A's"
        );
        owner.close().unwrap();
    }

    #[test]
    fn divergent_identity_apply_falls_through_to_write() {
        let temp = TempDir::new().unwrap();
        let (_owner, database) = persistent_database(&temp);
        database
            .apply_generation_unverified(
                arc_manifest(&manifest("source:one", "watermark:one")),
                &|| Ok(()),
            )
            .unwrap();

        // Same projection and generation id (same physical namespace), but a
        // different stored identity: the re-seat bind must refuse the cheap
        // return and fall through to a real apply instead of fail-closing
        // the apply with a verification mismatch.
        let divergent = manifest("source:two", "watermark:two");
        reset_batch_canonicalizations();
        let commit = database
            .apply_generation_unverified(arc_manifest(&divergent), &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            1,
            "a divergent identity must write, not re-seat"
        );
        assert_eq!(commit.source_generation.as_str(), "source:two");
        assert_eq!(commit.watermark.as_str(), "watermark:two");

        // Retrying the now-stored identity takes the cheap re-seat.
        reset_batch_canonicalizations();
        let retried = database
            .apply_generation_unverified(arc_manifest(&divergent), &|| Ok(()))
            .unwrap();
        assert_eq!(batch_canonicalizations(), 0);
        assert_eq!(retried.source_generation.as_str(), "source:two");
    }

    #[test]
    fn native_generation_stages_sixty_five_thousand_rows_in_one_durable_page() {
        let manifest = GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("wide-native-stage").unwrap(),
                GraphProjectionId::new("entities").unwrap(),
            ),
            GraphGenerationId::new("generation-wide-native-stage").unwrap(),
            SourceGeneration::new("source-wide-native-stage").unwrap(),
            GraphWatermark::new("watermark-wide-native-stage").unwrap(),
            vec![],
            (0..65_536)
                .map(|index| {
                    GraphEntity::new(
                        GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                        BTreeSet::new(),
                        BTreeMap::new(),
                    )
                    .unwrap()
                })
                .collect(),
            vec![],
        )
        .unwrap();
        let pages = generation_stage_pages(&manifest).unwrap();

        assert_eq!(
            pages.len(),
            1,
            "native generation staging must avoid a full-graph Grafeo commit scan every 4,096 rows"
        );

        let temp = TempDir::new().unwrap();
        let (owner, database) = persistent_database(&temp);
        reset_batch_canonicalizations();
        database
            .apply_generation_unverified(arc_manifest(&manifest), &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            2,
            "one native data page and one metadata bind must be committed"
        );
        owner.close().unwrap();
    }

    #[test]
    #[ignore = "large synthetic staging timing/RSS harness; run explicitly in a fresh process"]
    fn generation_stage_ownership_sandbox_probe() {
        let rows = std::env::var("TRACEDECAY_STAGE_BENCH_ROWS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(250_000usize);
        let mode =
            std::env::var("TRACEDECAY_STAGE_BENCH_MODE").unwrap_or_else(|_| "owned".to_owned());
        let payload = "staging-payload-".repeat(16);
        let manifest = Arc::new(
            GraphGenerationManifest::new(
                GraphProjectionIdentity::new(
                    GraphNamespace::new("stage-ownership-sandbox").unwrap(),
                    GraphProjectionId::new("entities").unwrap(),
                ),
                GraphGenerationId::new("stage-ownership-sandbox-generation").unwrap(),
                SourceGeneration::new("stage-ownership-sandbox-source").unwrap(),
                GraphWatermark::new("stage-ownership-sandbox-watermark").unwrap(),
                vec![],
                (0..rows)
                    .map(|index| {
                        GraphEntity::new(
                            GraphEntityId::new(format!("entity:{index:08}")).unwrap(),
                            BTreeSet::from([GraphLabel::new("symbol").unwrap()]),
                            BTreeMap::from([(
                                GraphPropertyName::new("payload").unwrap(),
                                GraphProperty::String(payload.clone()),
                            )]),
                        )
                        .unwrap()
                    })
                    .collect(),
                vec![],
            )
            .unwrap(),
        );
        let retained = match mode.as_str() {
            "owned" => None,
            "shared" => Some(Arc::clone(&manifest)),
            other => panic!("unknown TRACEDECAY_STAGE_BENCH_MODE `{other}`"),
        };
        let expected = manifest.expected_recovered_digest(&|| Ok(())).unwrap();
        let temp = TempDir::new().unwrap();
        let (owner, database) = persistent_database(&temp);
        let rss_before = proc_status_kib("VmRSS");
        let hwm_before = proc_status_kib("VmHWM");
        let started = Instant::now();
        database
            .apply_generation_unverified_with_digest(manifest, &expected, &|| Ok(()))
            .unwrap();
        let elapsed = started.elapsed();
        black_box(&retained);
        println!(
            "generation_stage mode={mode} rows={rows} elapsed_ms={} rss_before_kib={} \
             rss_after_kib={} hwm_before_kib={} hwm_after_kib={}",
            elapsed.as_millis(),
            rss_before,
            proc_status_kib("VmRSS"),
            hwm_before,
            proc_status_kib("VmHWM"),
        );
        drop(retained);
        owner.close().unwrap();
    }

    fn proc_status_kib(field: &str) -> u64 {
        let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
            return 0;
        };
        let prefix = format!("{field}:");
        status
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    #[test]
    fn generation_stage_page_planner_bounds_mutations_and_live_property_bytes() {
        let mut pages = Vec::new();
        super::append_generation_stage_pages_with_limits(
            &mut pages,
            super::GenerationStagePageKind::Entities,
            17,
            |_| Ok(8 * 1024 * 1024),
            MAX_NATIVE_GENERATION_STAGE_MUTATIONS,
            MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
        )
        .unwrap();

        assert!(
            pages.len() > 1,
            "the property-byte ceiling must split this input"
        );
        assert!(pages.iter().all(|page| {
            page.mutation_count() <= MAX_NATIVE_GENERATION_STAGE_MUTATIONS
                && page.live_bytes() <= MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES
        }));
        assert_eq!(
            pages
                .iter()
                .map(|page| page.mutation_count())
                .sum::<usize>(),
            17
        );
    }

    #[test]
    fn parallel_page_construction_matches_serial_digests() {
        let manifest = large_manifest("parallel-page-construct");
        let pages = super::generation_stage_pages_with_limits(
            &manifest,
            2_048,
            MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
        )
        .unwrap();
        assert!(
            pages.len() >= 2,
            "the fixture must split so construction can overlap"
        );
        let identity = manifest.identity();
        let context = super::GenerationStageContext {
            locator: GenerationLocator::new(
                manifest.projection.clone(),
                manifest.generation.clone(),
            ),
            physical_namespace: identity.physical_namespace().unwrap(),
            dependency_namespaces: BTreeMap::new(),
            dependency_digest: manifest.dependency_closure_digest(&|| Ok(())).unwrap(),
        };

        let serial_started = Instant::now();
        let serial = pages
            .iter()
            .map(|page| {
                super::construct_generation_stage_page(
                    &manifest,
                    &identity,
                    &context,
                    page,
                    &|| Ok(()),
                )
                .map(|prepared| prepared.digest)
            })
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let serial_ms = serial_started.elapsed();

        let parallel_started = Instant::now();
        let parallel = std::thread::scope(|scope| {
            pages
                .iter()
                .map(|page| {
                    scope.spawn(|| {
                        super::construct_generation_stage_page(
                            &manifest,
                            &identity,
                            &context,
                            page,
                            &|| Ok(()),
                        )
                        .map(|prepared| prepared.digest)
                    })
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap();
        let parallel_ms = parallel_started.elapsed();

        assert_eq!(serial, parallel);
        eprintln!(
            "page_construct pages={} serial={serial_ms:?} parallel={parallel_ms:?}",
            pages.len()
        );
    }

    #[test]
    fn pipelined_custom_pages_seal_the_same_digest() {
        let manifest = large_manifest("pipeline-page-apply");
        let pages = super::generation_stage_pages_with_limits(
            &manifest,
            2_048,
            MAX_NATIVE_GENERATION_STAGE_LIVE_BYTES,
        )
        .unwrap();
        assert!(pages.len() >= 2);
        let sealed = sealed_digest(&manifest);
        let temp = TempDir::new().unwrap();
        let (owner, database) = persistent_database(&temp);
        let identity = manifest.identity();
        let context = super::GenerationStageContext {
            locator: GenerationLocator::new(
                manifest.projection.clone(),
                manifest.generation.clone(),
            ),
            physical_namespace: identity.physical_namespace().unwrap(),
            dependency_namespaces: database.require_exact_dependencies(&identity).unwrap(),
            dependency_digest: manifest.dependency_closure_digest(&|| Ok(())).unwrap(),
        };
        let started = Instant::now();
        database
            .stage_generation_pages(
                &manifest,
                &identity,
                &sealed,
                &context,
                &pages,
                false,
                &|| Ok(()),
            )
            .unwrap();
        database
            .finalize_staged_generation(&identity, &sealed, &context, pages.last(), &|| Ok(()))
            .unwrap();
        eprintln!(
            "page_pipeline pages={} stage+finalize={:?}",
            pages.len(),
            started.elapsed()
        );
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(
                &identity,
                &sealed,
                manifest.row_counts(),
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(recovered, sealed);
        owner.close().unwrap();
    }

    #[test]
    fn partial_stage_resumes_to_applied_then_reseats_before_one_reopen() {
        let manifest = large_manifest("interrupted-stage-resume");
        let locator =
            GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone());

        // Rebuild this generation in a fresh database and cancel as soon as
        // the first native receipt becomes readable. The transaction itself
        // never observes cancellation after commit; the next explicit page
        // boundary does, leaving an exact durable resume point.
        let second_temp = TempDir::new().unwrap();
        let (second_owner, second_database) = persistent_database(&second_temp);
        let pages = generation_stage_pages(&manifest).unwrap();
        let sealed = sealed_digest(&manifest);
        let physical_namespace = manifest.identity().physical_namespace().unwrap();
        let (first_page_key, _) =
            super::generation_stage_page_receipt(&manifest.identity(), &sealed, &pages[0]).unwrap();
        let cancel_after_first_page = || {
            let Ok(database_guard) = second_database.inner.database.try_read() else {
                return Ok(());
            };
            let Some(database) = database_guard.as_ref() else {
                return Err(GraphDbError::Closed);
            };
            if crate::state::publication(database, &physical_namespace, &first_page_key)?.is_some()
            {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert!(matches!(
            second_database.apply_generation_unverified_with_digest_observed(
                arc_manifest(&manifest),
                &sealed,
                &cancel_after_first_page
            ),
            Err(GraphDbError::Cancelled)
        ));

        assert!(
            second_database
                .verified_generation(&locator)
                .unwrap()
                .is_none()
        );
        assert!(
            !second_database
                .inner
                .verified_generations
                .read()
                .unwrap()
                .stored
                .contains_key(&locator),
            "a partial physical stage must not become serveable or retained as complete"
        );

        reset_batch_canonicalizations();
        let resumed = second_database
            .apply_generation_unverified_with_digest_observed(
                arc_manifest(&manifest),
                &sealed,
                &|| Ok(()),
            )
            .unwrap();
        let GenerationStageOutcome::Applied(resumed) = resumed else {
            panic!("a partial stage must finish its durable pages before yielding to reopen");
        };
        assert_eq!(
            batch_canonicalizations(),
            1,
            "the exact native-page receipt must skip directly to finalization on resume"
        );

        reset_recovered_generation_enumerations();
        reset_batch_canonicalizations();
        let exact = second_database
            .apply_generation_unverified_with_digest_observed(
                arc_manifest(&manifest),
                &sealed,
                &|| Ok(()),
            )
            .unwrap();
        let GenerationStageOutcome::Reseated(exact) = exact else {
            panic!("the boundary retry must re-seat the exact durable stage");
        };
        assert_eq!(exact.sequence, resumed.sequence);
        assert_eq!(
            batch_canonicalizations(),
            0,
            "the boundary retry must not rebuild any durable page"
        );
        assert_eq!(
            recovered_generation_enumerations(),
            0,
            "re-seat must not stream rows before the mandatory reopen"
        );
        let (_, recovered) = second_database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(recovered, sealed, "the reopened rows must prove the seal");
        assert_eq!(
            recovered_generation_enumerations(),
            1,
            "partial resume, boundary yield, and re-seat must perform one reopen"
        );
        second_owner.close().unwrap();
    }

    #[test]
    fn wider_native_stage_adopts_an_exact_legacy_partial_receipt() {
        let mut manifest = large_manifest("legacy-page-resume");
        manifest.entities.extend((5_000..9_000).map(|index| {
            GraphEntity::new(
                GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                BTreeSet::new(),
                BTreeMap::new(),
            )
            .unwrap()
        }));
        let sealed = sealed_digest(&manifest);
        let temp = TempDir::new().unwrap();
        let (owner, database) = persistent_database(&temp);
        let legacy_pages = super::generation_stage_pages_with_limits(
            &manifest,
            MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
            crate::MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
        )
        .unwrap();
        assert_eq!(legacy_pages.len(), 3);
        assert_eq!(legacy_pages[0].range, 0..4_096);
        assert_eq!(legacy_pages[1].range, 4_096..8_192);
        assert_eq!(
            super::first_generation_stage_page_with_limits(
                &manifest,
                MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
                crate::MAX_VERIFIED_GENERATION_BATCH_LIVE_BYTES,
            )
            .unwrap(),
            Some(legacy_pages[0].clone()),
            "the bounded compatibility probe must reproduce the legacy first receipt"
        );
        let context = super::GenerationStageContext {
            locator: GenerationLocator::new(
                manifest.projection.clone(),
                manifest.generation.clone(),
            ),
            physical_namespace: manifest.identity().physical_namespace().unwrap(),
            dependency_namespaces: database
                .require_exact_dependencies(&manifest.identity())
                .unwrap(),
            dependency_digest: manifest.dependency_closure_digest(&|| Ok(())).unwrap(),
        };
        for (index, legacy_page) in legacy_pages.iter().take(2).enumerate() {
            database
                .apply_generation_stage_page_with_context(
                    &manifest,
                    &manifest.identity(),
                    &sealed,
                    &context,
                    index
                        .checked_sub(1)
                        .and_then(|prior| legacy_pages.get(prior)),
                    legacy_page,
                    false,
                    &|| Ok(()),
                )
                .unwrap();
        }

        let mut divergent = manifest.clone();
        divergent.source_generation = SourceGeneration::new("source-divergent").unwrap();
        let divergent_sealed = sealed_digest(&divergent);
        reset_batch_canonicalizations();
        assert!(
            matches!(
                database.apply_generation_unverified_with_digest_observed(
                    arc_manifest(&divergent),
                    &divergent_sealed,
                    &|| Ok(())
                ),
                Err(GraphDbError::Conflict { .. })
            ),
            "a legacy prefix may be replaced only by its exact source authority"
        );
        assert_eq!(
            batch_canonicalizations(),
            0,
            "a divergent legacy migration must fail before writing"
        );

        reset_batch_canonicalizations();
        let outcome = database
            .apply_generation_unverified_with_digest_observed(
                arc_manifest(&manifest),
                &sealed,
                &|| Ok(()),
            )
            .expect("an exact legacy partial stage must migrate to the wider page layout");
        assert!(matches!(outcome, GenerationStageOutcome::Applied(_)));
        assert_eq!(
            batch_canonicalizations(),
            2,
            "migration must write one wide data page and one final metadata bind"
        );
        let (_, recovered) = database
            .reopen_and_verify_existing_generation(
                &manifest.identity(),
                &sealed,
                manifest.row_counts(),
                &|| Ok(()),
            )
            .unwrap();
        assert_eq!(recovered, sealed);
        owner.close().unwrap();
    }

    #[test]
    fn near_complete_cancelled_stage_retires_in_bounded_idempotent_pages() {
        let manifest = large_manifest("bounded-retirement");
        let locator =
            GenerationLocator::new(manifest.projection.clone(), manifest.generation.clone());
        let temp = TempDir::new().unwrap();
        let (owner, database) = persistent_database(&temp);
        let pages = generation_stage_pages(&manifest).unwrap();
        assert_eq!(
            pages.len(),
            1,
            "the fixture must stage in exactly one native page"
        );
        let sealed = sealed_digest(&manifest);
        let physical_namespace = manifest.identity().physical_namespace().unwrap();
        let (last_page_key, _) =
            super::generation_stage_page_receipt(&manifest.identity(), &sealed, &pages[0]).unwrap();
        let cancel_before_finalization = || {
            let Ok(database_guard) = database.inner.database.try_read() else {
                return Ok(());
            };
            let Some(native) = database_guard.as_ref() else {
                return Err(GraphDbError::Closed);
            };
            if crate::state::publication(native, &physical_namespace, &last_page_key)?.is_some() {
                Err(GraphDbError::Cancelled)
            } else {
                Ok(())
            }
        };
        assert_eq!(
            database.apply_generation_unverified_with_digest(
                arc_manifest(&manifest),
                &sealed,
                &cancel_before_finalization
            ),
            Err(GraphDbError::Cancelled),
            "all native rows may commit, but finalization must remain cancelled"
        );

        reset_batch_canonicalizations();
        database
            .delete_generation_contents(&locator, &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            2,
            "5,000 entities must retire as 4,096 plus 904, never one full-generation batch"
        );
        assert!(
            max_canonical_batch_mutations() <= MAX_VERIFIED_GENERATION_BATCH_MUTATIONS,
            "every retirement transaction must obey the generation staging mutation bound"
        );
        let counts = {
            let guard = database.read_guard().unwrap();
            crate::state::projection_node_counts(
                guard.as_ref().unwrap(),
                &physical_namespace,
                &manifest.projection.projection,
            )
            .unwrap()
        };
        assert_eq!(counts, (0, 0));

        database
            .delete_generation_contents(&locator, &|| Ok(()))
            .unwrap();
        assert_eq!(
            batch_canonicalizations(),
            2,
            "an exact cleanup retry after every row is gone must be a no-op"
        );
        owner.close().unwrap();
    }

    #[test]
    fn later_generation_page_creates_its_first_native_vector_index() {
        let vector_property = GraphPropertyName::new("embedding").unwrap();
        let mut entities = (0..MAX_NATIVE_GENERATION_STAGE_MUTATIONS)
            .map(|index| {
                GraphEntity::new(
                    GraphEntityId::new(format!("entity:{index:05}")).unwrap(),
                    BTreeSet::new(),
                    BTreeMap::new(),
                )
                .unwrap()
            })
            .collect::<Vec<_>>();
        entities.push(
            GraphEntity::new(
                GraphEntityId::new("z-vector").unwrap(),
                BTreeSet::new(),
                BTreeMap::from([(
                    vector_property.clone(),
                    GraphProperty::Vector(
                        GraphVector::new(vec![1.0, 0.0], 2, VectorMetric::Cosine).unwrap(),
                    ),
                )]),
            )
            .unwrap(),
        );
        let manifest = GraphGenerationManifest::new(
            GraphProjectionIdentity::new(
                GraphNamespace::new("later-page-vector").unwrap(),
                GraphProjectionId::new("mixed").unwrap(),
            ),
            GraphGenerationId::new("generation-mixed").unwrap(),
            SourceGeneration::new("source-mixed").unwrap(),
            GraphWatermark::new("watermark-mixed").unwrap(),
            vec![],
            entities,
            vec![],
        )
        .unwrap();
        let owner = GraphDbOwner::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation: Arc::new(NeverCancelled),
        })
        .unwrap();
        let database = owner.issue_lease().unwrap();
        database
            .apply_generation_unverified(arc_manifest(&manifest), &|| Ok(()))
            .unwrap();

        assert_eq!(
            database
                .vector_index_status(GraphVectorIndexRequest {
                    namespace: manifest.identity().physical_namespace().unwrap(),
                    projection: manifest.projection.projection.clone(),
                    property: vector_property,
                    dimension: 2,
                    metric: VectorMetric::Cosine,
                    cancellation: Arc::new(NeverCancelled),
                })
                .unwrap(),
            GraphVectorIndexStatus::Available { vectors: 1 },
            "a vector shape first seen after page one must create its native index"
        );
        owner.close().unwrap();
    }
}
