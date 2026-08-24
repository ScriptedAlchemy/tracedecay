use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

use super::{
    ProcessResidentMemoryV1, ResidentMemoryAdmissionFailureV1, ResidentMemoryComponentIdV1,
    ResidentMemoryKeyV1,
};

fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test byte count is non-zero")
}

#[test]
fn process_shared_reservation_uses_same_ceiling_and_releases_exactly() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let component = ResidentMemoryComponentIdV1::new("sessions.codex.prepared-pages").unwrap();
    let mut reservation = authority
        .reserve_process_shared(component, bytes(80))
        .expect("process-shared reservation");
    assert_eq!(
        authority.snapshot().process_shared_charge_for(component),
        80
    );
    assert!(
        authority
            .reserve_process_shared(component, bytes(30))
            .is_err()
    );

    reservation.shrink_to(40).unwrap();
    assert_eq!(
        authority.snapshot().process_shared_charge_for(component),
        40
    );
    drop(reservation);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn keyed_and_process_shared_reservations_compete_for_one_ceiling() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let keyed = key("project-a", "worktree-a", "generation-a", "semantic-index");
    let shared = ResidentMemoryComponentIdV1::new("sessions.codex.prepared-pages").unwrap();
    let _keyed_reservation = authority
        .reserve(keyed.clone(), bytes(60))
        .expect("keyed reservation");
    let _shared_reservation = authority
        .reserve_process_shared(shared, bytes(40))
        .expect("process-shared reservation fills the common ceiling");

    let error = authority
        .reserve_process_shared(shared, bytes(1))
        .expect_err("neither ownership kind can overcommit the process ceiling");
    assert_eq!(
        error,
        ResidentMemoryAdmissionFailureV1 {
            used_bytes: 100,
            requested_bytes: 1,
            limit_bytes: 100,
        }
    );
    let snapshot = authority.snapshot();
    assert_eq!(snapshot.used_bytes, 100);
    assert_eq!(snapshot.charge_for(&keyed), 60);
    assert_eq!(snapshot.process_shared_charge_for(shared), 40);
}

#[test]
fn same_component_process_shared_reservations_shrink_and_release_independently() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let component = ResidentMemoryComponentIdV1::new("sessions.codex.prepared-pages").unwrap();
    let mut first = authority
        .reserve_process_shared(component, bytes(30))
        .expect("first process-shared reservation");
    let second = authority
        .reserve_process_shared(component, bytes(50))
        .expect("second process-shared reservation");
    assert_eq!(
        authority.snapshot().process_shared_charge_for(component),
        80
    );

    first.shrink_to(10).expect("first reservation shrinks");
    let after_shrink = authority.snapshot();
    assert_eq!(after_shrink.used_bytes, 60);
    assert_eq!(after_shrink.process_shared_charge_for(component), 60);

    drop(second);
    let after_second_drop = authority.snapshot();
    assert_eq!(after_second_drop.used_bytes, 10);
    assert_eq!(after_second_drop.process_shared_charge_for(component), 10);

    drop(first);
    let released = authority.snapshot();
    assert_eq!(released.used_bytes, 0);
    assert_eq!(released.process_shared_charge_for(component), 0);
    assert!(released.process_shared_charges.is_empty());
}

fn key(
    project: &str,
    worktree: &str,
    generation: &str,
    component: &'static str,
) -> ResidentMemoryKeyV1 {
    ResidentMemoryKeyV1 {
        project_id: ProjectId::new(project).expect("valid project id"),
        worktree_id: WorktreeId::new(worktree).expect("valid worktree id"),
        generation_id: CodeGenerationId::new(generation).expect("valid generation id"),
        component: ResidentMemoryComponentIdV1::new(component).expect("valid component id"),
    }
}

#[test]
fn reservation_tracks_exact_identity_and_releases_on_drop() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let canonical = key("project-a", "worktree-a", "generation-a", "canonical");
    let lexical = key("project-a", "worktree-a", "generation-a", "lexical");

    let canonical_reservation = authority
        .reserve(canonical.clone(), bytes(60))
        .expect("canonical reservation");
    let lexical_reservation = authority
        .reserve(lexical.clone(), bytes(30))
        .expect("lexical reservation");

    let snapshot = authority.snapshot();
    assert_eq!(snapshot.used_bytes, 90);
    assert_eq!(snapshot.charge_for(&canonical), 60);
    assert_eq!(snapshot.charge_for(&lexical), 30);

    drop(canonical_reservation);
    assert_eq!(authority.snapshot().charge_for(&canonical), 0);
    assert_eq!(authority.snapshot().used_bytes, 30);

    drop(lexical_reservation);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn rejection_reports_final_used_requested_and_limit_bytes() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let _held = authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            bytes(80),
        )
        .expect("initial reservation");

    let error = authority
        .reserve(
            key("project-b", "worktree-b", "generation-b", "canonical"),
            bytes(30),
        )
        .expect_err("reservation exceeds the process ceiling");

    assert_eq!(
        error,
        ResidentMemoryAdmissionFailureV1 {
            used_bytes: 80,
            requested_bytes: 30,
            limit_bytes: 100,
        }
    );
}

#[test]
fn reservation_can_only_adjust_down_to_measured_retained_bytes() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let mut reservation = authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            bytes(80),
        )
        .expect("conservative reservation");

    reservation
        .shrink_to(55)
        .expect("measured retained bytes fit the reservation");
    assert_eq!(reservation.reserved_bytes(), 55);
    assert_eq!(authority.snapshot().used_bytes, 55);

    let error = reservation
        .shrink_to(56)
        .expect_err("a reservation cannot grow after allocation");
    assert_eq!(error.reserved_bytes, 55);
    assert_eq!(error.measured_bytes, 56);
}

#[test]
fn reclaimers_run_outside_the_lock_in_stable_order_until_reservation_fits() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let held = Arc::new(Mutex::new(Some(
        authority
            .reserve(
                key("project-a", "worktree-a", "generation-a", "historical"),
                bytes(80),
            )
            .expect("historical reservation"),
    )));
    let calls = Arc::new(Mutex::new(Vec::new()));

    let first_calls = Arc::clone(&calls);
    let _first = authority
        .register_reclaimer(
            10,
            Arc::new(move |request| {
                first_calls.lock().expect("call log").push(10);
                assert_eq!(request.used_bytes, 80);
                assert_eq!(request.requested_bytes, 30);
            }),
        )
        .expect("first reclaimer");
    let second_calls = Arc::clone(&calls);
    let second_held = Arc::clone(&held);
    let _second = authority
        .register_reclaimer(
            20,
            Arc::new(move |_| {
                second_calls.lock().expect("call log").push(20);
                drop(second_held.lock().expect("held reservation").take());
            }),
        )
        .expect("second reclaimer");
    let third_calls = Arc::clone(&calls);
    let _third = authority
        .register_reclaimer(
            30,
            Arc::new(move |_| {
                third_calls.lock().expect("call log").push(30);
            }),
        )
        .expect("third reclaimer");

    let replacement = authority
        .reserve(
            key("project-b", "worktree-b", "generation-b", "canonical"),
            bytes(30),
        )
        .expect("second reclaimer releases enough bytes");

    assert_eq!(*calls.lock().expect("call log"), vec![10, 20]);
    assert_eq!(authority.snapshot().used_bytes, 30);
    drop(replacement);
}

#[test]
fn dropped_reclaimer_registration_is_not_called() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(10)));
    let calls = Arc::new(Mutex::new(0_u64));
    let callback_calls = Arc::clone(&calls);
    let registration = authority
        .register_reclaimer(
            10,
            Arc::new(move |_| {
                *callback_calls.lock().expect("call count") += 1;
            }),
        )
        .expect("reclaimer registration");
    drop(registration);

    let _held = authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            bytes(10),
        )
        .expect("full reservation");
    let _error = authority
        .reserve(
            key("project-b", "worktree-b", "generation-b", "canonical"),
            bytes(1),
        )
        .expect_err("no registered reclaimer remains");

    assert_eq!(*calls.lock().expect("call count"), 0);
}

#[test]
fn concurrent_reservations_never_overcommit_the_process_ceiling() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(80)));
    let barrier = Arc::new(std::sync::Barrier::new(9));
    let mut tasks = Vec::new();
    for index in 0..8 {
        let authority = Arc::clone(&authority);
        let barrier = Arc::clone(&barrier);
        tasks.push(std::thread::spawn(move || {
            let reservation = authority
                .reserve(
                    key(
                        "project-a",
                        "worktree-a",
                        "generation-a",
                        Box::leak(format!("component-{index}").into_boxed_str()),
                    ),
                    bytes(10),
                )
                .expect("reservation within ceiling");
            barrier.wait();
            barrier.wait();
            reservation
        }));
    }

    barrier.wait();
    assert_eq!(authority.snapshot().used_bytes, 80);
    barrier.wait();
    for task in tasks {
        drop(task.join().expect("reservation task"));
    }
    assert_eq!(authority.snapshot().used_bytes, 0);
}
