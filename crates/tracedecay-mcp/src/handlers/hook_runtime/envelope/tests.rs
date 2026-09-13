use tracedecay_domain::ObservationSourceRangeV1;

use super::super::test_support::*;
use super::*;

#[test]
fn daemon_minted_hook_ids_are_replay_stable_typed_and_binding_scoped() {
    let mut native = admission_test_envelope(9, 7);
    native.event = tracedecay_hooks::HookEventV2::SavedEdit {
        file_id: [9; 16],
        changed_range_count: 1,
    };

    let first = daemon_mint_hook_v2_envelope(&native);
    let replay = daemon_mint_hook_v2_envelope(&native);
    assert_eq!(replay, first);

    let tracedecay_hooks::HookEventV2::SavedEdit { file_id, .. } = first.event else {
        panic!("expected saved-edit envelope");
    };
    assert_ne!(first.event_id, file_id);

    let mut different_binding = native.clone();
    different_binding.binding_token = [8; 32];
    let different_binding = daemon_mint_hook_v2_envelope(&different_binding);
    assert_ne!(different_binding.event_id, first.event_id);
    let tracedecay_hooks::HookEventV2::SavedEdit {
        file_id: different_file_id,
        ..
    } = different_binding.event
    else {
        panic!("expected saved-edit envelope");
    };
    assert_ne!(different_file_id, file_id);

    let mut different_session = native.clone();
    different_session.protected_session_id = [6; 32];
    let different_session = daemon_mint_hook_v2_envelope(&different_session);
    assert_ne!(different_session.event_id, first.event_id);
}

#[test]
fn lifecycle_range_prefers_native_sequence_and_reuses_unknown_ledger_order() {
    let receipt = tracedecay_hooks::HookAdmissionLedgerReceiptV1 {
        decision: tracedecay_hooks::HookAdmissionDecisionV1::Admitted,
        order: 7,
        work_completed: false,
    };
    let unknown = admission_test_envelope(21, 7);
    assert_eq!(
        hook_v2_lifecycle_range(&unknown, receipt),
        ObservationSourceRangeV1::new(8, 9).ok()
    );

    let mut native = admission_test_envelope(22, 7);
    native.ordering = tracedecay_hooks::HookOrderingV1::ProviderSequence(41);
    assert_eq!(
        hook_v2_lifecycle_range(&native, receipt),
        ObservationSourceRangeV1::new(41, 42).ok()
    );
}
