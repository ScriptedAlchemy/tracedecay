//! Incident-debris ownership (Plan 38 §5).
//!
//! Recovery and corruption artifacts (`*.corrupt-*`, `*.corrupt`,
//! `*.recovered*`, `recovery-*`) accumulate as loose siblings of live stores with no owner
//! surface. This module gives them a typed classifier, a single quarantine
//! location contract with metadata, and a scan read model that a Doctor producer
//! turns into an `IncidentDebrisPresent` finding. It performs no filesystem
//! effect: detection consumes already-listed file names, and quarantine is a
//! declarative record the owning storage operation later enacts.

use serde::{Deserialize, Serialize};
use tracedecay_domain::UtcMicros;

use crate::error::ApplicationContractError;

use super::identity::{
    QuarantineLocationV1, RelativeArtifactPathV1, StorageByteSizeV1, StoreKeyV1,
};

/// The class of incident artifact a debris file represents.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum IncidentDebrisKindV1 {
    /// A `*.corrupt-*` or `*.corrupt` sibling: a store copied aside after
    /// corruption detection.
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
    /// The patterns mirror the measured evidence: `*.corrupt-*`, `*.corrupt`,
    /// `*.recovered*`, and `recovery-*`.
    #[must_use]
    pub fn classify(file_name: &str) -> Option<Self> {
        // `recovery-*` scratch: prefix match, but not the bare word.
        if file_name.starts_with("recovery-") && file_name.len() > "recovery-".len() {
            return Some(Self::RecoveryScratch);
        }
        // `*.corrupt-<suffix>`: a `.corrupt-` segment somewhere in the name.
        // The bare `*.corrupt` suffix is the same artifact from an older
        // quarantine naming convention; profiles upgraded across that change
        // still carry it, and no live store name ends in `.corrupt`.
        if file_name.contains(".corrupt-") || file_name.ends_with(".corrupt") {
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

/// The single quarantine location debris is collected into, with metadata.
///
/// Plan 38 §5 requires recovery/corruption artifacts to be written into one
/// quarantined location with metadata, surfaced by Doctor and collected by the
/// retention machinery — never left as loose siblings. This contract names that
/// location (store-relative) and the retention window after which quarantined
/// artifacts become collection-eligible.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuarantineContractV1 {
    /// The single store-relative directory debris is moved into.
    pub location: QuarantineLocationV1,
    /// Micros after which a quarantined artifact is collection-eligible.
    pub retention_window_micros: i64,
}

impl QuarantineContractV1 {
    /// Validate the contract. The retention window must be positive; a
    /// non-positive window would make every artifact instantly collectible,
    /// defeating the owner-visible retention guarantee.
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if self.retention_window_micros <= 0 {
            return Err(ApplicationContractError::ZeroValue {
                field: "quarantine retention window",
            });
        }
        Ok(())
    }

    /// Declare the quarantined placement for an artifact. This is a record, not
    /// a move: the owning storage operation enacts the relocation and honors the
    /// window. The eligibility time is `quarantined_at + retention_window`.
    pub fn quarantine(
        &self,
        artifact: IncidentDebrisArtifactV1,
        quarantined_at: UtcMicros,
    ) -> Result<QuarantinedArtifactV1, ApplicationContractError> {
        self.validate()?;
        Ok(QuarantinedArtifactV1 {
            collection_eligible_at: UtcMicros(
                quarantined_at
                    .0
                    .saturating_add(self.retention_window_micros),
            ),
            location: self.location.clone(),
            artifact,
            quarantined_at,
        })
    }
}

/// An artifact declared into the quarantine location with its collection window.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QuarantinedArtifactV1 {
    pub artifact: IncidentDebrisArtifactV1,
    pub location: QuarantineLocationV1,
    pub quarantined_at: UtcMicros,
    pub collection_eligible_at: UtcMicros,
}

impl QuarantinedArtifactV1 {
    /// True when `now` has reached the collection-eligibility watermark.
    #[must_use]
    pub fn is_collection_eligible(&self, now: UtcMicros) -> bool {
        now.0 >= self.collection_eligible_at.0
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

    /// The pre-timestamp quarantine naming an upgraded profile still carries.
    #[test]
    fn classifier_matches_bare_corrupt_suffix() {
        assert_eq!(
            IncidentDebrisKindV1::classify("tracedecay.db.corrupt"),
            Some(IncidentDebrisKindV1::Corrupt)
        );
        assert_eq!(
            IncidentDebrisKindV1::classify("sessions.db.corrupt"),
            Some(IncidentDebrisKindV1::Corrupt)
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
    fn classify_path_uses_basename() {
        let path = RelativeArtifactPathV1::new("nested/sessions.db.corrupt-9").expect("valid");
        let artifact = IncidentDebrisArtifactV1::classify_path(
            store(),
            path,
            StorageByteSizeV1(10),
            UtcMicros(1),
        )
        .expect("ok")
        .expect("debris");
        assert_eq!(artifact.kind, IncidentDebrisKindV1::Corrupt);
    }

    #[test]
    fn classify_path_returns_none_for_live_file() {
        let path = RelativeArtifactPathV1::new("sessions.db").expect("valid");
        assert!(
            IncidentDebrisArtifactV1::classify_path(
                store(),
                path,
                StorageByteSizeV1(10),
                UtcMicros(1)
            )
            .expect("ok")
            .is_none()
        );
    }

    #[test]
    fn quarantine_computes_eligibility_and_rejects_nonpositive_window() {
        let location = QuarantineLocationV1::new("quarantine").expect("valid");
        let contract = QuarantineContractV1 {
            location,
            retention_window_micros: 1_000,
        };
        let path = RelativeArtifactPathV1::new("sessions.db.corrupt-9").expect("valid");
        let artifact = IncidentDebrisArtifactV1::classify_path(
            store(),
            path,
            StorageByteSizeV1(10),
            UtcMicros(1),
        )
        .expect("ok")
        .expect("debris");
        let quarantined = contract
            .quarantine(artifact, UtcMicros(500))
            .expect("quarantined");
        assert_eq!(quarantined.collection_eligible_at, UtcMicros(1_500));
        assert!(!quarantined.is_collection_eligible(UtcMicros(1_499)));
        assert!(quarantined.is_collection_eligible(UtcMicros(1_500)));

        let bad = QuarantineContractV1 {
            location: QuarantineLocationV1::new("quarantine").expect("valid"),
            retention_window_micros: 0,
        };
        assert!(bad.validate().is_err());
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
