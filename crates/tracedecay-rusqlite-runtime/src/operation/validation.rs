use tracedecay_store::{RuntimeSubmitRequestV1, StorageRuntimeContractErrorV1};

pub(super) fn validate(
    request: &RuntimeSubmitRequestV1,
) -> Result<(), StorageRuntimeContractErrorV1> {
    request.validate()?;
    request
        .transaction_scope()
        .validate_operation(&request.envelope().metadata)
}
