use serde::de::DeserializeOwned;
use tracedecay_domain::{
    LogicalCopyRecordV1, RetrievalAnchorId, SessionEvidenceMetadataV1, SessionId,
    SessionSummaryIdV1, SessionSummaryRecordV1, SummaryPublicationMetadataV1,
    SummarySourceHorizonV1, TemporalValidityV1, UtcMicros,
};

use tracedecay_temporal_query::ports::{SummarySourceRecord, TemporalPortError, TemporalRecord};
use tracedecay_temporal_query::resolution::{
    ResolutionAssertion, ResolutionEvidence, ResolutionOccurrence, SummarySourceState,
    ValidatedAuthorization,
};

use super::super::sql::TemporalSqlRow;
use super::RECORD_OPERATION;

pub(super) fn temporal_record_from_row(
    row: &TemporalSqlRow,
) -> Result<TemporalRecord, TemporalPortError> {
    let kind: String = row
        .get(3)
        .map_err(|error| read_error(RECORD_OPERATION, error))?;
    match kind.as_str() {
        "occurrence" => {
            let occurrence_id = required_string(row, 4)?;
            let anchor_id = required_string(row, 5)?;
            let knowledge_at = required_i64(row, 7)?;
            let valid_time = required_string(row, 8)?;
            let evidence = required_string(row, 9)?;
            let evidence: SessionEvidenceMetadataV1 = parse_json(&evidence, RECORD_OPERATION)?;
            let mut evidence = authorized_evidence(evidence);
            if let Some(derived_anchor) = optional_string(row, 6)? {
                evidence =
                    evidence.with_supporting_anchor(parse_text(derived_anchor, RECORD_OPERATION)?);
            }
            Ok(TemporalRecord::Occurrence(ResolutionOccurrence {
                occurrence_id: parse_text(occurrence_id, RECORD_OPERATION)?,
                anchor_id: parse_text(anchor_id, RECORD_OPERATION)?,
                knowledge_at: UtcMicros(knowledge_at),
                valid_time: parse_json(&valid_time, RECORD_OPERATION)?,
                evidence,
            }))
        }
        "assertion" => {
            let assertion_kind = required_string(row, 4)?;
            let subject = required_string(row, 5)?;
            let object = required_string(row, 6)?;
            let evidence: SessionEvidenceMetadataV1 =
                parse_json(&required_string(row, 9)?, RECORD_OPERATION)?;
            Ok(TemporalRecord::Assertion(ResolutionAssertion {
                kind: parse_text(assertion_kind, RECORD_OPERATION)?,
                subject_anchor_id: parse_text(subject, RECORD_OPERATION)?,
                object_anchor_id: parse_text(object, RECORD_OPERATION)?,
                knowledge_at: UtcMicros(required_i64(row, 7)?),
                valid_time: parse_json(&required_string(row, 8)?, RECORD_OPERATION)?,
                evidence: authorized_evidence(evidence),
            }))
        }
        "copy" => {
            let valid_time = match required_string(row, 8) {
                Ok(encoded) => parse_json(&encoded, RECORD_OPERATION)?,
                Err(_) => TemporalValidityV1::Unknown,
            };
            Ok(TemporalRecord::Copy(LogicalCopyRecordV1 {
                occurrence_id: parse_text(required_string(row, 4)?, RECORD_OPERATION)?,
                copied_from_occurrence_id: parse_text(required_string(row, 5)?, RECORD_OPERATION)?,
                knowledge_at: UtcMicros(required_i64(row, 7)?),
                valid_time,
                proof: parse_json(&required_string(row, 10)?, RECORD_OPERATION)?,
            }))
        }
        "summary" => summary_from_row(row).map(TemporalRecord::Summary),
        "summary_source" => summary_source_from_row(row).map(TemporalRecord::SummarySource),
        _ => Err(read_message(
            RECORD_OPERATION,
            "unknown temporal record kind",
        )),
    }
}

pub(super) fn summary_from_row(
    row: &TemporalSqlRow,
) -> Result<SessionSummaryRecordV1, TemporalPortError> {
    let summary_id: SessionSummaryIdV1 = parse_text(required_string(row, 4)?, RECORD_OPERATION)?;
    let summary_anchor: RetrievalAnchorId = parse_text(required_string(row, 5)?, RECORD_OPERATION)?;
    let source_values: Vec<String> = parse_json(&required_string(row, 11)?, RECORD_OPERATION)?;
    let mut source_anchors = Vec::with_capacity(source_values.len());
    for value in source_values {
        source_anchors.push(parse_text(value, RECORD_OPERATION)?);
    }
    let session_id: SessionId = parse_text(required_string(row, 15)?, RECORD_OPERATION)?;
    let horizon: SummarySourceHorizonV1 = parse_json(&required_string(row, 10)?, RECORD_OPERATION)?;
    let mut summary = SessionSummaryRecordV1::new(
        summary_id,
        session_id,
        summary_anchor,
        source_anchors,
        horizon,
        UtcMicros(required_i64(row, 7)?),
    )
    .map_err(|error| read_error(RECORD_OPERATION, error))?;
    if let Some(predecessor) = optional_string(row, 12)? {
        summary = summary
            .with_predecessor(parse_text(predecessor, RECORD_OPERATION)?)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
    }
    if let Some(publication) = optional_string(row, 13)? {
        let value: serde_json::Value = parse_json(&publication, RECORD_OPERATION)?;
        let publication = if value.get("version").is_some() {
            let configuration_digest = value["configuration_digest"]
                .as_str()
                .map(|digest| {
                    if digest.starts_with("sha256:") {
                        digest.to_owned()
                    } else {
                        format!("sha256:{digest}")
                    }
                })
                .ok_or_else(|| {
                    read_message(
                        RECORD_OPERATION,
                        "summary publication configuration digest is unavailable",
                    )
                })?;
            serde_json::json!({
                "model_route": value["model_route"],
                "configuration_digest": configuration_digest,
                "sanitization_receipt": {
                    "receipt_id": value["sanitization_receipt"],
                    "sanitizer_version": super::super::operations::SANITIZER_VERSION,
                },
            })
        } else {
            value
        };
        let publication: SummaryPublicationMetadataV1 = serde_json::from_value(publication)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
        summary = summary
            .with_publication(publication)
            .map_err(|error| read_error(RECORD_OPERATION, error))?;
    }
    Ok(summary)
}

pub(super) fn summary_source_from_row(
    row: &TemporalSqlRow,
) -> Result<SummarySourceRecord, TemporalPortError> {
    let anchor_id = parse_text(required_string(row, 4)?, RECORD_OPERATION)?;
    let state = match required_string(row, 14)?.as_str() {
        "covered" => SummarySourceState::Covered {
            knowledge_at: UtcMicros(required_i64(row, 7)?),
            valid_time: parse_json(&required_string(row, 8)?, RECORD_OPERATION)?,
        },
        "stale" => SummarySourceState::Stale,
        "unavailable" => SummarySourceState::Unavailable,
        "missing" => SummarySourceState::Missing,
        _ => {
            return Err(read_message(
                RECORD_OPERATION,
                "unknown summary source state",
            ));
        }
    };
    Ok(SummarySourceRecord { anchor_id, state })
}

pub(super) fn authorized_evidence(evidence: SessionEvidenceMetadataV1) -> ResolutionEvidence {
    ResolutionEvidence::new(evidence.authority, ValidatedAuthorization::Authorized)
        .with_supporting_anchor(evidence.source_anchor_id)
}

pub(super) fn required_string(
    row: &TemporalSqlRow,
    column: i32,
) -> Result<String, TemporalPortError> {
    row.get(column)
        .map_err(|error| read_error(RECORD_OPERATION, error))
}

pub(super) fn optional_string(
    row: &TemporalSqlRow,
    column: i32,
) -> Result<Option<String>, TemporalPortError> {
    row.get(column)
        .map_err(|error| read_error(RECORD_OPERATION, error))
}

pub(super) fn required_i64(row: &TemporalSqlRow, column: i32) -> Result<i64, TemporalPortError> {
    row.get(column)
        .map_err(|error| read_error(RECORD_OPERATION, error))
}

pub(super) fn parse_json<T: DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> Result<T, TemporalPortError> {
    serde_json::from_str(value).map_err(|error| read_error(operation, error))
}

pub(super) fn parse_text<T: DeserializeOwned>(
    value: String,
    operation: &'static str,
) -> Result<T, TemporalPortError> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|error| read_error(operation, error))
}

pub(super) fn read_error(
    operation: &'static str,
    error: impl std::fmt::Display,
) -> TemporalPortError {
    TemporalPortError::Read {
        operation,
        message: error.to_string(),
    }
}

pub(super) fn read_message(
    operation: &'static str,
    message: impl Into<String>,
) -> TemporalPortError {
    TemporalPortError::Read {
        operation,
        message: message.into(),
    }
}
