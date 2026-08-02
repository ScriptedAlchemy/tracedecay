//! Identity-only inventory of published vector-generation state.
//!
//! This module names which code generations are still named by a readable
//! vector-generation entry, without ever exposing vector bytes to Rust. That
//! identity set is the shared liveness authority for code-generation
//! retention (a source generation still read by a vector generation must not
//! be garbage-collected) and for Doctor's exact collectable-byte accounting.
//! It carries no format-migration or whole-corpus rebuild logic: fresh vector
//! stores are created directly at the current row-per-vector, slice-
//! externalized shape.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracedecay_domain::{CodeGenerationId, ManifestDigest, VectorGenerationIdV1, canonical_sha256};

const LEGACY_MIGRATION_INVENTORY_DOMAIN_V1: &str =
    "tracedecay.semantic-code.legacy-vector-inventory.v1";

/// Read-only identity inventory. Deliberately contains no legacy vector bytes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LegacyVectorInventoryV1 {
    pub expected_active_generation: Option<VectorGenerationIdV1>,
    pub entries: Vec<LegacyVectorInventoryEntryV1>,
}

impl LegacyVectorInventoryV1 {
    pub fn canonical_digest(&self) -> Result<ManifestDigest, LegacyVectorMigrationErrorV1> {
        let mut canonical = self.clone();
        canonical
            .entries
            .sort_by(|left, right| left.legacy_generation().cmp(right.legacy_generation()));
        validate_inventory(&canonical)?;
        canonical_sha256(&(
            LEGACY_MIGRATION_INVENTORY_DOMAIN_V1,
            &canonical.expected_active_generation,
            &canonical.entries,
        ))
        .map_err(|error| LegacyVectorMigrationErrorV1::CanonicalCode(error.to_string()))
    }

    /// Code generations still named by readable vector inventory entries.
    ///
    /// This is the shared liveness authority for code-generation retention
    /// and Doctor's exact collectable-byte reading.
    pub fn retained_readable_sources(&self) -> BTreeSet<CodeGenerationId> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                LegacyVectorInventoryEntryV1::Readable {
                    source_generation, ..
                } => Some(source_generation.clone()),
                LegacyVectorInventoryEntryV1::Unreadable { .. } => None,
            })
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "readability", rename_all = "snake_case")]
pub enum LegacyVectorInventoryEntryV1 {
    Readable {
        legacy_generation: VectorGenerationIdV1,
        source_generation: CodeGenerationId,
    },
    Unreadable {
        legacy_generation: VectorGenerationIdV1,
        reason_digest: ManifestDigest,
    },
}

impl LegacyVectorInventoryEntryV1 {
    fn legacy_generation(&self) -> &VectorGenerationIdV1 {
        match self {
            Self::Readable {
                legacy_generation, ..
            }
            | Self::Unreadable {
                legacy_generation, ..
            } => legacy_generation,
        }
    }
}

pub trait LegacyVectorInventoryPortV1 {
    fn read_only_inventory(&self) -> Result<LegacyVectorInventoryV1, LegacyVectorMigrationErrorV1>;
}

fn validate_inventory(
    inventory: &LegacyVectorInventoryV1,
) -> Result<(), LegacyVectorMigrationErrorV1> {
    let generations = inventory
        .entries
        .iter()
        .map(LegacyVectorInventoryEntryV1::legacy_generation)
        .collect::<BTreeSet<_>>();
    if generations.len() != inventory.entries.len() {
        return Err(LegacyVectorMigrationErrorV1::DuplicateLegacyGeneration);
    }
    if inventory
        .expected_active_generation
        .as_ref()
        .is_some_and(|active| !generations.contains(active))
    {
        return Err(LegacyVectorMigrationErrorV1::DanglingActivePointer);
    }
    Ok(())
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LegacyVectorMigrationErrorV1 {
    #[error("legacy vector inventory failed: {0}")]
    Inventory(String),
    #[error("legacy vector inventory contains duplicate generation identity")]
    DuplicateLegacyGeneration,
    #[error("legacy vector inventory active pointer is absent from the inventory")]
    DanglingActivePointer,
    #[error("canonical retained code is invalid: {0}")]
    CanonicalCode(String),
}
