use std::cell::Cell;
use std::fs;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, OnceLock};

use tracedecay_domain::{CodeGenerationId, ProjectId, WorktreeId};

use super::{
    CgroupMemoryCeilingV1, ProcessResidentMemoryV1, ProcessResidentPeakV1, ProcessResidentSampleV1,
    RESIDENT_MEMORY_PRESSURE_ADMISSION_FLOOR_BYTES_V1, ResidentMemoryAdmissionFailureV1,
    ResidentMemoryComponentIdV1, ResidentMemoryKeyV1, ResidentMemoryPressureStateV1,
    ResidentMemoryPressureV1, cgroup_service_ceiling_bytes, cgroup_v2_memory_ceiling_v1,
    effective_memory_bytes_v1, resident_memory_authority_v1,
};

fn bytes(value: u64) -> NonZeroU64 {
    NonZeroU64::new(value).expect("test byte count is non-zero")
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
        cgroup_v2_memory_ceiling_v1(proc_self_cgroup, cgroup_root)
            .and_then(cgroup_service_ceiling_bytes),
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

fn hard_ceiling(max_bytes: u64) -> CgroupMemoryCeilingV1 {
    CgroupMemoryCeilingV1 {
        max_bytes: Some(max_bytes),
        high_bytes: None,
    }
}

#[test]
fn configured_override_cannot_exceed_the_cgroup_ceiling() {
    let gib = 1024 * 1024 * 1024;
    let capped = resident_memory_authority_v1(
        88 * gib,
        Some(CgroupMemoryCeilingV1 {
            max_bytes: Some(30 * gib),
            high_bytes: Some(26 * gib),
        }),
        Some(bytes(64 * gib)),
    );

    assert_eq!(
        capped.limit_bytes.get(),
        30 * gib,
        "an override is capped by memory.max, not by the reclaim line"
    );
    assert_eq!(capped.reclaim_watermark_bytes, Some(26 * gib));
}

#[test]
fn cgroup_service_allowance_is_not_discounted_twice() {
    let gib = 1024 * 1024 * 1024;
    let only_high = resident_memory_authority_v1(
        128 * gib,
        Some(CgroupMemoryCeilingV1 {
            max_bytes: None,
            high_bytes: Some(26 * gib),
        }),
        None,
    );
    assert_eq!(
        only_high.limit_bytes.get(),
        26 * gib,
        "a lone memory.high is the service ceiling and is not quartered again"
    );
    assert_eq!(only_high.reclaim_watermark_bytes, None);

    let small_host = resident_memory_authority_v1(16 * gib, Some(hard_ceiling(30 * gib)), None);
    assert_eq!(
        small_host.limit_bytes.get(),
        12 * gib,
        "a larger cgroup must not erase the physical-host reserve"
    );
    assert_eq!(small_host.reclaim_watermark_bytes, None);
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
fn memory_high_does_not_replace_memory_max_as_the_hard_capacity() {
    let gib = 1024 * 1024 * 1024;
    let (_directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("32212254720\n"),
        Some("25769803776\n"),
    );
    assert_eq!(
        effective_memory_bytes(88 * gib, &proc_self_cgroup, &cgroup_root),
        30 * gib,
        "memory.max is the kernel kill line"
    );
    let ceiling = cgroup_v2_memory_ceiling_v1(&proc_self_cgroup, &cgroup_root).expect("cgroup");
    let authority = resident_memory_authority_v1(88 * gib, Some(ceiling), None);
    assert_eq!(authority.limit_bytes.get(), 30 * gib);
    assert_eq!(authority.reclaim_watermark_bytes, Some(24 * gib));
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

/// The slice owns `memory.max` and the service owns `memory.high`.
///
/// On a 128 GiB host those are 30 GiB and 26 GiB. RSS at 24 GiB is still under
/// the reclaim line, so the authority admits growth instead of latching at a
/// percentage of a ceiling the operator never set.
#[test]
fn slice_max_and_service_high_keep_the_reclaim_band_usable() {
    let gib = 1024 * 1024 * 1024;
    let (directory, proc_self_cgroup, cgroup_root) = cgroup_fixture(
        Some("0::/trace.slice/daemon.scope\n"),
        Some("max\n"),
        Some(&format!("{}\n", 26 * gib)),
    );
    fs::write(
        cgroup_root.join("trace.slice/memory.max"),
        format!("{}\n", 30 * gib),
    )
    .expect("ancestor memory.max fixture");
    fs::write(cgroup_root.join("trace.slice/memory.high"), "max\n")
        .expect("ancestor memory.high fixture");

    let ceiling = cgroup_v2_memory_ceiling_v1(&proc_self_cgroup, &cgroup_root).expect("cgroup");
    let detected = resident_memory_authority_v1(128 * gib, Some(ceiling), None);
    let pressure = Arc::new(ResidentMemoryPressureV1::with_reclaim_line(
        detected.limit_bytes,
        detected.reclaim_watermark_bytes,
        Arc::new(|| None),
    ));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        detected.limit_bytes,
        Arc::clone(&pressure),
    ));

    assert_eq!(detected.limit_bytes.get(), 30 * gib);
    assert_eq!(pressure.high_watermark_bytes(), 26 * gib);
    assert_eq!(
        pressure.low_watermark_bytes(),
        23_264_406_186,
        "hysteresis stays at 750/900 of memory.high, not 75% of memory.max"
    );
    assert!(
        !pressure
            .publish_observed_resident_bytes(22 * gib)
            .is_over_budget()
    );
    assert!(
        !pressure
            .publish_observed_resident_bytes(24 * gib)
            .is_over_budget(),
        "rss under memory.high is not over budget"
    );
    authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "text-build"),
            bytes(3 * gib),
        )
        .expect("the process authority admits a 3 GiB reservation at 24 GiB RSS");

    // Text-artifact admission spends the band down to the reclaim line, never
    // down to memory.max: `text_artifact_admitted_build_budget` subtracts the
    // same watermark headroom it charges, so its growth budget reduces to
    // `high_watermark - observed`. At 24 GiB observed that is 2 GiB, clearing
    // the 1536 MiB builder floor. The 90%-of-26 GiB watermark left 0 and
    // deadlocked the replacement build.
    let headroom = detected
        .limit_bytes
        .get()
        .saturating_sub(pressure.high_watermark_bytes());
    let available_for_growth = detected
        .limit_bytes
        .get()
        .saturating_sub(24 * gib)
        .saturating_sub(headroom);
    assert_eq!(available_for_growth, 2 * gib);
    assert!(available_for_growth >= 1536 * 1024 * 1024);

    assert!(
        pressure
            .publish_observed_resident_bytes(26 * gib)
            .is_over_budget()
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
    let ceiling = cgroup_v2_memory_ceiling_v1(&proc_self_cgroup, &cgroup_root).expect("cgroup");
    let detected = resident_memory_authority_v1(8 * 1024 * mib, Some(ceiling), None);
    let pressure = Arc::new(ResidentMemoryPressureV1::with_reclaim_line(
        detected.limit_bytes,
        detected.reclaim_watermark_bytes,
        Arc::new(|| None),
    ));
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        detected.limit_bytes,
        Arc::clone(&pressure),
    ));

    assert_eq!(detected.limit_bytes.get(), 128 * mib);
    assert_eq!(pressure.high_watermark_bytes(), 96 * mib);
    assert!(pressure.high_watermark_bytes() < detected.limit_bytes.get());
    assert!(
        !pressure
            .publish_observed_resident_bytes(95 * mib)
            .is_over_budget(),
        "rss below memory.high is still under the hard ceiling"
    );
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
fn keyed_and_process_shared_reservations_compete_for_one_ceiling() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let keyed = key("project-a", "worktree-a", "generation-a", "code-index");
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
fn additional_reservations_share_identity_but_charge_and_release_independently() {
    let authority = Arc::new(ProcessResidentMemoryV1::with_pressure(
        bytes(100),
        Arc::new(ResidentMemoryPressureV1::with_sampler(
            bytes(100),
            Arc::new(|| {
                Some(ProcessResidentSampleV1 {
                    resident_bytes: 0,
                    unreclaimable_bytes: 0,
                })
            }),
        )),
    ));
    let owner = key("project-a", "worktree-a", "generation-a", "reader");
    let baseline = authority
        .reserve(owner.clone(), bytes(60))
        .expect("baseline");
    let additional = baseline
        .reserve_additional(bytes(40))
        .expect("overlap charge");
    assert_eq!(authority.snapshot().charge_for(&owner), 100);
    assert!(baseline.reserve_additional(bytes(1)).is_err());
    drop(additional);
    assert_eq!(authority.snapshot().charge_for(&owner), 60);
    let next = baseline
        .reserve_additional(bytes(40))
        .expect("released capacity reused");
    drop(baseline);
    assert_eq!(authority.snapshot().charge_for(&owner), 40);
    drop(next);
    assert_eq!(authority.snapshot().used_bytes, 0);
}

#[test]
fn transfer_keeps_retained_bytes_when_a_new_admission_cannot() {
    let authority = Arc::new(ProcessResidentMemoryV1::new(bytes(100)));
    let build = key(
        "project-a",
        "worktree-a",
        "generation-a",
        "code-text-artifact-build",
    );
    let reader = key(
        "project-a",
        "worktree-a",
        "generation-a",
        "code-text-artifact-reader",
    );
    let mut held = authority
        .reserve(build.clone(), bytes(80))
        .expect("build reservation");
    let _neighbor = authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "graph"),
            bytes(20),
        )
        .expect("neighbor fills the ceiling");
    let denied = authority
        .reserve(reader.clone(), bytes(30))
        .expect_err("a fresh reader admission does not fit beside the held build charge");
    assert!(matches!(
        denied,
        ResidentMemoryAdmissionFailureV1::ReservationCeiling { .. }
    ));

    held.transfer_component(reader.component, 30)
        .expect("the held charge moves without a new admission");
    assert_eq!(held.key(), &reader);
    assert_eq!(held.reserved_bytes(), 30);
    let snapshot = authority.snapshot();
    assert_eq!(snapshot.used_bytes, 50);
    assert_eq!(snapshot.charge_for(&build), 0);
    assert_eq!(snapshot.charge_for(&reader), 30);

    let grown = held.transfer_component(reader.component, 40);
    assert!(grown.is_err(), "a transfer cannot grow the held charge");
    assert_eq!(held.reserved_bytes(), 30);
    assert_eq!(authority.snapshot().charge_for(&reader), 30);

    drop(held);
    assert_eq!(authority.snapshot().used_bytes, 20);
    assert_eq!(authority.snapshot().charge_for(&reader), 0);
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
fn nominal_rss_refuses_growth_that_would_cross_the_limit() {
    let (authority, pressure) = pressure_authority();
    let requested = bytes(PRESSURE_TEST_LIMIT_BYTES / 4);
    let observed = pressure.limit_bytes() - requested.get() + 1;
    assert!(
        !pressure
            .publish_observed_resident_bytes(observed)
            .is_over_budget()
    );

    let failure = authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            requested,
        )
        .expect_err("growth must fit measured headroom before allocation");
    assert!(failure.is_observed_over_budget());
    let failure = authority
        .reserve_process_shared(
            ResidentMemoryComponentIdV1::new("sessions.codex.prepared-pages").unwrap(),
            requested,
        )
        .expect_err("shared growth must fit the same measured headroom");
    assert!(failure.is_observed_over_budget());
    assert_eq!(authority.snapshot().used_bytes, 0);

    pressure.publish_observed_resident_bytes(observed - 1);
    authority
        .reserve(
            key("project-a", "worktree-a", "generation-a", "canonical"),
            requested,
        )
        .expect("growth that fits the headroom can proceed");
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

#[test]
fn psi_some_avg10_reads_the_memory_stall_share() {
    let pressure = "some avg10=12.50 avg60=3.10 avg300=0.80 total=123456\n\
                    full avg10=4.00 avg60=1.00 avg300=0.20 total=45678\n";
    assert_eq!(super::psi_some_avg10_v1(pressure), Some(12.5));
    assert_eq!(
        super::psi_some_avg10_v1("full avg10=4.00 avg60=1.00\n"),
        None
    );
}

thread_local! {
    static INSTALLED_RELEASES: Cell<usize> = const { Cell::new(0) };
}

fn count_installed_release() {
    INSTALLED_RELEASES.with(|count| count.set(count.get() + 1));
}

/// The allocator the composition root installed is the one released: a
/// mimalloc daemon asked glibc's `malloc_trim`, which returns nothing from
/// mimalloc's pages. Installation happens once; a second is refused.
#[test]
fn allocator_release_runs_the_installed_allocator_release() {
    super::install_process_allocator_release_v1(count_installed_release)
        .expect("first installation");
    let before = INSTALLED_RELEASES.with(Cell::get);
    let trim = super::release_process_allocator_memory_v1();
    assert_eq!(INSTALLED_RELEASES.with(Cell::get), before + 1);
    assert!(trim.trimmed);
    assert_eq!(
        super::install_process_allocator_release_v1(count_installed_release),
        Err("the process allocator release is already installed".to_owned())
    );
}

#[test]
fn process_status_splits_clean_file_pages_from_unreclaimable_bytes() {
    let status = "Name:\ttracedecay\nVmHWM:\t 6553600 kB\nVmRSS:\t 3355444 kB\n\
                  RssAnon:\t 2528172 kB\nRssFile:\t  807272 kB\nRssShmem:\t   20000 kB\n";
    assert_eq!(
        super::process_resident_sample_from_status_v1(status),
        Some(super::ProcessResidentSampleV1 {
            resident_bytes: 3_355_444 * 1024,
            unreclaimable_bytes: 2_548_172 * 1024,
        })
    );
    assert_eq!(
        super::process_resident_sample_from_status_v1("VmRSS:\t 1024 kB\n"),
        None,
        "a kernel without split RSS counters is unobserved, not zero"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_resident_peak_reports_growth_a_pass_touched_and_released() {
    const TOUCHED_BYTES: usize = 128 * 1024 * 1024;
    let peak = ProcessResidentPeakV1::start()
        .expect("sampler starts")
        .expect("linux reports a resident set");
    let touched = vec![1_u8; TOUCHED_BYTES];
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(std::hint::black_box(touched));
    let growth = peak.finish().expect("sampler joins");
    assert!(
        growth >= (TOUCHED_BYTES / 2) as u64,
        "a pass that touched {TOUCHED_BYTES} bytes reported {growth} bytes of growth"
    );
}
