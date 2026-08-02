use std::io::Write;

use serde::Serialize;
use tracedecay_domain::{
    CompactContextBundleV1, CompactContextOmissionV1, CompactContextRecordV1,
    ContextOmissionReasonV1, HydrationStateV1, RetrievalGrainV1,
};

use super::super::ports::ExecutionControl;
use super::super::resolution::summary::SummaryOmission;
use super::assembly::{try_reserve, validate_bundle};
use super::wire::{
    CanonicalContextWire, CanonicalPayload, CanonicalPayloads, StreamingWriter, WireMeasure,
};
use super::{
    CANONICAL_CONTEXT_FORMAT, ContextBudget, ContextError, ContextPayload,
    MAX_CONTEXT_OUTPUT_BYTES, TokenPolicy,
};

#[derive(Clone, Copy, Debug)]
enum BudgetLimit {
    Byte,
    Token,
}

impl BudgetLimit {
    const fn omission_reason(self) -> ContextOmissionReasonV1 {
        match self {
            Self::Byte => ContextOmissionReasonV1::ByteBudget,
            Self::Token => ContextOmissionReasonV1::TokenBudget,
        }
    }
}
#[derive(Clone)]
struct StaticWireMeasures {
    format: WireMeasure,
    estimator_version: WireMeasure,
    omissions: [WireMeasure; 3],
    coverage: WireMeasure,
    conflicts: WireMeasure,
    lineage: WireMeasure,
    summary_omissions: WireMeasure,
}

/// Token/byte measures for the constant JSON structural literals that frame
/// every admission candidate. They depend only on the literal text and the
/// token policy, so measuring them once per `prepare_admission` and reusing the
/// clones avoids re-scanning ~12 fixed strings on every `measure_candidate`
/// iteration of the admission search loop.
#[derive(Clone)]
struct WireSeparators {
    open_format: WireMeasure,
    estimator_version_key: WireMeasure,
    bundle_records: WireMeasure,
    omissions_key: WireMeasure,
    continuation_anchors_key: WireMeasure,
    coverage_key: WireMeasure,
    conflicts_key: WireMeasure,
    lineage_key: WireMeasure,
    encoded_bytes_key: WireMeasure,
    summary_omissions_key: WireMeasure,
    payloads_key: WireMeasure,
    close: WireMeasure,
}

impl WireSeparators {
    fn measure(policy: TokenPolicy, control: &ExecutionControl) -> Result<Self, ContextError> {
        Ok(Self {
            open_format: measure_raw("{\"format\":", policy, control)?,
            estimator_version_key: measure_raw(",\"estimator_version\":", policy, control)?,
            bundle_records: measure_raw(",\"bundle\":{\"records\":[", policy, control)?,
            omissions_key: measure_raw("],\"omissions\":", policy, control)?,
            continuation_anchors_key: measure_raw(",\"continuation_anchors\":[", policy, control)?,
            coverage_key: measure_raw("],\"coverage\":", policy, control)?,
            conflicts_key: measure_raw(",\"conflicts\":", policy, control)?,
            lineage_key: measure_raw(",\"lineage\":", policy, control)?,
            encoded_bytes_key: measure_raw(",\"encoded_bytes\":", policy, control)?,
            summary_omissions_key: measure_raw("},\"summary_omissions\":", policy, control)?,
            payloads_key: measure_raw(",\"payloads\":[", policy, control)?,
            close: measure_raw("]}", policy, control)?,
        })
    }
}

pub(super) struct PreparedAdmission {
    records: Vec<CompactContextRecordV1>,
    record_prefix: Vec<WireMeasure>,
    continuation_suffix: Vec<WireMeasure>,
    payload_prefix: Vec<WireMeasure>,
    encoded_prefix: Vec<u64>,
    static_wire: StaticWireMeasures,
    separators: WireSeparators,
}

#[derive(Clone, Copy)]
pub struct AdmissionDecision {
    pub admitted: usize,
    limit: Option<BudgetLimit>,
    pub bytes: u64,
    pub tokens: u64,
}

pub fn prepare_admission<P: ContextPayload>(
    available: &[P],
    grain: RetrievalGrainV1,
    bundle: &CompactContextBundleV1,
    summary_omissions: &[SummaryOmission],
    estimator_version: &str,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<PreparedAdmission, ContextError> {
    let mut records = Vec::new();
    let mut record_prefix = Vec::new();
    let mut continuation_items = Vec::new();
    let mut continuation_suffix = Vec::new();
    let mut payload_prefix = Vec::new();
    let mut encoded_prefix = Vec::new();
    for values in [
        &mut record_prefix,
        &mut continuation_items,
        &mut continuation_suffix,
        &mut payload_prefix,
    ] {
        try_reserve(values, available.len().saturating_add(1))?;
    }
    try_reserve(&mut records, available.len())?;
    try_reserve(&mut encoded_prefix, available.len().saturating_add(1))?;

    record_prefix.push(WireMeasure::empty(policy)?);
    payload_prefix.push(WireMeasure::empty(policy)?);
    encoded_prefix.push(0);
    let comma = measure_raw(",", policy, control)?;
    for payload in available {
        control.checkpoint()?;
        let payload_measure = measure_serializable(&CanonicalPayload(payload), policy, control)?;
        let record = CompactContextRecordV1 {
            anchor_id: payload.anchor_id().clone(),
            grain,
            hydration: HydrationStateV1::Available,
            encoded_bytes: payload_measure.bytes,
        };
        let record_measure = measure_serializable(&record, policy, control)?;
        let anchor_measure = measure_serializable(payload.anchor_id(), policy, control)?;
        let record_next = append_array_measure(
            record_prefix.last().ok_or_else(|| {
                ContextError::InvalidBundle("missing record prefix seed".to_string())
            })?,
            &record_measure,
            records.len(),
            &comma,
        )?;
        let payload_next = append_array_measure(
            payload_prefix.last().ok_or_else(|| {
                ContextError::InvalidBundle("missing payload prefix seed".to_string())
            })?,
            &payload_measure,
            records.len(),
            &comma,
        )?;
        let encoded_next = encoded_prefix
            .last()
            .copied()
            .unwrap_or(0_u64)
            .checked_add(payload_measure.bytes)
            .ok_or(ContextError::BudgetExceeded { resource: "byte" })?;
        records.push(record);
        record_prefix.push(record_next);
        payload_prefix.push(payload_next);
        encoded_prefix.push(encoded_next);
        continuation_items.push(anchor_measure);
    }
    for _ in 0..=available.len() {
        continuation_suffix.push(WireMeasure::empty(policy)?);
    }
    for index in (0..available.len()).rev() {
        let item = if index + 1 == available.len() {
            continuation_items[index].clone()
        } else {
            continuation_items[index]
                .concatenate(&comma)?
                .concatenate(&continuation_suffix[index + 1])?
        };
        continuation_suffix[index] = item;
    }

    let omissions = [
        measure_omissions(&bundle.omissions, None, policy, control)?,
        measure_omissions(&bundle.omissions, Some(BudgetLimit::Byte), policy, control)?,
        measure_omissions(&bundle.omissions, Some(BudgetLimit::Token), policy, control)?,
    ];
    Ok(PreparedAdmission {
        records,
        record_prefix,
        continuation_suffix,
        payload_prefix,
        encoded_prefix,
        static_wire: StaticWireMeasures {
            format: measure_serializable(&CANONICAL_CONTEXT_FORMAT, policy, control)?,
            estimator_version: measure_serializable(&estimator_version, policy, control)?,
            omissions,
            coverage: measure_serializable(&bundle.coverage, policy, control)?,
            conflicts: measure_serializable(&bundle.conflicts, policy, control)?,
            lineage: measure_serializable(&bundle.lineage, policy, control)?,
            summary_omissions: measure_serializable(summary_omissions, policy, control)?,
        },
        separators: WireSeparators::measure(policy, control)?,
    })
}

fn append_array_measure(
    prefix: &WireMeasure,
    item: &WireMeasure,
    item_index: usize,
    comma: &WireMeasure,
) -> Result<WireMeasure, ContextError> {
    if item_index == 0 {
        prefix.concatenate(item)
    } else {
        prefix.concatenate(comma)?.concatenate(item)
    }
}

fn measure_omissions(
    base: &[CompactContextOmissionV1],
    limit: Option<BudgetLimit>,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<WireMeasure, ContextError> {
    let mut values = Vec::new();
    try_reserve(
        &mut values,
        base.len().saturating_add(usize::from(limit.is_some())),
    )?;
    for omission in base {
        values.push(omission.clone());
    }
    if let Some(limit) = limit {
        values.push(CompactContextOmissionV1 {
            anchor_id: None,
            reason: limit.omission_reason(),
        });
    }
    measure_serializable(&values, policy, control)
}

pub fn choose_admission(
    prepared: &PreparedAdmission,
    bundle: &CompactContextBundleV1,
    summary_omissions: &[SummaryOmission],
    budget: &ContextBudget,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<AdmissionDecision, ContextError> {
    let max_bytes = budget.max_bytes.min(MAX_CONTEXT_OUTPUT_BYTES);
    let baseline = measure_candidate(
        prepared,
        bundle,
        summary_omissions,
        0,
        None,
        policy,
        control,
    )?;
    require_fit(&baseline, max_bytes, budget.max_tokens)?;
    for admitted in 1..=prepared.records.len() {
        control.checkpoint()?;
        let candidate = measure_candidate(
            prepared,
            bundle,
            summary_omissions,
            admitted,
            None,
            policy,
            control,
        )?;
        let limit = if candidate.bytes > max_bytes {
            Some(BudgetLimit::Byte)
        } else if candidate.tokens() > budget.max_tokens {
            Some(BudgetLimit::Token)
        } else {
            None
        };
        if let Some(limit) = limit {
            let final_measure = measure_candidate(
                prepared,
                bundle,
                summary_omissions,
                admitted - 1,
                Some(limit),
                policy,
                control,
            )?;
            require_fit(&final_measure, max_bytes, budget.max_tokens)?;
            return Ok(AdmissionDecision {
                admitted: admitted - 1,
                limit: Some(limit),
                bytes: final_measure.bytes,
                tokens: final_measure.tokens(),
            });
        }
    }
    let final_measure = measure_candidate(
        prepared,
        bundle,
        summary_omissions,
        prepared.records.len(),
        None,
        policy,
        control,
    )?;
    Ok(AdmissionDecision {
        admitted: prepared.records.len(),
        limit: None,
        bytes: final_measure.bytes,
        tokens: final_measure.tokens(),
    })
}

fn require_fit(measure: &WireMeasure, max_bytes: u64, max_tokens: u64) -> Result<(), ContextError> {
    if measure.bytes > max_bytes {
        return Err(ContextError::BudgetExceeded { resource: "byte" });
    }
    if measure.tokens() > max_tokens {
        return Err(ContextError::BudgetExceeded { resource: "token" });
    }
    Ok(())
}

fn measure_candidate(
    prepared: &PreparedAdmission,
    _bundle: &CompactContextBundleV1,
    _summary_omissions: &[SummaryOmission],
    admitted: usize,
    limit: Option<BudgetLimit>,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<WireMeasure, ContextError> {
    let omissions = match limit {
        None => &prepared.static_wire.omissions[0],
        Some(BudgetLimit::Byte) => &prepared.static_wire.omissions[1],
        Some(BudgetLimit::Token) => &prepared.static_wire.omissions[2],
    };
    let encoded = measure_serializable(&prepared.encoded_prefix[admitted], policy, control)?;
    let separators = &prepared.separators;
    let mut measure = WireMeasure::empty(policy)?;
    for part in [
        separators.open_format.clone(),
        prepared.static_wire.format.clone(),
        separators.estimator_version_key.clone(),
        prepared.static_wire.estimator_version.clone(),
        separators.bundle_records.clone(),
        prepared.record_prefix[admitted].clone(),
        separators.omissions_key.clone(),
        omissions.clone(),
        separators.continuation_anchors_key.clone(),
        prepared.continuation_suffix[admitted].clone(),
        separators.coverage_key.clone(),
        prepared.static_wire.coverage.clone(),
        separators.conflicts_key.clone(),
        prepared.static_wire.conflicts.clone(),
        separators.lineage_key.clone(),
        prepared.static_wire.lineage.clone(),
        separators.encoded_bytes_key.clone(),
        encoded,
        separators.summary_omissions_key.clone(),
        prepared.static_wire.summary_omissions.clone(),
        separators.payloads_key.clone(),
        prepared.payload_prefix[admitted].clone(),
        separators.close.clone(),
    ] {
        measure = measure.concatenate(&part)?;
    }
    Ok(measure)
}

pub fn materialize_admission<P: ContextPayload>(
    bundle: &mut CompactContextBundleV1,
    available: &[P],
    _grain: RetrievalGrainV1,
    prepared: &PreparedAdmission,
    decision: AdmissionDecision,
    control: &ExecutionControl,
) -> Result<(), ContextError> {
    for record in &prepared.records[..decision.admitted] {
        control.checkpoint()?;
        bundle.records.push(record.clone());
    }
    for payload in &available[decision.admitted..] {
        control.checkpoint()?;
        bundle
            .continuation_anchors
            .push(payload.anchor_id().clone());
    }
    bundle.encoded_bytes = prepared.encoded_prefix[decision.admitted];
    if let Some(limit) = decision.limit {
        bundle.omissions.push(CompactContextOmissionV1 {
            anchor_id: None,
            reason: limit.omission_reason(),
        });
    }
    Ok(())
}

pub fn measure_context<P: ContextPayload>(
    bundle: &CompactContextBundleV1,
    summary_omissions: &[SummaryOmission],
    payloads: &[P],
    estimator_version: &str,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<WireMeasure, ContextError> {
    validate_bundle(bundle)?;
    measure_serializable(
        &CanonicalContextWire {
            format: CANONICAL_CONTEXT_FORMAT,
            estimator_version,
            bundle,
            summary_omissions,
            payloads: CanonicalPayloads(payloads),
        },
        policy,
        control,
    )
}

pub fn render_exact<P: ContextPayload>(
    bundle: &CompactContextBundleV1,
    summary_omissions: &[SummaryOmission],
    payloads: &[P],
    estimator_version: &str,
    policy: TokenPolicy,
    exact_bytes: u64,
    control: &ExecutionControl,
) -> Result<String, ContextError> {
    let wire = CanonicalContextWire {
        format: CANONICAL_CONTEXT_FORMAT,
        estimator_version,
        bundle,
        summary_omissions,
        payloads: CanonicalPayloads(payloads),
    };
    let mut writer = StreamingWriter::collecting(policy, exact_bytes, control)?;
    let result = serde_json::to_writer(&mut writer, &wire);
    let (measurement, output) = writer.finish(result)?;
    if measurement.bytes != exact_bytes {
        return Err(ContextError::InvalidBundle(
            "final canonical context length drifted".to_string(),
        ));
    }
    output
        .ok_or_else(|| ContextError::InvalidBundle("missing canonical context output".to_string()))
}

fn measure_serializable<T: Serialize + ?Sized>(
    value: &T,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<WireMeasure, ContextError> {
    let mut writer = StreamingWriter::measuring(policy, control)?;
    let result = serde_json::to_writer(&mut writer, value);
    writer.finish(result).map(|(measure, _)| measure)
}

fn measure_raw(
    value: &str,
    policy: TokenPolicy,
    control: &ExecutionControl,
) -> Result<WireMeasure, ContextError> {
    let mut writer = StreamingWriter::measuring(policy, control)?;
    let result = writer
        .write_all(value.as_bytes())
        .map_err(serde_json::Error::io);
    writer.finish(result).map(|(measure, _)| measure)
}

#[cfg(test)]
mod separator_equivalence_tests {
    //! Finding 8 equivalence: precomputing the constant JSON structural literals
    //! once in `prepare_admission` must produce byte-identical `WireMeasure`s to
    //! the previous implementation, which recomputed every literal inside
    //! `measure_candidate` on every admission-search iteration.
    use tracedecay_domain::RetrievalAnchorId;

    use super::*;

    struct TestPayload {
        anchor_id: RetrievalAnchorId,
        bytes: Vec<u8>,
    }

    impl ContextPayload for TestPayload {
        fn anchor_id(&self) -> &RetrievalAnchorId {
            &self.anchor_id
        }

        fn bytes(&self) -> &[u8] {
            &self.bytes
        }
    }

    fn anchor(value: &str) -> RetrievalAnchorId {
        serde_json::from_str(&format!("\"{value}\"")).expect("valid anchor")
    }

    /// Reference `measure_candidate`: recomputes every structural literal inline
    /// via `measure_raw`, exactly as the pre-optimization code did.
    fn measure_candidate_reference(
        prepared: &PreparedAdmission,
        admitted: usize,
        limit: Option<BudgetLimit>,
        policy: TokenPolicy,
        control: &ExecutionControl,
    ) -> Result<WireMeasure, ContextError> {
        let omissions = match limit {
            None => &prepared.static_wire.omissions[0],
            Some(BudgetLimit::Byte) => &prepared.static_wire.omissions[1],
            Some(BudgetLimit::Token) => &prepared.static_wire.omissions[2],
        };
        let encoded = measure_serializable(&prepared.encoded_prefix[admitted], policy, control)?;
        let mut measure = WireMeasure::empty(policy)?;
        for part in [
            measure_raw("{\"format\":", policy, control)?,
            prepared.static_wire.format.clone(),
            measure_raw(",\"estimator_version\":", policy, control)?,
            prepared.static_wire.estimator_version.clone(),
            measure_raw(",\"bundle\":{\"records\":[", policy, control)?,
            prepared.record_prefix[admitted].clone(),
            measure_raw("],\"omissions\":", policy, control)?,
            omissions.clone(),
            measure_raw(",\"continuation_anchors\":[", policy, control)?,
            prepared.continuation_suffix[admitted].clone(),
            measure_raw("],\"coverage\":", policy, control)?,
            prepared.static_wire.coverage.clone(),
            measure_raw(",\"conflicts\":", policy, control)?,
            prepared.static_wire.conflicts.clone(),
            measure_raw(",\"lineage\":", policy, control)?,
            prepared.static_wire.lineage.clone(),
            measure_raw(",\"encoded_bytes\":", policy, control)?,
            encoded,
            measure_raw("},\"summary_omissions\":", policy, control)?,
            prepared.static_wire.summary_omissions.clone(),
            measure_raw(",\"payloads\":[", policy, control)?,
            prepared.payload_prefix[admitted].clone(),
            measure_raw("]}", policy, control)?,
        ] {
            measure = measure.concatenate(&part)?;
        }
        Ok(measure)
    }

    fn sample_bundle() -> CompactContextBundleV1 {
        CompactContextBundleV1 {
            omissions: vec![
                CompactContextOmissionV1 {
                    anchor_id: Some(anchor("dropped-1")),
                    reason: ContextOmissionReasonV1::ByteBudget,
                },
                CompactContextOmissionV1 {
                    anchor_id: None,
                    reason: ContextOmissionReasonV1::TokenBudget,
                },
            ],
            ..CompactContextBundleV1::default()
        }
    }

    #[test]
    fn precomputed_separators_equal_inline_measures() {
        for value in [
            "{\"format\":",
            ",\"estimator_version\":",
            ",\"bundle\":{\"records\":[",
            "],\"omissions\":",
            ",\"continuation_anchors\":[",
            "],\"coverage\":",
            ",\"conflicts\":",
            ",\"lineage\":",
            ",\"encoded_bytes\":",
            "},\"summary_omissions\":",
            ",\"payloads\":[",
            "]}",
        ] {
            for policy in [TokenPolicy::Whitespace, TokenPolicy::Characters] {
                let control = ExecutionControl::default();
                let separators = WireSeparators::measure(policy, &control).expect("separators");
                let inline = measure_raw(value, policy, &control).expect("inline measure");
                let precomputed = match value {
                    "{\"format\":" => &separators.open_format,
                    ",\"estimator_version\":" => &separators.estimator_version_key,
                    ",\"bundle\":{\"records\":[" => &separators.bundle_records,
                    "],\"omissions\":" => &separators.omissions_key,
                    ",\"continuation_anchors\":[" => &separators.continuation_anchors_key,
                    "],\"coverage\":" => &separators.coverage_key,
                    ",\"conflicts\":" => &separators.conflicts_key,
                    ",\"lineage\":" => &separators.lineage_key,
                    ",\"encoded_bytes\":" => &separators.encoded_bytes_key,
                    "},\"summary_omissions\":" => &separators.summary_omissions_key,
                    ",\"payloads\":[" => &separators.payloads_key,
                    "]}" => &separators.close,
                    other => panic!("unexpected literal {other}"),
                };
                assert_eq!(precomputed, &inline, "literal {value:?} policy {policy:?}");
            }
        }
    }

    #[test]
    fn measure_candidate_matches_reference_across_admissions() {
        let available = (0..5)
            .map(|index| TestPayload {
                anchor_id: anchor(&format!("anchor-{index}")),
                bytes: format!("payload body {index} with words").into_bytes(),
            })
            .collect::<Vec<_>>();
        let bundle = sample_bundle();
        for policy in [TokenPolicy::Whitespace, TokenPolicy::Characters] {
            let control = ExecutionControl::default();
            let prepared = prepare_admission(
                &available,
                RetrievalGrainV1::LogicalMessage,
                &bundle,
                &[],
                "estimator-v1",
                policy,
                &control,
            )
            .expect("prepare admission");
            for admitted in 0..=available.len() {
                for limit in [None, Some(BudgetLimit::Byte), Some(BudgetLimit::Token)] {
                    let optimized = measure_candidate(
                        &prepared,
                        &bundle,
                        &[],
                        admitted,
                        limit,
                        policy,
                        &control,
                    )
                    .expect("optimized measure");
                    let reference =
                        measure_candidate_reference(&prepared, admitted, limit, policy, &control)
                            .expect("reference measure");
                    assert_eq!(
                        optimized, reference,
                        "admitted {admitted} limit {limit:?} policy {policy:?}"
                    );
                }
            }
        }
    }
}
