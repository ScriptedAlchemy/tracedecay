use std::fs;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

use super::{
    DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1, ProcessResidentMemoryV1,
    RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1,
    RESIDENT_MEMORY_PRESSURE_HIGH_WATERMARK_PERMILLE_V1,
    RESIDENT_MEMORY_PRESSURE_LOW_WATERMARK_PERMILLE_V1, ResidentMemoryAdmissionFailureV1,
    ResidentMemoryComponentIdV1, ResidentMemoryKeyV1, ResidentMemoryPressureStateV1,
    ResidentMemoryPressureV1, cgroup_v2_memory_limit_v1, effective_memory_bytes_v1,
    process_resident_memory_limit_for_system_v1, process_resident_memory_limit_v1,
    resident_memory_watermark_bytes_v1,
};

fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test byte count is non-zero")
}

#[test]
fn host_capacity_reserves_one_quarter_without_a_universal_ceiling() {
    assert_eq!(
        process_resident_memory_limit_for_system_v1(8 * 1024 * 1024 * 1024).get(),
        6 * 1024 * 1024 * 1024
    );
    assert_eq!(
        process_resident_memory_limit_for_system_v1(88 * 1024 * 1024 * 1024).get(),
        66 * 1024 * 1024 * 1024
    );
    assert_eq!(
        process_resident_memory_limit_for_system_v1(0),
        DEFAULT_PROCESS_RESIDENT_MEMORY_LIMIT_V1
    );
}

fn cgroup_fixture(
    cgroup_membership: Option<&str>,
    memory_max: Option<&str>,
    memory_high: Option<&str>,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let directory = tempfile::tempdir().expect("cgroup fixture root");
    let proc_self_cgroup = directory.path().join("proc-self-cgroup");
    let cgroup_root = directory.path().join("sys-fs-cgroup");
    fs::create_dir_all(&cgroup_root).expect("cgroup mount fixture");
    if let Some(membership) = cgroup_membership {
        fs::write(&proc_self_cgroup, membership).expect("process cgroup membership fixture");
    }
    let process_cgroup = cgroup_root.join("trace.slice/daemon.scope");
    fs::create_dir_all(&process_cgroup).expect("process cgroup fixture");
    if let Some(limit) = memory_max {
        fs::write(process_cgroup.join("memory.max"), limit).expect("memory.max fixture");
    }
    if let Some(limit) = memory_high {
        fs::write(process_cgroup.join("memory.high"), limit).expect("memory.high fixture");
    }
    (directory, proc_self_cgroup, cgroup_root)
}

fn effective_memory_bytes(
    total_memory_bytes: u64,
    proc_self_cgroup: &std::path::Path,
    cgroup_root: &std::path::Path,
) -> u64 {
    effective_memory_bytes_v1(
        total_memory_bytes,
        cgroup_v2_memory_limit_v1(proc_self_cgroup, cgroup_root),
    )
}

#[test]
fn absent_cgroup_membership_keeps_host_memory_capacity() {
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(None, None, None);
    assert_eq!(
        effective_memory_bytes(88 * 1024 * 1024 * 1024, &proc_self_cgroup, &cgroup_root,),
        88 * 1024 * 1024 * 1024
    );
}

#[test]
fn absent_cgroup_memory_files_keep_host_memory_capacity() {
    let (_directory, proc_self_cgroup, cgroup_root) =
        cgroup_fixture(Some("0::/trace.slice/daemon.scope\n"), None, None);
    assert_eq!(
        effective_memory_bytes(88 * 1024 * 1024 * 1024, &proc_self_cgroup, &cgroup_root,),
        88 * 1024 * 1024 * 1024
    );
}

#[test]
fn cgroup_v1_only_membership_does_not_invent_a_v2_ceiling() {
    let gib = 1024 * 1024 * 1024;
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("12:memory:/trace.slice/daemon.scope\n"),
        Some("32212254720\n"),
        Some("max\n"),
    );

    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        88 * gib
    );
}

#[test]
fn hybrid_membership_uses_the_unified_v2_memory_ceiling() {
    let gib = 1024 * 1024 * 1024;
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("12:memory:/legacy.slice\n0::/trace.slice/daemon.scope\n"),
        Some("32212254720\n"),
        Some("max\n"),
    );

    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        30 * gib
    );
}

#[test]
fn root_v2_membership_reads_the_mount_root_ceiling() {
    let gib = 1024 * 1024 * 1024;
    let directory = tempfile::tempdir().expect("cgroup fixture root");
    let proc_self_cgroup = directory.path().join("proc-self-cgroup");
    let cgroup_root = directory.path().join("sys-fs-cgroup");
    fs::create_dir_all(&cgroup_root).expect("cgroup mount fixture");
    fs::write(&proc_self_cgroup, "0::/\n").expect("root process cgroup membership fixture");
    fs::write(cgroup_root.join("memory.max"), "32212254720\n").expect("root memory.max fixture");
    fs::write(cgroup_root.join("memory.high"), "max\n").expect("root memory.high fixture");

    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        30 * gib
    );
}

#[test]
fn configured_override_cannot_exceed_the_cgroup_ceiling() {
    let gib = 1024 * 1024 * 1024;

    assert_eq!(
        process_resident_memory_limit_v1(88 * gib, Some(30 * gib), Some(bytes(64 * gib))).get(),
        30 * gib
    );
}

#[test]
fn unlimited_cgroup_memory_files_keep_host_memory_capacity() {
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("max\n"),
        Some("max\n"),
    );
    assert_eq!(
        effective_memory_bytes(88 * 1024 * 1024 * 1024, &proc_self_cgroup, &cgroup_root,),
        88 * 1024 * 1024 * 1024
    );
}

#[test]
fn finite_memory_max_bounds_host_memory_capacity() {
    let gib = 1024 * 1024 * 1024;
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("32212254720\n"),
        Some("max\n"),
    );
    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        30 * gib
    );
}

#[test]
fn finite_memory_high_below_max_is_the_effective_capacity() {
    let gib = 1024 * 1024 * 1024;
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("32212254720\n"),
        Some("25769803776\n"),
    );
    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        24 * gib
    );
}

#[test]
fn finite_ancestor_limit_bounds_an_unlimited_process_cgroup() {
    let gib = 1024 * 1024 * 1024;
    let (directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("max\n"),
        Some("max\n"),
    );
    fs::write(cgroup_root.join("trace.slice/memory.max"), "32212254720\n")
        .expect("ancestor memory.max fixture");
    fs::write(cgroup_root.join("trace.slice/memory.high"), "max\n")
        .expect("ancestor memory.high fixture");

    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        30 * gib
    );
    drop(directory);
}

#[test]
fn low_effective_cgroup_ceiling_engages_measured_pressure_before_the_cap() {
    let mib = 1024 * 1024;
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("134217728\n"),
        Some("100663296\n"),
    );
    let effective = effective_memory_bytes(8 * 1024 * mib, &proc_self_cgroup, &cgroup_root);
    let limit = process_resident_memory_limit_for_system_v1(effective);
    let pressure = Arc::new(ResidentMemoryPressureV1::new(limit));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::clone(&pressure),
    ));

    assert_eq!(effective, 96 * mib);
    assert_eq!(limit.get(), 72 * mib);
    assert!(pressure.high_watermark_bytes() < effective);
    assert!(
        pressure
            .publish_observed_resident_bytes(pressure.high_watermark_bytes())
            .is_over_budget()
    );
    assert!(
        authority
            .reserve(
                key("project-a", "worktree-a", "generation-a", "canonical"),
                growth_request(),
            )
            .expect_err("cgroup-bounded pressure must refuse growth")
            .is_observed_over_budget()
    );
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
        ResidentMemoryAdmissionFailureV1::ReservationCeiling {
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
        ResidentMemoryAdmissionFailureV1::ReservationCeiling {
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

/// A one-gigabyte authority whose measured-RSS cell is fed by the test rather
/// than by `/proc`. Production wires the same cell to the daemon's existing
/// `VmRSS` sampler; nothing here reads the filesystem.
const PRESSURE_TEST_LIMIT_BYTES: u64 = 1024 * 1024 * 1024;

fn pressure_authority() -> (Arc<ProcessResidentMemoryV1>, Arc<ResidentMemoryPressureV1>) {
    let limit = bytes(PRESSURE_TEST_LIMIT_BYTES);
    let pressure = Arc::new(ResidentMemoryPressureV1::new(limit));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        limit,
        Arc::clone(&pressure),
    ));
    (authority, pressure)
}

/// Comfortably above the admission floor, so refusal is about pressure rather
/// than about the request being small enough to always let through.
fn growth_request() -> NonZeroU64 {
    bytes(RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1 * 2)
}

#[test]
fn watermarks_derive_from_the_configured_limit_and_keep_low_below_high() {
    let limit = bytes(PRESSURE_TEST_LIMIT_BYTES);
    let pressure = ResidentMemoryPressureV1::new(limit);
    assert_eq!(
        pressure.high_watermark_bytes(),
        resident_memory_watermark_bytes_v1(
            limit,
            RESIDENT_MEMORY_PRESSURE_HIGH_WATERMARK_PERMILLE_V1
        )
    );
    assert_eq!(
        pressure.low_watermark_bytes(),
        resident_memory_watermark_bytes_v1(
            limit,
            RESIDENT_MEMORY_PRESSURE_LOW_WATERMARK_PERMILLE_V1
        )
    );
    assert!(pressure.low_watermark_bytes() < pressure.high_watermark_bytes());
    assert_eq!(pressure.state(), ResidentMemoryPressureStateV1::Unobserved);
}

#[test]
fn unobserved_rss_leaves_admission_on_the_reservation_ceiling_alone() {
    let (authority, _pressure) = pressure_authority();
    authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            growth_request(),
        )
        .expect("no measured sample means no measured refusal");
}

#[test]
fn measured_rss_above_the_high_watermark_refuses_growth_with_a_typed_state() {
    let (authority, pressure) = pressure_authority();
    let observed = pressure.high_watermark_bytes() + 1;
    assert!(
        pressure
            .publish_observed_resident_bytes(observed)
            .is_over_budget()
    );

    let failure = authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            growth_request(),
        )
        .expect_err("measured RSS over the high watermark refuses new growth");

    assert_eq!(
        failure,
        ResidentMemoryAdmissionFailureV1::ObservedOverBudget {
            observed_bytes: observed,
            limit_bytes: PRESSURE_TEST_LIMIT_BYTES,
            high_watermark_bytes: pressure.high_watermark_bytes(),
            requested_bytes: growth_request().get(),
            floor_bytes: RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1,
        }
    );
    assert!(failure.is_observed_over_budget());
    // The refusal names observed and configured bytes rather than stalling.
    let rendered = failure.to_string();
    assert!(rendered.contains(&observed.to_string()), "{rendered}");
    assert!(
        rendered.contains(&PRESSURE_TEST_LIMIT_BYTES.to_string()),
        "{rendered}"
    );
    // Reservations were never charged, so nothing leaked into the model.
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn process_shared_admission_refuses_under_the_same_measured_pressure() {
    let (authority, pressure) = pressure_authority();
    let component = ResidentMemoryComponentIdV1::new("sessions.codex.prepared-pages").unwrap();
    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes());

    let failure = authority
        .reserve_process_shared(component, growth_request())
        .expect_err("process-shared growth is refused under measured pressure");
    assert!(failure.is_observed_over_budget());
}

#[test]
fn admissions_at_or_below_the_floor_survive_measured_pressure() {
    let (authority, pressure) = pressure_authority();
    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes());

    authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            bytes(RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1),
        )
        .expect("floor-sized admissions keep the daemon serving under pressure");
}

#[test]
fn already_admitted_reservations_are_never_revoked_by_measured_pressure() {
    let (authority, pressure) = pressure_authority();
    let held = key("project-a", "worktree-a", "generation-a", "canonical");
    let reservation = authority
        .reserve(held.clone(), growth_request())
        .expect("admitted before pressure");

    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes() + 4096);

    assert_eq!(
        authority.snapshot().charge_for(&held),
        growth_request().get(),
        "pressure refuses new growth; it does not revoke live work"
    );
    assert_eq!(reservation.reserved_bytes(), growth_request().get());
    drop(reservation);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn measured_pressure_holds_between_watermarks_then_clears_at_the_low_watermark() {
    let (authority, pressure) = pressure_authority();
    let request = key("project-a", "worktree-a", "generation-a", "canonical");

    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes());
    assert!(
        authority
            .reserve(request.clone(), growth_request())
            .is_err(),
        "at the high watermark admission refuses"
    );

    // Hysteresis: between the watermarks the previous verdict stands, so a
    // sample that merely dips below high does not resume admitting.
    let between = u64::midpoint(
        pressure.low_watermark_bytes(),
        pressure.high_watermark_bytes(),
    );
    assert!(between > pressure.low_watermark_bytes());
    assert!(between < pressure.high_watermark_bytes());
    for _ in 0..4 {
        assert!(
            pressure
                .publish_observed_resident_bytes(between)
                .is_over_budget(),
            "state must not flap between the watermarks"
        );
        assert!(
            authority
                .reserve(request.clone(), growth_request())
                .is_err(),
            "admission must not flap between the watermarks"
        );
    }

    // Falling to the low watermark clears the latch and re-admits.
    assert!(
        !pressure
            .publish_observed_resident_bytes(pressure.low_watermark_bytes())
            .is_over_budget()
    );
    let readmitted = authority
        .reserve(request.clone(), growth_request())
        .expect("admission is retryable once measured pressure falls");
    drop(readmitted);

    // Climbing back through the middle does not re-latch either.
    for _ in 0..4 {
        assert!(
            !pressure
                .publish_observed_resident_bytes(between)
                .is_over_budget(),
            "a cleared latch must not re-arm between the watermarks"
        );
        authority
            .reserve(request.clone(), growth_request())
            .expect("still admitting between the watermarks after clearing");
    }
}

#[test]
fn reaching_the_high_watermark_runs_pressure_reclaimers_with_the_measurement() {
    let (_authority, pressure) = pressure_authority();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let callback_seen = Arc::clone(&seen);
    let _registration = pressure
        .register_pressure_reclaimer(
            10,
            Arc::new(move |request| {
                callback_seen.lock().expect("call log").push(request);
                4096
            }),
        )
        .expect("pressure reclaimer registration");

    // Below the watermark nothing is released.
    pressure.publish_observed_resident_bytes(pressure.low_watermark_bytes());
    assert!(seen.lock().expect("call log").is_empty());

    let observed = pressure.high_watermark_bytes() + 8192;
    pressure.publish_observed_resident_bytes(observed);
    let calls = seen.lock().expect("call log").clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].observed_bytes, observed);
    assert_eq!(calls[0].limit_bytes, PRESSURE_TEST_LIMIT_BYTES);
    assert_eq!(calls[0].excess_bytes, 8192);
}

#[test]
fn post_reclaim_observation_replaces_pressure_state_without_reentering_reclaimers() {
    let (authority, pressure) = pressure_authority();
    let calls = Arc::new(Mutex::new(0_u64));
    let callback_calls = Arc::clone(&calls);
    let callback_pressure = Arc::downgrade(&pressure);
    let after_reclaim = pressure.low_watermark_bytes();
    let _registration = pressure
        .register_pressure_reclaimer(
            10,
            Arc::new(move |_| {
                *callback_calls.lock().expect("call count") += 1;
                callback_pressure
                    .upgrade()
                    .expect("pressure authority remains live")
                    .publish_post_reclaim_observed_resident_bytes(after_reclaim);
                4096
            }),
        )
        .expect("pressure reclaimer registration");

    let state = pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes());

    assert_eq!(*calls.lock().expect("call count"), 1);
    assert_eq!(
        state,
        ResidentMemoryPressureStateV1::Nominal {
            observed_bytes: after_reclaim,
            limit_bytes: PRESSURE_TEST_LIMIT_BYTES,
            high_watermark_bytes: pressure.high_watermark_bytes(),
        },
        "admission must consume the observation measured after reclaim"
    );
    authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            growth_request(),
        )
        .expect("post-reclaim nominal RSS must immediately re-admit growth");
}

#[test]
fn dropped_pressure_reclaimer_registration_is_not_called() {
    let (_authority, pressure) = pressure_authority();
    let calls = Arc::new(Mutex::new(0_u64));
    let callback_calls = Arc::clone(&calls);
    let registration = pressure
        .register_pressure_reclaimer(
            10,
            Arc::new(move |_| {
                *callback_calls.lock().expect("call count") += 1;
                0
            }),
        )
        .expect("pressure reclaimer registration");
    drop(registration);

    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes());
    assert_eq!(*calls.lock().expect("call count"), 0);
}

#[test]
fn allocator_trim_reclaimer_runs_under_pressure_and_reports_only_measured_release() {
    let (_authority, pressure) = pressure_authority();
    let order = Arc::new(Mutex::new(Vec::new()));
    let state_order = Arc::clone(&order);
    let _state = pressure
        .register_pressure_reclaimer(
            10,
            Arc::new(move |_| {
                state_order.lock().expect("order").push("state");
                0
            }),
        )
        .expect("state reclaimer registration");
    let _trim = super::register_process_allocator_pressure_reclaimer_v1(&pressure)
        .expect("allocator trim registration");

    // Below the high watermark the trim never runs: freed pages are only
    // returned once measured RSS threatens admission.
    pressure.publish_observed_resident_bytes(pressure.low_watermark_bytes());
    assert!(order.lock().expect("order").is_empty());

    pressure.publish_observed_resident_bytes(pressure.high_watermark_bytes());
    assert_eq!(order.lock().expect("order").as_slice(), ["state"]);

    let trim = super::release_process_allocator_memory_v1();
    // A trim can only claim bytes the kernel surface measured on both sides.
    match (trim.before_bytes, trim.after_bytes) {
        (Some(before), Some(after)) => {
            assert_eq!(trim.released_bytes(), before.saturating_sub(after));
        }
        _ => assert_eq!(trim.released_bytes(), 0),
    }
}

#[test]
fn process_allocator_pressure_reclaimer_installs_once() {
    let first = super::install_process_allocator_pressure_reclaimer_v1()
        .expect("allocator pressure reclaimer installation");
    let second = super::install_process_allocator_pressure_reclaimer_v1()
        .expect("installed allocator pressure reclaimer remains available");
    assert!(!second, "a second install must be a no-op");
    // Another test in this process may have installed it first; either way
    // exactly one call reports the installation.
    let _ = first;
}

#[test]
fn allocator_pressure_reclaimer_installation_preserves_registration_failure() {
    let pressure = Arc::new(ResidentMemoryPressureV1::new(bytes(
        PRESSURE_TEST_LIMIT_BYTES,
    )));
    pressure.lock_state().next_sequence = u64::MAX;
    let registration = OnceLock::new();

    let failure =
        super::install_process_allocator_pressure_reclaimer_on_v1(&registration, &pressure)
            .expect_err("sequence exhaustion must not report an installed reclaimer");

    assert_eq!(failure, super::ResidentMemoryPressureRegistrationFailureV1);
    assert!(registration.get().is_some_and(Result::is_err));
    assert_eq!(
        super::install_process_allocator_pressure_reclaimer_on_v1(&registration, &pressure)
            .expect_err("a stored registration failure must remain truthful"),
        failure
    );
}
