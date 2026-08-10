//! Typed SDK proof for provider-qualified Work-to-TaskSession availability.

use tracedecay_application::{
    VerifiedWorkGraphVersionV1, WorkAttemptReceiptV1, WorkEvidenceOmissionReasonV1,
    WorkEvidenceRetrieveRequestV1, WorkEvidenceSourceV1, WorkProductSelectionScopeV1,
    WorkTaskSessionEvidenceV1,
};
use tracedecay_domain::{TemporalModeV1, UtcMicros, WorkAttemptIdentityV1};
use tracedecay_sdk::client::Client;
use tracedecay_sdk::operations::WorkRetrieveEvidence;

use super::{PROVIDER_SESSION_ID, now};

pub(super) fn assert_restored_provider_session_unavailable(
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &tracedecay_domain::TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
) -> WorkAttemptReceiptV1 {
    let mut restored_receipt = None;
    for temporal in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf { cutoff: now() },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let (receipt, evidence, omissions) = retrieve(
            client,
            selection,
            task_id,
            verified_version,
            identity,
            temporal,
        )
        .unwrap_or_else(|error| panic!("typed SDK retrieval failed in {temporal:?}: {error}"));
        let receipt =
            receipt.unwrap_or_else(|| panic!("typed SDK omitted attempt receipt in {temporal:?}"));
        assert!(
            evidence.is_none(),
            "a missing evaluated query authority cannot hydrate TaskSession in {temporal:?}"
        );
        assert!(
            omissions.iter().any(|omission| {
                omission.relation == "task_session"
                    && omission.reason == WorkEvidenceOmissionReasonV1::Unavailable
            }),
            "TaskSession unavailability must remain typed in {temporal:?}: {omissions:?}"
        );
        let provider_session = receipt
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.provider_session.as_ref())
            .expect("provider-qualified attempt receipt");
        assert_eq!(provider_session.provider().as_str(), "claude");
        assert_eq!(provider_session.session_id().as_str(), PROVIDER_SESSION_ID);
        if let Some(restored) = &restored_receipt {
            assert_eq!(
                restored, &receipt,
                "temporal modes must preserve the receipt"
            );
        } else {
            restored_receipt = Some(receipt);
        }
    }

    restored_receipt.expect("restored provider receipt")
}

fn retrieve(
    client: &Client,
    selection: &WorkProductSelectionScopeV1,
    task_id: &tracedecay_domain::TaskId,
    verified_version: &VerifiedWorkGraphVersionV1,
    identity: &WorkAttemptIdentityV1,
    temporal: TemporalModeV1,
) -> Result<
    (
        Option<WorkAttemptReceiptV1>,
        Option<WorkTaskSessionEvidenceV1>,
        Vec<tracedecay_application::WorkEvidenceOmissionV1>,
    ),
    String,
> {
    let result = client
        .execute::<WorkRetrieveEvidence>(&WorkEvidenceRetrieveRequestV1 {
            selection: selection.clone(),
            task_id: task_id.clone(),
            verified_version: verified_version.clone(),
            temporal,
            page_size: 100,
            expansion: None,
            continuation: None,
            observed_at: UtcMicros(now().0),
        })
        .map_err(|error| error.to_string())?
        .result;
    let omissions = result.omissions;
    let mut receipt = None;
    let mut task_session = None;
    for source in result.sources {
        match source {
            WorkEvidenceSourceV1::AttemptReceipt { receipt: candidate }
                if candidate.identity == *identity =>
            {
                receipt = Some(candidate);
            }
            WorkEvidenceSourceV1::TaskSession { attempt, evidence } if attempt == *identity => {
                task_session = Some(evidence)
            }
            _ => {}
        }
    }
    Ok((receipt, task_session, omissions))
}
