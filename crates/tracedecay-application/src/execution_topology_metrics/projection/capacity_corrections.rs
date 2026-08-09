use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use tracedecay_domain::canonical_sha256;

use super::capacity_rollup::ExecutionTopologyCapacityRollupV1;
use super::{
    ConflictOutcomeRowV1, ConflictPredictionRowV1, DuplicateReceiptKeyV1, DuplicateRowV1,
    ExecutionTopologyEvidenceV1, ExecutionTopologyRollupStateErrorV1,
};

pub(in crate::execution_topology_metrics) const MAX_CAPACITY_CORRECTION_CARRY_V1: usize = 512;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(in crate::execution_topology_metrics) struct ExecutionTopologyCapacityCorrectionCarryV1 {
    candidates: Vec<ExecutionTopologyCapacityCorrectionCandidateV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ExecutionTopologyCapacityCorrectionCandidateV1 {
    Duplicate {
        reference: String,
        revision: u64,
        event_time_micros: i64,
        row: Option<DuplicateRowV1>,
    },
    Prediction {
        reference: String,
        row: ConflictPredictionRowV1,
    },
    Outcome {
        reference: String,
        row: ConflictOutcomeRowV1,
    },
}

impl ExecutionTopologyEvidenceV1 {
    pub(in crate::execution_topology_metrics) fn reduce_capacity_correction_carry(
        &self,
    ) -> Result<ExecutionTopologyCapacityCorrectionCarryV1, ExecutionTopologyRollupStateErrorV1>
    {
        let mut carry = ExecutionTopologyCapacityCorrectionCarryV1::default();
        for ((reference, revision), (row, event_time_micros)) in &self.duplicates {
            carry.push(ExecutionTopologyCapacityCorrectionCandidateV1::Duplicate {
                reference: protected_reference("execution-topology.duplicate", reference)?,
                revision: *revision,
                event_time_micros: *event_time_micros,
                row: *row,
            })?;
        }
        for (reference, row) in &self.predictions {
            carry.push(ExecutionTopologyCapacityCorrectionCandidateV1::Prediction {
                reference: protected_reference("execution-topology.conflict", reference)?,
                row: row.clone(),
            })?;
        }
        for (reference, row) in &self.outcomes {
            carry.push(ExecutionTopologyCapacityCorrectionCandidateV1::Outcome {
                reference: protected_reference("execution-topology.conflict", reference)?,
                row: *row,
            })?;
        }
        carry.canonicalize()?;
        Ok(carry)
    }
}

fn protected_reference(
    domain: &str,
    reference: &str,
) -> Result<String, ExecutionTopologyRollupStateErrorV1> {
    canonical_sha256(&(domain, reference))
        .map(|digest| digest.as_str().to_owned())
        .map_err(|_| ExecutionTopologyRollupStateErrorV1::IncompatibleState)
}

impl ExecutionTopologyCapacityCorrectionCarryV1 {
    pub(in crate::execution_topology_metrics) fn validate(
        &self,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        if self.candidates.len() > MAX_CAPACITY_CORRECTION_CARRY_V1
            || self.candidates.iter().any(|candidate| {
                let (reference, _) = candidate.reference_and_time();
                !protected_reference_is_valid(reference)
                    || matches!(
                        candidate,
                        ExecutionTopologyCapacityCorrectionCandidateV1::Duplicate {
                            revision: 0,
                            ..
                        }
                    )
                    || matches!(
                        candidate,
                        ExecutionTopologyCapacityCorrectionCandidateV1::Duplicate {
                            event_time_micros,
                            row: Some(row),
                            ..
                        } if *event_time_micros != row.event_time_micros
                    )
            })
        {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        let mut canonical = self.clone();
        canonical.canonicalize()?;
        if canonical != *self {
            return Err(ExecutionTopologyRollupStateErrorV1::IncompatibleState);
        }
        Ok(())
    }

    pub(in crate::execution_topology_metrics) fn event_times_within(
        &self,
        since_micros: i64,
        until_micros: i64,
    ) -> bool {
        self.candidates.iter().all(|candidate| {
            let (_, event_time) = candidate.reference_and_time();
            event_time >= since_micros && event_time < until_micros
        })
    }

    pub(in crate::execution_topology_metrics) fn merge(
        &mut self,
        mut other: Self,
    ) -> Result<u64, ExecutionTopologyRollupStateErrorV1> {
        if self.candidates.len().saturating_add(other.candidates.len())
            > MAX_CAPACITY_CORRECTION_CARRY_V1
        {
            return Err(ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded);
        }
        let conflicts_before = self.invalid_candidate_group_count()?;
        let incoming_conflicts = other.invalid_candidate_group_count()?;
        self.candidates.append(&mut other.candidates);
        self.canonicalize()?;
        Ok(self
            .invalid_candidate_group_count()?
            .saturating_sub(conflicts_before.saturating_add(incoming_conflicts)))
    }

    fn push(
        &mut self,
        candidate: ExecutionTopologyCapacityCorrectionCandidateV1,
    ) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        if self.candidates.len() == MAX_CAPACITY_CORRECTION_CARRY_V1 {
            return Err(ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded);
        }
        self.candidates.push(candidate);
        Ok(())
    }

    fn canonicalize(&mut self) -> Result<(), ExecutionTopologyRollupStateErrorV1> {
        let mut encoded = Vec::with_capacity(self.candidates.len());
        for candidate in std::mem::take(&mut self.candidates) {
            let canonical = serde_json::to_string(&candidate)
                .map_err(|_| ExecutionTopologyRollupStateErrorV1::IncompatibleState)?;
            encoded.push((canonical, candidate));
        }
        encoded.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        self.candidates = encoded
            .into_iter()
            .map(|(_, candidate)| candidate)
            .collect();
        Ok(())
    }

    fn invalid_candidate_group_count(&self) -> Result<u64, ExecutionTopologyRollupStateErrorV1> {
        let mut predictions = BTreeMap::<&str, &ConflictPredictionRowV1>::new();
        let mut outcomes = BTreeMap::<(&str, u64), &ConflictOutcomeRowV1>::new();
        let mut invalid = BTreeSet::<(&str, u64, u8)>::new();
        for candidate in &self.candidates {
            match candidate {
                ExecutionTopologyCapacityCorrectionCandidateV1::Duplicate { .. } => {}
                ExecutionTopologyCapacityCorrectionCandidateV1::Prediction { reference, row } => {
                    match predictions.insert(reference, row) {
                        Some(existing) if existing != row => {
                            invalid.insert((reference, 0, 0));
                        }
                        _ => {}
                    }
                }
                ExecutionTopologyCapacityCorrectionCandidateV1::Outcome { reference, row } => {
                    match outcomes.insert((reference, u64::from(row.correction_revision)), row) {
                        Some(existing) if existing != row => {
                            invalid.insert((reference, u64::from(row.correction_revision), 1));
                        }
                        _ => {}
                    }
                }
            }
        }
        u64::try_from(invalid.len())
            .map_err(|_| ExecutionTopologyRollupStateErrorV1::IncompatibleState)
    }
}

fn protected_reference_is_valid(reference: &str) -> bool {
    reference.len() == 71
        && reference.starts_with("sha256:")
        && reference[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl ExecutionTopologyCapacityRollupV1 {
    pub(in crate::execution_topology_metrics) fn with_carry_applied(
        &self,
        carry: &ExecutionTopologyCapacityCorrectionCarryV1,
    ) -> Result<Self, ExecutionTopologyRollupStateErrorV1> {
        if carry.candidates.len() > MAX_CAPACITY_CORRECTION_CARRY_V1 {
            return Err(ExecutionTopologyRollupStateErrorV1::CarryBudgetExceeded);
        }
        let mut combined = self.clone();
        apply_candidates(&mut combined, &carry.candidates);
        Ok(combined)
    }
}

impl ExecutionTopologyCapacityCorrectionCandidateV1 {
    fn reference_and_time(&self) -> (&str, i64) {
        match self {
            Self::Duplicate {
                reference,
                event_time_micros,
                ..
            } => (reference, *event_time_micros),
            Self::Prediction { reference, row } => (reference, row.event_time_micros),
            Self::Outcome { reference, row } => (reference, row.event_time_micros),
        }
    }
}

fn apply_candidates(
    capacity: &mut ExecutionTopologyCapacityRollupV1,
    candidates: &[ExecutionTopologyCapacityCorrectionCandidateV1],
) {
    let mut duplicates = BTreeMap::<DuplicateReceiptKeyV1, (Option<DuplicateRowV1>, i64)>::new();
    let mut predictions = BTreeMap::<String, ConflictPredictionRowV1>::new();
    let mut outcomes = BTreeMap::<String, ConflictOutcomeRowV1>::new();
    for candidate in candidates {
        match candidate {
            ExecutionTopologyCapacityCorrectionCandidateV1::Duplicate {
                reference,
                revision,
                row,
                event_time_micros,
            } => absorb_duplicate_candidate(
                &mut duplicates,
                reference,
                *revision,
                *event_time_micros,
                *row,
            ),
            ExecutionTopologyCapacityCorrectionCandidateV1::Prediction { reference, row } => {
                predictions
                    .entry(reference.clone())
                    .or_insert_with(|| row.clone());
            }
            ExecutionTopologyCapacityCorrectionCandidateV1::Outcome { reference, row } => {
                absorb_outcome_candidate(&mut outcomes, reference, *row);
            }
        }
    }
    capacity.absorb_duplicate_rows(&duplicates);
    capacity.absorb_conflict_rows(&predictions, &outcomes);
}

fn absorb_duplicate_candidate(
    target: &mut BTreeMap<DuplicateReceiptKeyV1, (Option<DuplicateRowV1>, i64)>,
    reference: &str,
    revision: u64,
    event_time_micros: i64,
    row: Option<DuplicateRowV1>,
) {
    let receipt = (reference.to_owned(), revision);
    match target.get_mut(&receipt) {
        Some((existing, existing_time)) => {
            *existing_time = (*existing_time).max(event_time_micros);
            if *existing != row {
                *existing = None;
            }
        }
        None => {
            target.insert(receipt, (row, event_time_micros));
        }
    }
}

fn absorb_outcome_candidate(
    target: &mut BTreeMap<String, ConflictOutcomeRowV1>,
    reference: &str,
    row: ConflictOutcomeRowV1,
) {
    match target.get(reference) {
        Some(existing) if existing.correction_revision > row.correction_revision => {}
        Some(existing) if existing.correction_revision == row.correction_revision => {}
        _ => {
            target.insert(reference.to_owned(), row);
        }
    }
}
