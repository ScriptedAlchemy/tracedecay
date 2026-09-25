//! Incident-debris ownership (Plan 38 §5).
//!
//! Recovery and corruption artifacts (`*.corrupt-*`, `*.corrupt`,
//! `*.recovered*`, `recovery-*`) accumulate as loose siblings of live stores with no owner
//! surface. This module gives them a typed classifier and a scan read model
//! that a Doctor producer turns into an `IncidentDebrisPresent` finding. It
//! performs no filesystem effect: detection consumes already-listed file names,
//! and retention deletes classified debris directly.

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::error::ApplicationContractError;

use super::identity::{RelativeArtifactPathV1, StorageByteSizeV1, StoreKeyV1};

/// The class of incident artifact a debris file represents.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDebrisKindV1 {
    /// A `*.corrupt-*` sibling: a store copied aside after corruption
    /// detection.
    Corrupt,
    /// A `*.recovered*` sibling: the output of a recovery pass.
    Recovered,
    /// A `recovery-*` sibling: a recovery working/scratch artifact.
    RecoveryScratch,
}

impl IncidentDebrisKindV1 {
    /// Classify a store-sibling file name into a debris kind, or `None` if the
    /// name is not recognized incident debris.
    ///
    /// Matching is deliberately narrow so a live store (`sessions.db`,
    /// `sessions.db-wal`, `sessions.db-shm`) is never misclassified as debris.
    /// The patterns mirror the measured evidence: `*.corrupt-*`,
    /// `*.recovered*`, and `recovery-*`.
    #[must_use]
    pub fn classify(file_name: &str) -> Option<Self> {
        // `recovery-*` scratch: prefix match, but not the bare word.
        if file_name.starts_with("recovery-") && file_name.len() > "recovery-".len() {
            return Some(Self::RecoveryScratch);
        }
        // `*.corrupt-<suffix>`: a `.corrupt-` segment somewhere in the name.
        if file_name.contains(".corrupt-") {
            return Some(Self::Corrupt);
        }
        // `*.recovered*`: a `.recovered` segment somewhere in the name.
        if file_name.contains(".recovered") {
            return Some(Self::Recovered);
        }
        None
    }
}

/// One detected incident-debris artifact sitting beside a live store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IncidentDebrisArtifactV1 {
    /// The store the artifact is a sibling of.
    pub store: StoreKeyV1,
    /// The store-relative path of the artifact.
    pub path: RelativeArtifactPathV1,
    pub kind: IncidentDebrisKindV1,
    pub size_bytes: StorageByteSizeV1,
    pub observed_at: UtcMicros,
}

impl IncidentDebrisArtifactV1 {
    /// Build an artifact by classifying `path`'s file name. Returns `Ok(None)`
    /// when the name is not incident debris, so a directory scan can map over
    /// every sibling without pre-filtering.
    pub fn classify_path(
        store: StoreKeyV1,
        path: RelativeArtifactPathV1,
        size_bytes: StorageByteSizeV1,
        observed_at: UtcMicros,
    ) -> Result<Option<Self>, ApplicationContractError> {
        let file_name = path.as_str().rsplit('/').next().unwrap_or(path.as_str());
        Ok(IncidentDebrisKindV1::classify(file_name).map(|kind| Self {
            store,
            path,
            kind,
            size_bytes,
            observed_at,
        }))
    }
}

/// The read model of one debris scan over a store's siblings.
///
/// A scan is *complete* when every sibling was listed and classified; it is
/// *partial* when the listing was truncated or a subdirectory was skipped. This
/// completeness flows into the Doctor coverage statement so an incomplete scan
/// can never assert a clean result.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct IncidentDebrisScanV1 {
    pub store: StoreKeyV1,
    pub artifacts: Vec<IncidentDebrisArtifactV1>,
    /// Whether the sibling listing was exhaustive.
    pub listing_complete: bool,
}

impl IncidentDebrisScanV1 {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.artifacts.is_empty()
    }

    #[must_use]
    pub fn artifact_count(&self) -> usize {
        self.artifacts.len()
    }

    /// Total bytes of all detected debris artifacts (saturating).
    #[must_use]
    pub fn total_bytes(&self) -> StorageByteSizeV1 {
        let total = self.artifacts.iter().fold(0u64, |acc, artifact| {
            acc.saturating_add(artifact.size_bytes.get())
        });
        StorageByteSizeV1(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> StoreKeyV1 {
        StoreKeyV1::new("sessions.db").expect("valid")
    }

    #[test]
    fn classifier_matches_each_debris_pattern() {
        assert_eq!(
            IncidentDebrisKindV1::classify("sessions.db.corrupt-1721692800"),
            Some(IncidentDebrisKindV1::Corrupt)
        );
        assert_eq!(
            IncidentDebrisKindV1::classify("graph.db.recovered"),
            Some(IncidentDebrisKindV1::Recovered)
        );
        assert_eq!(
            IncidentDebrisKindV1::classify("graph.db.recovered-2"),
            Some(IncidentDebrisKindV1::Recovered)
        );
        assert_eq!(
            IncidentDebrisKindV1::classify("recovery-scratch-42.tmp"),
            Some(IncidentDebrisKindV1::RecoveryScratch)
        );
    }

    #[test]
    fn classifier_never_flags_live_store_files() {
        for name in [
            "sessions.db",
            "sessions.db-wal",
            "sessions.db-shm",
            "recovery-",
        ] {
            assert_eq!(IncidentDebrisKindV1::classify(name), None, "{name}");
        }
    }

    #[test]
    fn scan_totals_bytes_and_reports_emptiness() {
        let path = RelativeArtifactPathV1::new("sessions.db.corrupt-9").expect("valid");
        let artifact = IncidentDebrisArtifactV1::classify_path(
            store(),
            path,
            StorageByteSizeV1(700),
            UtcMicros(1),
        )
        .expect("ok")
        .expect("debris");
        let scan = IncidentDebrisScanV1 {
            store: store(),
            artifacts: vec![artifact],
            listing_complete: true,
        };
        assert!(!scan.is_empty());
        assert_eq!(scan.total_bytes(), StorageByteSizeV1(700));

        let empty = IncidentDebrisScanV1 {
            store: store(),
            artifacts: Vec::new(),
            listing_complete: true,
        };
        assert!(empty.is_empty());
        assert_eq!(empty.total_bytes(), StorageByteSizeV1::ZERO);
    }
}
