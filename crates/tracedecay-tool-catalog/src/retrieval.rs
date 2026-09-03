use serde::Serialize;

use crate::id::{CapabilityId, RetrieverId, SortContractId};
use crate::manifest::{CancellationPoint, DeadlineBehavior, SchemaRef, canonicalize_set};
use crate::validation::CatalogValidationError;

/// The narrow evidence family served by a retrieval primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalFamily {
    Symbol,
    Source,
    Graph,
    Test,
    Temporal,
    Operational,
}

/// Temporal horizon supported by one bounded primitive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TemporalMode {
    Current,
    AsOf,
    Evolution,
    Forensic,
}

/// Stable sorting semantics used for pagination and concatenation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SortContract {
    sort_contract_id: SortContractId,
    revision: u32,
}

impl SortContract {
    pub fn new(
        sort_contract_id: SortContractId,
        revision: u32,
    ) -> Result<Self, CatalogValidationError> {
        if revision == 0 {
            return Err(CatalogValidationError::InvalidValue {
                field: "sort contract revision",
                reason: "must be greater than zero",
            });
        }
        Ok(Self {
            sort_contract_id,
            revision,
        })
    }

    pub fn sort_contract_id(&self) -> &SortContractId {
        &self.sort_contract_id
    }

    pub const fn revision(&self) -> u32 {
        self.revision
    }
}

macro_rules! packet_contract_ref {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
        pub struct $name {
            schema: SchemaRef,
        }

        impl $name {
            pub fn new(schema: SchemaRef) -> Self {
                Self { schema }
            }

            pub fn schema(&self) -> &SchemaRef {
                &self.schema
            }
        }
    };
}

packet_contract_ref!(CoverageContractRef);
packet_contract_ref!(OmissionContractRef);
packet_contract_ref!(ScoringContractRef);
packet_contract_ref!(ContributionContractRef);

/// Input used to create an immutable retrieval primitive manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetrievalPrimitiveManifestInputV1 {
    pub capability_id: CapabilityId,
    pub family: RetrievalFamily,
    pub retriever_id: RetrieverId,
    pub request_schema: SchemaRef,
    pub evidence_packet_schema: SchemaRef,
    pub coverage_contract: CoverageContractRef,
    pub omission_contract: OmissionContractRef,
    pub scoring_contract: ScoringContractRef,
    pub contribution_contract: ContributionContractRef,
    pub deterministic_order: SortContract,
    pub default_page_size: u32,
    pub maximum_page_size: u32,
    pub temporal_modes: Vec<TemporalMode>,
    pub cancellation_points: Vec<CancellationPoint>,
    pub deadline_behavior: DeadlineBehavior,
}

/// Metadata for one concrete bounded retrieval operation.
///
/// It deliberately has no planner, model, fan-out, dispatcher, or nested
/// invocation field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetrievalPrimitiveManifestV1 {
    capability_id: CapabilityId,
    family: RetrievalFamily,
    retriever_id: RetrieverId,
    request_schema: SchemaRef,
    evidence_packet_schema: SchemaRef,
    coverage_contract: CoverageContractRef,
    omission_contract: OmissionContractRef,
    scoring_contract: ScoringContractRef,
    contribution_contract: ContributionContractRef,
    deterministic_order: SortContract,
    default_page_size: u32,
    maximum_page_size: u32,
    temporal_modes: Vec<TemporalMode>,
    cancellation_points: Vec<CancellationPoint>,
    deadline_behavior: DeadlineBehavior,
}

impl RetrievalPrimitiveManifestV1 {
    pub fn new(input: RetrievalPrimitiveManifestInputV1) -> Result<Self, CatalogValidationError> {
        if input.default_page_size == 0
            || input.maximum_page_size == 0
            || input.default_page_size > input.maximum_page_size
        {
            return Err(CatalogValidationError::InvalidValue {
                field: "retrieval page bounds",
                reason: "default and maximum must be non-zero and ordered",
            });
        }
        if input.temporal_modes.is_empty() {
            return Err(CatalogValidationError::MissingValue {
                field: "retrieval temporal modes",
            });
        }
        if input.cancellation_points.is_empty() {
            return Err(CatalogValidationError::MissingValue {
                field: "retrieval cancellation points",
            });
        }
        if input.deadline_behavior == DeadlineBehavior::ReturnEffectReceipt {
            return Err(CatalogValidationError::InvalidValue {
                field: "retrieval deadline behavior",
                reason: "retrieval primitives cannot return effect receipts",
            });
        }

        let mut temporal_modes = input.temporal_modes;
        let mut cancellation_points = input.cancellation_points;
        canonicalize_set(&mut temporal_modes, "retrieval temporal modes")?;
        canonicalize_set(&mut cancellation_points, "retrieval cancellation points")?;

        Ok(Self {
            capability_id: input.capability_id,
            family: input.family,
            retriever_id: input.retriever_id,
            request_schema: input.request_schema,
            evidence_packet_schema: input.evidence_packet_schema,
            coverage_contract: input.coverage_contract,
            omission_contract: input.omission_contract,
            scoring_contract: input.scoring_contract,
            contribution_contract: input.contribution_contract,
            deterministic_order: input.deterministic_order,
            default_page_size: input.default_page_size,
            maximum_page_size: input.maximum_page_size,
            temporal_modes,
            cancellation_points,
            deadline_behavior: input.deadline_behavior,
        })
    }

    pub fn capability_id(&self) -> &CapabilityId {
        &self.capability_id
    }

    pub const fn family(&self) -> RetrievalFamily {
        self.family
    }

    pub fn retriever_id(&self) -> &RetrieverId {
        &self.retriever_id
    }

    pub fn request_schema(&self) -> &SchemaRef {
        &self.request_schema
    }

    pub fn evidence_packet_schema(&self) -> &SchemaRef {
        &self.evidence_packet_schema
    }

    pub fn coverage_contract(&self) -> &CoverageContractRef {
        &self.coverage_contract
    }

    pub fn omission_contract(&self) -> &OmissionContractRef {
        &self.omission_contract
    }

    pub fn scoring_contract(&self) -> &ScoringContractRef {
        &self.scoring_contract
    }

    pub fn contribution_contract(&self) -> &ContributionContractRef {
        &self.contribution_contract
    }

    pub fn deterministic_order(&self) -> &SortContract {
        &self.deterministic_order
    }

    pub const fn default_page_size(&self) -> u32 {
        self.default_page_size
    }

    pub const fn maximum_page_size(&self) -> u32 {
        self.maximum_page_size
    }

    pub fn temporal_modes(&self) -> &[TemporalMode] {
        &self.temporal_modes
    }

    pub fn cancellation_points(&self) -> &[CancellationPoint] {
        &self.cancellation_points
    }

    pub const fn deadline_behavior(&self) -> DeadlineBehavior {
        self.deadline_behavior
    }

    pub fn schema_refs(&self) -> [&SchemaRef; 6] {
        [
            &self.request_schema,
            &self.evidence_packet_schema,
            self.coverage_contract.schema(),
            self.omission_contract.schema(),
            self.scoring_contract.schema(),
            self.contribution_contract.schema(),
        ]
    }
}
