use tracedecay_domain::{
    EvidenceAvailabilityV1, GenerationBoundRepositoryProvenanceV1, RetrievalAnchorRecordV2,
};
use tracedecay_store::{
    ObservationStoreError, ObservationStoreResult, RepositoryProvenanceAttachmentV1,
};

pub(super) fn storage(
    operation: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> ObservationStoreError {
    ObservationStoreError::Storage {
        operation,
        source: Box::new(source),
    }
}

pub(super) fn storage_message(
    operation: &'static str,
    message: impl Into<String>,
) -> ObservationStoreError {
    storage(operation, std::io::Error::other(message.into()))
}

pub(super) fn encode<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> ObservationStoreResult<String> {
    serde_json::to_string(value).map_err(|error| storage(operation, error))
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(
    value: &str,
    operation: &'static str,
) -> ObservationStoreResult<T> {
    serde_json::from_str(value).map_err(|error| storage(operation, error))
}

pub(super) fn encode_json_string<T: serde::Serialize>(
    value: &T,
    operation: &'static str,
) -> ObservationStoreResult<String> {
    match serde_json::to_value(value).map_err(|error| storage(operation, error))? {
        serde_json::Value::String(value) => Ok(value),
        _ => Err(storage_message(operation, "encoded value is not a string")),
    }
}

pub(super) fn decode_repository_provenance_attachment(
    availability_json: &str,
    capture_json: Option<&str>,
    anchor_json: Option<&str>,
    operation: &'static str,
) -> ObservationStoreResult<RepositoryProvenanceAttachmentV1> {
    let availability: EvidenceAvailabilityV1<GenerationBoundRepositoryProvenanceV1> =
        decode(availability_json, operation)?;
    let capture = capture_json
        .map(|capture| decode::<GenerationBoundRepositoryProvenanceV1>(capture, operation))
        .transpose()?;
    if availability.value() != capture.as_ref() {
        return Err(ObservationStoreError::RepositoryProvenanceBindingMismatch);
    }
    RepositoryProvenanceAttachmentV1::new(
        availability,
        anchor_json
            .map(|anchor| decode::<RetrievalAnchorRecordV2>(anchor, operation))
            .transpose()?,
    )
}

pub(super) fn decode_sequence(value: i64, operation: &'static str) -> ObservationStoreResult<u64> {
    u64::try_from(value).map_err(|_| storage_message(operation, "negative observation sequence"))
}
