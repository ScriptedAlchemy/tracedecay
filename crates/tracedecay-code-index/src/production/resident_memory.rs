use std::mem::size_of;

use serde::{Deserialize, de::IgnoredAny};
use tracedecay_domain::{
    CanonicalRelationEdgeV1, CodeGenerationId, CodeSearchChunkV1, SymbolLineageCandidateV1,
};

use super::{
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, FileGenerationArtifactsV1,
};
use crate::chunks::CodeIndexEdgeAbstentionV1;
use crate::lineage::LineageSymbolRecordV1;

#[derive(Deserialize)]
struct SealedPublishedGenerationResidentProbeV1 {
    generation: PersistedPublishedGenerationResidentProbeV1,
}

#[derive(Deserialize)]
struct PersistedPublishedGenerationResidentProbeV1 {
    manifest: CodeGenerationManifestResidentProbeV1,
    files: Vec<PersistedFileGenerationResidentProbeV1>,
    lineage: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct CodeGenerationManifestResidentProbeV1 {
    generation_id: CodeGenerationId,
}

#[derive(Deserialize)]
struct PersistedFileGenerationResidentProbeV1 {
    artifacts: CodeFileIndexArtifactsResidentProbeV1,
}

#[derive(Deserialize)]
struct CodeFileIndexArtifactsResidentProbeV1 {
    chunks: CodeFileChunksResidentProbeV1,
    symbols: Vec<IgnoredAny>,
    edges: Vec<IgnoredAny>,
    edge_abstentions: Vec<IgnoredAny>,
}

#[derive(Deserialize)]
struct CodeFileChunksResidentProbeV1 {
    chunks: Vec<IgnoredAny>,
}

/// Authenticated allocation counts used to reserve memory before a sealed
/// generation is decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedGenerationResidentEstimateV1 {
    generation_id: CodeGenerationId,
    reservation_bytes: u64,
}

impl SealedGenerationResidentEstimateV1 {
    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation_id
    }

    pub const fn reservation_bytes(&self) -> u64 {
        self.reservation_bytes
    }
}

fn resident_count(values: &[IgnoredAny]) -> Result<u64, CodeIndexProductionErrorV1> {
    u64::try_from(values.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation resident count exceeds u64".to_owned(),
        )
    })
}

fn resident_capacity_upper_bound(count: u64) -> Result<u64, CodeIndexProductionErrorV1> {
    if count == 0 {
        return Ok(0);
    }
    count.checked_next_power_of_two().ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation resident capacity exceeds u64".to_owned(),
        )
    })
}

fn resident_allocation_upper_bound<T>(count: u64) -> Result<u64, CodeIndexProductionErrorV1> {
    resident_capacity_upper_bound(count)?
        .checked_mul(u64::try_from(size_of::<T>()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation resident element size exceeds u64".to_owned(),
            )
        })?)
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation resident allocation exceeds u64".to_owned(),
            )
        })
}

fn resident_allocation_bytes<T>(values: &Vec<T>) -> Result<u64, CodeIndexProductionErrorV1> {
    u64::try_from(values.capacity())
        .ok()
        .and_then(|capacity| {
            u64::try_from(size_of::<T>())
                .ok()
                .and_then(|element| capacity.checked_mul(element))
        })
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "decoded generation resident allocation exceeds u64".to_owned(),
            )
        })
}

fn resident_add(total: &mut u64, bytes: u64) -> Result<(), CodeIndexProductionErrorV1> {
    *total = total.checked_add(bytes).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation resident estimate exceeds u64".to_owned(),
        )
    })?;
    Ok(())
}

impl CodeIndexPublishedGenerationV1 {
    /// Conservative charge retained for a streamed sealed-generation restore.
    ///
    /// The sealed encoding contains every dynamic string and byte body. Six
    /// complete payload widths cover the decoded envelope, canonical chunk /
    /// symbol / edge copies, exact-authority copies, and decode scratch. The
    /// fixed allowance covers bounded parser buffering and small-container
    /// backing. This is intentionally retained as overcharge: allocator and
    /// map overhead are opaque and must not be described as exact measurement.
    pub fn sealed_resident_memory_upper_bound(
        sealed_bytes: u64,
    ) -> Result<u64, CodeIndexProductionErrorV1> {
        const RETAINED_PAYLOAD_WIDTHS: u64 = 6;
        const FIXED_STREAMING_ALLOWANCE_BYTES: u64 = 8 * 1024 * 1024;
        sealed_bytes
            .checked_mul(RETAINED_PAYLOAD_WIDTHS)
            .and_then(|bytes| bytes.checked_add(FIXED_STREAMING_ALLOWANCE_BYTES))
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation resident upper bound exceeds u64".to_owned(),
                )
            })
    }

    /// Reserve an upper bound before materializing one authenticated sealed
    /// generation.
    ///
    /// The probe ignores content-bearing values and retains only allocation
    /// counts. Four sealed-payload widths cover decoded scalar/string bodies,
    /// their canonical copies, and decode scratch. Vector backing stores are
    /// charged separately at their geometric-growth upper bound, including the
    /// canonical chunk/symbol/edge copies reconstructed during restore.
    pub fn sealed_resident_memory_estimate(
        bytes: &[u8],
    ) -> Result<SealedGenerationResidentEstimateV1, CodeIndexProductionErrorV1> {
        let probe: SealedPublishedGenerationResidentProbeV1 = serde_json::from_slice(bytes)
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation resident probe failed: {error}"
                ))
            })?;
        let file_count = u64::try_from(probe.generation.files.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation file count exceeds u64".to_owned(),
            )
        })?;
        let lineage_count = resident_count(&probe.generation.lineage)?;
        let mut chunk_count = 0_u64;
        let mut symbol_count = 0_u64;
        let mut edge_count = 0_u64;
        let mut abstention_count = 0_u64;
        for file in &probe.generation.files {
            resident_add(
                &mut chunk_count,
                resident_count(&file.artifacts.chunks.chunks)?,
            )?;
            resident_add(&mut symbol_count, resident_count(&file.artifacts.symbols)?)?;
            resident_add(&mut edge_count, resident_count(&file.artifacts.edges)?)?;
            resident_add(
                &mut abstention_count,
                resident_count(&file.artifacts.edge_abstentions)?,
            )?;
        }

        let sealed_bytes = u64::try_from(bytes.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation byte length exceeds u64".to_owned(),
            )
        })?;
        let mut reservation_bytes = sealed_bytes.checked_mul(4).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation resident estimate exceeds u64".to_owned(),
            )
        })?;
        resident_add(
            &mut reservation_bytes,
            resident_allocation_upper_bound::<FileGenerationArtifactsV1>(file_count)?,
        )?;
        for _ in 0..2 {
            resident_add(
                &mut reservation_bytes,
                resident_allocation_upper_bound::<CodeSearchChunkV1>(chunk_count)?,
            )?;
            resident_add(
                &mut reservation_bytes,
                resident_allocation_upper_bound::<LineageSymbolRecordV1>(symbol_count)?,
            )?;
            resident_add(
                &mut reservation_bytes,
                resident_allocation_upper_bound::<CanonicalRelationEdgeV1>(edge_count)?,
            )?;
            resident_add(
                &mut reservation_bytes,
                resident_allocation_upper_bound::<CodeIndexEdgeAbstentionV1>(abstention_count)?,
            )?;
        }
        resident_add(
            &mut reservation_bytes,
            resident_allocation_upper_bound::<SymbolLineageCandidateV1>(lineage_count)?,
        )?;
        Ok(SealedGenerationResidentEstimateV1 {
            generation_id: probe.generation.manifest.generation_id,
            reservation_bytes,
        })
    }

    /// Exact charge under the decoded-generation structural model.
    ///
    /// Unlike the preflight estimate, this reads every retained vector's actual
    /// capacity. Three sealed-payload widths conservatively cover dynamic
    /// scalar/string bodies and their canonical generation copies.
    pub fn structural_resident_memory_bytes(
        &self,
        sealed_bytes: u64,
    ) -> Result<u64, CodeIndexProductionErrorV1> {
        let mut measured_bytes = sealed_bytes.checked_mul(3).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "decoded generation resident measurement exceeds u64".to_owned(),
            )
        })?;
        resident_add(&mut measured_bytes, resident_allocation_bytes(&self.files)?)?;
        for file in &self.files {
            resident_add(
                &mut measured_bytes,
                resident_allocation_bytes(&file.artifacts.chunks.chunks)?,
            )?;
            resident_add(
                &mut measured_bytes,
                resident_allocation_bytes(&file.artifacts.symbols)?,
            )?;
            resident_add(
                &mut measured_bytes,
                resident_allocation_bytes(&file.artifacts.edges)?,
            )?;
            resident_add(
                &mut measured_bytes,
                resident_allocation_bytes(&file.artifacts.edge_abstentions)?,
            )?;
        }
        let chunks = self.chunks.shared_chunks();
        resident_add(
            &mut measured_bytes,
            resident_allocation_bytes(chunks.as_ref())?,
        )?;
        resident_add(
            &mut measured_bytes,
            resident_allocation_bytes(&self.symbols.symbols)?,
        )?;
        resident_add(
            &mut measured_bytes,
            resident_allocation_bytes(self.edges.as_ref())?,
        )?;
        resident_add(
            &mut measured_bytes,
            resident_allocation_bytes(&self.edge_abstentions)?,
        )?;
        resident_add(
            &mut measured_bytes,
            resident_allocation_bytes(&self.lineage)?,
        )?;
        Ok(measured_bytes)
    }
}
