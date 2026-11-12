//! Exact sealed-code replay from the registered project's SQL authority.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use tracedecay_code_index::graph_projection::{
    CodeGraphLayeredReportV1, CodeGraphProjectionError, SealedCodeGraphRowsError,
    build_layered_code_graph_rows, build_sealed_code_graph_rows,
};
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, SealedGenerationFileWindowsV1, SealedGenerationSegmentReadV1,
    SealedGenerationSegmentReaderV1,
};
use tracedecay_domain::{CodeGenerationId, ManifestDigest, ProjectId, RepositoryId};
use tracedecay_graph_db::{
    GraphBudgetKind, GraphDbError, GraphGenerationManifestProvider, GraphGenerationRowSpill,
    GraphGenerationRows, GraphLayeredRowSpill, GraphNamespace, GraphProjectionId,
    GraphProjectionIdentity, GraphProjectorRevision, GraphSealedBaseAbsenceV1,
    GraphSiblingSealedBaseV1, SealedCodeGenerationReplay, SealedGraphStateDigest,
    SpilledGraphGeneration,
};
use tracedecay_runtime_core::db::{
    CodeIndexGenerationKey, CodeIndexPayloadIdentity, CodeIndexSqliteStorage,
    CodeIndexStorageError, CodeIndexStoredBytes,
};
use tracedecay_runtime_core::resident_memory::ResidentMemoryPressureV1;
use tracedecay_store::{GraphProjectionIdentityV1, StoreShardIdV1, StoreShardScopeV1};

fn decode_error(error: CodeIndexProductionErrorV1, digest: &ManifestDigest) -> GraphDbError {
    match error {
        CodeIndexProductionErrorV1::SealedGenerationRetired => GraphDbError::SealSuperseded,
        CodeIndexProductionErrorV1::SourceCommitmentsUnavailable => {
            GraphDbError::SourceCommitmentsUnavailable {
                sealed_state_digest: digest.as_str().to_owned(),
            }
        }
        error @ (CodeIndexProductionErrorV1::SealedRowContractRefused { .. }
        | CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(_)) => {
            GraphDbError::SealedRevisionIncompatible {
                sealed_state_digest: digest.as_str().to_owned(),
                message: error.to_string(),
            }
        }
        error => GraphDbError::Corrupt {
            message: format!("sealed code generation replay is invalid: {error}"),
        },
    }
}

pub(super) fn sql_graph_error(error: CodeIndexStorageError) -> GraphDbError {
    match error {
        CodeIndexStorageError::Retired => GraphDbError::SealSuperseded,
        CodeIndexStorageError::Corrupt(message) => GraphDbError::Corrupt { message },
        CodeIndexStorageError::AllocationRefused(_) => GraphDbError::budget_exhausted(
            GraphBudgetKind::ResidentMemory,
            tracedecay_runtime_core::resident_memory::detected_process_resident_memory_limit_v1()
                .get(),
        ),
        error => GraphDbError::unavailable(error.to_string()),
    }
}

fn projection_error(error: CodeGraphProjectionError) -> GraphDbError {
    match error {
        CodeGraphProjectionError::Cancelled => GraphDbError::Cancelled,
        CodeGraphProjectionError::DeadlineExceeded => GraphDbError::DeadlineExceeded,
        CodeGraphProjectionError::BudgetExhausted { budget, limit } => {
            GraphDbError::budget_exhausted(
                GraphBudgetKind::from_name(&budget).unwrap_or(GraphBudgetKind::Read),
                limit,
            )
        }
        error => GraphDbError::Corrupt {
            message: format!("sealed generation graph projection is invalid: {error}"),
        },
    }
}

struct VerifiedSealGraphSourceV1 {
    source: SealedGenerationFileWindowsV1,
    digest: ManifestDigest,
    storage: CodeIndexSqliteStorage,
    generation: CodeIndexGenerationKey,
}

fn open_sql_seal(
    storage: &CodeIndexSqliteStorage,
    generation: &CodeIndexGenerationKey,
    digest: &SealedGraphStateDigest,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<VerifiedSealGraphSourceV1, GraphDbError> {
    check()?;
    let digest = ManifestDigest::new(digest.as_str())
        .map_err(|error| GraphDbError::invalid(error.to_string()))?;
    let bytes = storage.read_manifest(generation, &digest).map_err(sql_graph_error)?;
    let source = SealedGenerationFileWindowsV1::open(&bytes)
        .map_err(|error| decode_error(error, &digest))?;
    if source.generation_id() != &generation.generation_id {
        return Err(GraphDbError::conflict("code_graph_manifest.sql_generation"));
    }
    check()?;
    Ok(VerifiedSealGraphSourceV1 {
        source, digest, storage: storage.clone(), generation: generation.clone(),
    })
}

/// Preserve the caller's exact interruption or SQL retirement even when a codec
/// reports the failed callback as a source-contract error.
fn with_verified_segments<T>(
    seal: &VerifiedSealGraphSourceV1,
    check: &dyn Fn() -> Result<(), GraphDbError>,
    build: impl FnOnce(&mut SealedGenerationSegmentReaderV1<'_>)
        -> Result<T, SealedCodeGraphRowsError>,
) -> Result<T, GraphDbError> {
    let mut failure = None;
    let mut read = |request: SealedGenerationSegmentReadV1<'_>|
        -> Result<CodeIndexStoredBytes, CodeIndexProductionErrorV1> {
        let result: Result<CodeIndexStoredBytes, GraphDbError> = (|| {
            check()?;
            let (digest, size_bytes, offset, length) = match request {
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                    (digest, size_bytes, 0, size_bytes)
                }
                SealedGenerationSegmentReadV1::Range { digest, size_bytes, offset, length } => {
                    (digest, size_bytes, offset, length)
                }
            };
            let bytes = seal.storage.read_range(
                &seal.generation,
                &CodeIndexPayloadIdentity { digest: digest.clone(), size_bytes },
                offset, length,
            ).map_err(sql_graph_error)?;
            check()?;
            Ok(bytes)
        })();
        result.map_err(|error| {
            failure = Some(error.clone());
            CodeIndexProductionErrorV1::Contract(error.to_string())
        })
    };
    let built = build(&mut read);
    if let Some(failure) = failure {
        return Err(failure);
    }
    built.map_err(|error| match error {
        SealedCodeGraphRowsError::Source(error) => decode_error(error, &seal.digest),
        SealedCodeGraphRowsError::Projection(error) => projection_error(error),
    })
}

#[allow(clippy::too_many_arguments)]
fn spill_verified_seal_graph(
    seal: &VerifiedSealGraphSourceV1,
    projection: GraphProjectionIdentity,
    projector_revision: &GraphProjectorRevision,
    spill: GraphGenerationRowSpill,
    admit: &mut dyn FnMut(
        tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<(), GraphDbError>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    with_verified_segments(seal, check, |read| {
        build_sealed_code_graph_rows(
            projection, &seal.source, read, projector_revision, spill, admit, check,
        )
    })
}

#[cfg(test)]
pub(super) fn spill_sealed_generation_graph(
    storage: &CodeIndexSqliteStorage,
    generation: &CodeIndexGenerationKey,
    digest: &SealedGraphStateDigest,
    projection: GraphProjectionIdentity,
    revision: &GraphProjectorRevision,
    spill: GraphGenerationRowSpill,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<SpilledGraphGeneration, GraphDbError> {
    let seal = open_sql_seal(storage, generation, digest, check)?;
    spill_verified_seal_graph(&seal, projection, revision, spill, &mut |_| Ok(()), check)
}

pub(super) type LayeredRowSpillV1 =
    Result<Result<GraphLayeredRowSpill, GraphSealedBaseAbsenceV1>, GraphDbError>;

pub(super) enum LayeredBaseV1<'a> {
    Parent(&'a CodeGenerationId),
    Sibling(GraphSiblingSealedBaseV1),
}

#[allow(clippy::too_many_arguments)]
#[tracing::instrument(name = "daemon.session_registry.seal.graph_rows", level = "trace", skip_all)]
pub(super) fn graph_rows_from_sql(
    storage: &CodeIndexSqliteStorage,
    generation: &CodeIndexGenerationKey,
    digest: &SealedGraphStateDigest,
    projection: GraphProjectionIdentity,
    revision: &GraphProjectorRevision,
    layered_spill: &dyn Fn(LayeredBaseV1<'_>) -> LayeredRowSpillV1,
    sibling_bases: &dyn Fn() -> Result<Vec<GraphSiblingSealedBaseV1>, GraphDbError>,
    cold_spill: &dyn Fn() -> Result<GraphGenerationRowSpill, GraphDbError>,
    admit: &mut dyn FnMut(
        tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<(), GraphDbError>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<(GraphGenerationRows, Option<CodeGraphLayeredReportV1>), GraphDbError> {
    let seal = open_sql_seal(storage, generation, digest, check)?;
    let mut layered = |base: &str, spill: LayeredRowSpillV1| {
        layered_graph_rows(&seal, &projection, revision, base, spill, admit, check)
    };
    if let Some(parent) = seal.source.manifest().parent_generation.clone() {
        if let Some(built) = layered(parent.as_str(), layered_spill(LayeredBaseV1::Parent(&parent)))? {
            return Ok(built);
        }
    } else {
        let siblings = match sibling_bases() {
            Ok(siblings) => siblings,
            Err(error) if is_interruption(&error) => return Err(error),
            Err(error) => {
                tracing::warn!(%error, "sibling sealed graphs unavailable; sealing cold");
                Vec::new()
            }
        };
        for sibling in siblings {
            let base = sibling.base().generation().as_str().to_owned();
            if let Some(built) = layered(&base, layered_spill(LayeredBaseV1::Sibling(sibling)))? {
                return Ok(built);
            }
        }
    }
    let rows = spill_verified_seal_graph(
        &seal, projection, revision, cold_spill()?, admit, check,
    )?;
    Ok((rows.into(), None))
}

fn is_interruption(error: &GraphDbError) -> bool {
    matches!(error, GraphDbError::Cancelled | GraphDbError::DeadlineExceeded
        | GraphDbError::SealSuperseded)
}

#[allow(clippy::too_many_arguments)]
fn layered_graph_rows(
    seal: &VerifiedSealGraphSourceV1,
    projection: &GraphProjectionIdentity,
    revision: &GraphProjectorRevision,
    base: &str,
    spill: LayeredRowSpillV1,
    admit: &mut dyn FnMut(
        tracedecay_code_index::production::CodeGraphBuildBoundV1,
    ) -> Result<(), GraphDbError>,
    check: &dyn Fn() -> Result<(), GraphDbError>,
) -> Result<Option<(GraphGenerationRows, Option<CodeGraphLayeredReportV1>)>, GraphDbError> {
    let layered = spill.and_then(|spill| match spill {
        Ok(spill) => with_verified_segments(seal, check, |read| {
            build_layered_code_graph_rows(
                projection.clone(), &seal.source, read, revision, spill, admit, check,
            )
        }).map(|built| built.map_err(|reason| format!("{reason:?}"))),
        Err(absence) => Ok(Err(format!("{absence:?}"))),
    });
    match layered {
        Ok(Ok(built)) => Ok(Some((built.generation.into(), Some(built.report)))),
        Err(error) if is_interruption(&error) => Err(error),
        Ok(Err(reason)) => {
            tracing::info!(%base, %reason, "layered graph declined; sealing cold");
            Ok(None)
        }
        Err(error) => {
            tracing::warn!(%base, %error, "layered graph unavailable; sealing cold");
            Ok(None)
        }
    }
}

#[derive(Clone)]
struct BoundCodeGenerationSourceV1 {
    project_id: ProjectId,
    routes: BTreeMap<RepositoryId, usize>,
    storage: CodeIndexSqliteStorage,
}

#[must_use = "retain the route guard for the runtime's lifetime"]
pub(super) struct CodeGraphManifestRouteV1 {
    provider: Arc<DaemonCodeGraphManifestProviderV1>,
    shard: StoreShardIdV1,
    repository: RepositoryId,
}

impl Drop for CodeGraphManifestRouteV1 {
    fn drop(&mut self) {
        let mut sources = match self.provider.sources.write() {
            Ok(sources) => sources,
            Err(error) => {
                tracing::error!(%error, "cannot retire poisoned graph replay route registry");
                return;
            }
        };
        if let Some(binding) = sources.get_mut(&self.shard) {
            if let Some(references) = binding.routes.get_mut(&self.repository) {
                *references -= 1;
                if *references == 0 { binding.routes.remove(&self.repository); }
            }
            if binding.routes.is_empty() { sources.remove(&self.shard); }
        }
    }
}

pub(super) struct DaemonCodeGraphManifestProviderV1 {
    sources: RwLock<BTreeMap<StoreShardIdV1, BoundCodeGenerationSourceV1>>,
    pressure: Arc<ResidentMemoryPressureV1>,
}

impl Default for DaemonCodeGraphManifestProviderV1 {
    fn default() -> Self {
        Self::with_pressure(
            tracedecay_runtime_core::resident_memory::process_resident_memory_pressure_v1(),
        )
    }
}

impl DaemonCodeGraphManifestProviderV1 {
    pub(super) fn with_pressure(pressure: &Arc<ResidentMemoryPressureV1>) -> Self {
        Self { sources: RwLock::new(BTreeMap::new()), pressure: Arc::clone(pressure) }
    }

    pub(super) fn resident_memory_pressure(&self) -> &Arc<ResidentMemoryPressureV1> {
        &self.pressure
    }

    pub(super) fn bind_sql(
        self: &Arc<Self>,
        shard: StoreShardIdV1,
        project: ProjectId,
        repository: RepositoryId,
        storage: CodeIndexSqliteStorage,
    ) -> Result<CodeGraphManifestRouteV1, GraphDbError> {
        if storage.binding().shard_id != shard
            || !matches!(&shard.scope, StoreShardScopeV1::Project { project_id } if project_id == &project)
        {
            return Err(GraphDbError::conflict("code_graph_manifest.sql_binding"));
        }
        let mut sources = self.sources.write()
            .map_err(|_| GraphDbError::unavailable("code graph manifest registry poisoned"))?;
        match sources.get_mut(&shard) {
            Some(source) => {
                if source.project_id != project || source.storage.binding() != storage.binding()
                    || source.storage.verified_locator() != storage.verified_locator()
                {
                    return Err(GraphDbError::conflict("code_graph_manifest.sql_route"));
                }
                let count = source.routes.entry(repository.clone()).or_default();
                *count = count.checked_add(1)
                    .ok_or_else(|| GraphDbError::unavailable("graph route reference overflow"))?;
            }
            None => {
                sources.insert(shard.clone(), BoundCodeGenerationSourceV1 {
                    project_id: project, routes: BTreeMap::from([(repository.clone(), 1)]), storage,
                });
            }
        }
        Ok(CodeGraphManifestRouteV1 { provider: Arc::clone(self), shard, repository })
    }
}

impl GraphGenerationManifestProvider for DaemonCodeGraphManifestProviderV1 {
    fn hydrate_sealed_code_generation(
        &self,
        owner: &GraphProjectionIdentityV1,
        source: &SealedCodeGenerationReplay,
        spill: GraphGenerationRowSpill,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<SpilledGraphGeneration, GraphDbError> {
        check()?;
        let binding = self.sources.read()
            .map_err(|_| GraphDbError::unavailable("code graph manifest registry poisoned"))?
            .get(&owner.shard_id).cloned()
            .ok_or_else(|| GraphDbError::unavailable("sealed code replay source is not mounted"))?;
        if !binding.routes.contains_key(&source.repository)
            || !matches!(&owner.shard_id.scope,
                StoreShardScopeV1::Project { project_id } if project_id == &binding.project_id)
        {
            return Err(GraphDbError::conflict("code_graph_manifest.replay_owner"));
        }
        let digest = ManifestDigest::new(source.sealed_state_digest.as_str())
            .map_err(|error| GraphDbError::invalid(error.to_string()))?;
        let generation = binding.storage.manifest_generation(&source.generation, &digest)
            .map_err(sql_graph_error)?
            .ok_or_else(|| GraphDbError::unavailable("exact registered generation is not retained"))?;
        let seal = open_sql_seal(&binding.storage, &generation, &source.sealed_state_digest, check)?;
        if seal.source.manifest().project_id != binding.project_id
            || seal.source.snapshot().repository != source.repository
        {
            return Err(GraphDbError::conflict("code_graph_manifest.replay_source"));
        }
        spill_verified_seal_graph(
            &seal,
            GraphProjectionIdentity::new(
                GraphNamespace::new(owner.namespace.as_str())?,
                GraphProjectionId::new(owner.projection.as_str())?,
            ),
            &source.projector_revision, spill, &mut |_| Ok(()), check,
        )
    }
}

#[cfg(test)]
mod tests;
