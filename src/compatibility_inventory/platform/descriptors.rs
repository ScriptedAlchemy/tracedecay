use super::super::model::PlatformDispositionV1;

pub(super) const PLATFORM_COUNT: usize = 4;

#[derive(Clone, Copy)]
pub(super) struct PlatformSpec {
    pub(super) name: &'static str,
    pub(super) receipt: LaneReceipt,
}

#[derive(Clone, Copy)]
pub(super) struct LaneReceipt {
    pub(super) id: &'static str,
    pub(super) target: &'static str,
    pub(super) cfg: &'static str,
    pub(super) executed_tests: &'static [&'static str],
    pub(super) ignored_tests: &'static [&'static str],
    pub(super) substitutions: &'static [&'static str],
}

#[derive(Clone, Copy)]
pub(super) struct CfgExclusion {
    pub(super) reference: &'static str,
    pub(super) ignored_platforms: &'static [&'static str],
}

#[derive(Clone)]
pub(super) struct OperationSpec {
    pub(super) id: &'static str,
    pub(super) production_owner: &'static str,
    pub(super) dispositions: [PlatformDispositionV1; PLATFORM_COUNT],
    pub(super) tests: [&'static [&'static str]; PLATFORM_COUNT],
    pub(super) substitute_refs: [&'static str; PLATFORM_COUNT],
    pub(super) recovery: [&'static str; PLATFORM_COUNT],
    pub(super) parity_gate: &'static str,
    pub(super) cutover_gate: [&'static str; PLATFORM_COUNT],
    pub(super) broad_cfg_exclusion: &'static str,
}

const S: PlatformDispositionV1 = PlatformDispositionV1::Supported;
const A: PlatformDispositionV1 = PlatformDispositionV1::Alternative;
const U: PlatformDispositionV1 = PlatformDispositionV1::Unavailable;
pub(super) const NONE: &[&str] = &[];

const LINUX_SERVICE_TESTS: &[&str] = &[
    "test:tracedecay::daemon::service::tests::service_status_includes_journalctl_debug_command",
    "test:tracedecay::daemon::service::tests::user_service_runs_daemon_with_socket_path",
];
const MACOS_SERVICE_TESTS: &[&str] = &[
    "test:tracedecay::daemon::service::tests::launchd_command_plans_map_start_and_uninstall_sequences",
    "test:tracedecay::daemon::service::tests::service_status_includes_launchd_debug_commands",
];
const LOCK_TESTS: &[&str] = &[
    "test:tracedecay::lifecycle_lease::tests::exclusive_lease_rejects_a_concurrent_mutator",
    "test:tracedecay::lifecycle_lease::tests::shared_doctor_lease_blocks_mutation_but_not_another_reader",
];
const WINDOWS_LOCK_TESTS: &[&str] = &[
    "test:tracedecay::lifecycle_lease::tests::exclusive_lease_rejects_a_concurrent_mutator",
    "test:tracedecay::lifecycle_lease::tests::post_update_child_never_trusts_a_matching_windows_sidecar_token",
];
const LINUX_HOLDER_TESTS: &[&str] = &[
    "test:tracedecay::open_store_holders::tests::linux_scan_ignores_its_own_pid_and_unrelated_files",
    "test:tracedecay::open_store_holders::tests::linux_scan_matches_open_sidecars_by_file_identity",
];
const MACOS_HOLDER_TESTS: &[&str] = &[
    "test:tracedecay::open_store_holders::lsof_tests::lsof_field_output_is_bounded_to_targets_and_excludes_self",
    "test:tracedecay::open_store_holders::lsof_tests::lsof_field_output_preserves_non_utf8_and_newline_paths",
];
const PATH_TESTS: &[&str] = &[
    "test:tracedecay::daemon::service::tests::default_socket_path_is_profile_scoped_not_project_scoped",
];
const PLAN_TESTS: &[&str] = &[
    "test:tracedecay::migrate::consolidate::tests::dry_run_reports_live_split_shape_without_mutation",
];
const APPLY_TESTS: &[&str] = &[
    "test:tracedecay::migrate::consolidate::tests::consolidation_restarts_after_every_durable_state",
    "test:tracedecay::migrate::consolidate::tests::interrupted_apply_retries_without_duplicates_and_cuts_over_last",
];
const BACKUP_TESTS: &[&str] = &[
    "test:tracedecay::migrate::consolidate::tests::interrupted_apply_retries_without_duplicates_and_cuts_over_last",
];
const RECOVERY_TESTS: &[&str] = &[
    "test:tracedecay::doctor::tests::database_recovery_guidance_names_the_preserved_recovery_set",
    "test:tracedecay::migrate::consolidate::tests::consolidation_restarts_after_every_durable_state",
];
const CORE_DAEMON_TESTS: &[&str] = &["test:core_cli_suite::tool_daemon_test::*"];
const PR_AUTOTRACK_TESTS: &[&str] = &["test:daemon_suite::pr_autotrack_test::*"];

const TEST_SCAN_SUBSTITUTE: &str = concat!(
    "substitute:src/open_store_holders.rs#scan:cfg(test):",
    "behavior=Supported(empty):production_reachable=false"
);
const WINDOWS_OFFLINE_SUBSTITUTE: &str = concat!(
    "substitute:src/migrate/consolidate/sqlite/inspect.rs#acquire_offline_guards:",
    "cfg(all(test,windows)):behavior=no-op:production_reachable=false"
);

const LINUX_EXECUTED_TESTS: &[&str] = &[
    "test:core_cli_suite::tool_daemon_test::*",
    "test:daemon_suite::pr_autotrack_test::*",
    "test:tracedecay::daemon::service::tests::service_status_includes_journalctl_debug_command",
    "test:tracedecay::daemon::service::tests::user_service_runs_daemon_with_socket_path",
    "test:tracedecay::doctor::tests::database_recovery_guidance_names_the_preserved_recovery_set",
    "test:tracedecay::lifecycle_lease::tests::exclusive_lease_rejects_a_concurrent_mutator",
    "test:tracedecay::lifecycle_lease::tests::shared_doctor_lease_blocks_mutation_but_not_another_reader",
    "test:tracedecay::migrate::consolidate::tests::consolidation_restarts_after_every_durable_state",
    "test:tracedecay::migrate::consolidate::tests::dry_run_reports_live_split_shape_without_mutation",
    "test:tracedecay::migrate::consolidate::tests::interrupted_apply_retries_without_duplicates_and_cuts_over_last",
    "test:tracedecay::open_store_holders::tests::linux_scan_ignores_its_own_pid_and_unrelated_files",
    "test:tracedecay::open_store_holders::tests::linux_scan_matches_open_sidecars_by_file_identity",
    "test:tracedecay::daemon::service::tests::default_socket_path_is_profile_scoped_not_project_scoped",
];

const MACOS_EXECUTED_TESTS: &[&str] = &[
    "test:core_cli_suite::tool_daemon_test::*",
    "test:daemon_suite::pr_autotrack_test::*",
    "test:tracedecay::daemon::service::tests::default_socket_path_is_profile_scoped_not_project_scoped",
    "test:tracedecay::daemon::service::tests::launchd_command_plans_map_start_and_uninstall_sequences",
    "test:tracedecay::daemon::service::tests::service_status_includes_launchd_debug_commands",
    "test:tracedecay::doctor::tests::database_recovery_guidance_names_the_preserved_recovery_set",
    "test:tracedecay::lifecycle_lease::tests::exclusive_lease_rejects_a_concurrent_mutator",
    "test:tracedecay::lifecycle_lease::tests::shared_doctor_lease_blocks_mutation_but_not_another_reader",
    "test:tracedecay::migrate::consolidate::tests::consolidation_restarts_after_every_durable_state",
    "test:tracedecay::migrate::consolidate::tests::dry_run_reports_live_split_shape_without_mutation",
    "test:tracedecay::migrate::consolidate::tests::interrupted_apply_retries_without_duplicates_and_cuts_over_last",
    "test:tracedecay::open_store_holders::lsof_tests::lsof_field_output_is_bounded_to_targets_and_excludes_self",
    "test:tracedecay::open_store_holders::lsof_tests::lsof_field_output_preserves_non_utf8_and_newline_paths",
];

const WINDOWS_EXECUTED_TESTS: &[&str] = &[
    "test:tracedecay::daemon::service::tests::default_socket_path_is_profile_scoped_not_project_scoped",
    "test:tracedecay::doctor::tests::database_recovery_guidance_names_the_preserved_recovery_set",
    "test:tracedecay::lifecycle_lease::tests::exclusive_lease_rejects_a_concurrent_mutator",
    "test:tracedecay::lifecycle_lease::tests::post_update_child_never_trusts_a_matching_windows_sidecar_token",
    "test:tracedecay::migrate::consolidate::tests::consolidation_restarts_after_every_durable_state",
    "test:tracedecay::migrate::consolidate::tests::dry_run_reports_live_split_shape_without_mutation",
    "test:tracedecay::migrate::consolidate::tests::interrupted_apply_retries_without_duplicates_and_cuts_over_last",
];

const WINDOWS_IGNORED_TESTS: &[&str] = &[CORE_DAEMON_TESTS[0], PR_AUTOTRACK_TESTS[0]];
const KNOWN_SUBSTITUTIONS: &[&str] = &[TEST_SCAN_SUBSTITUTE, WINDOWS_OFFLINE_SUBSTITUTE];

pub(super) const PLATFORMS: [PlatformSpec; PLATFORM_COUNT] = [
    PlatformSpec {
        name: "linux",
        receipt: LaneReceipt {
            id: "ci-linux-full",
            target: "x86_64-unknown-linux-gnu",
            cfg: "target_os=linux",
            executed_tests: LINUX_EXECUTED_TESTS,
            ignored_tests: NONE,
            substitutions: &[TEST_SCAN_SUBSTITUTE],
        },
    },
    PlatformSpec {
        name: "macos",
        receipt: LaneReceipt {
            id: "ci-macos-full",
            target: "aarch64-apple-darwin",
            cfg: "target_os=macos",
            executed_tests: MACOS_EXECUTED_TESTS,
            ignored_tests: NONE,
            substitutions: &[TEST_SCAN_SUBSTITUTE],
        },
    },
    PlatformSpec {
        name: "windows",
        receipt: LaneReceipt {
            id: "ci-windows-shards-1-through-5",
            target: "x86_64-pc-windows-msvc",
            cfg: "target_os=windows",
            executed_tests: WINDOWS_EXECUTED_TESTS,
            ignored_tests: WINDOWS_IGNORED_TESTS,
            substitutions: KNOWN_SUBSTITUTIONS,
        },
    },
    PlatformSpec {
        name: "other",
        receipt: LaneReceipt {
            id: "none",
            target: "unsupported",
            cfg: "not(any(target_os=linux,target_os=macos,target_os=windows))",
            executed_tests: NONE,
            ignored_tests: WINDOWS_IGNORED_TESTS,
            substitutions: &[TEST_SCAN_SUBSTITUTE],
        },
    },
];

pub(super) const CFG_EXCLUSIONS: &[CfgExclusion] = &[
    CfgExclusion {
        reference: "broad-cfg-exclusion:tests/core_cli_suite/main.rs:cfg(unix):tool_daemon_test",
        ignored_platforms: &["other", "windows"],
    },
    CfgExclusion {
        reference: "broad-cfg-exclusion:tests/daemon_suite/main.rs:cfg(unix):pr_autotrack_test",
        ignored_platforms: &["other", "windows"],
    },
];

pub(super) const OPERATIONS: &[OperationSpec] = &[
    OperationSpec {
        id: "backup-create-and-verify",
        production_owner: "src/migrate/consolidate/prepare.rs#prepare_backups",
        dispositions: [S, S, A, U],
        tests: [BACKUP_TESTS, BACKUP_TESTS, BACKUP_TESTS, NONE],
        substitute_refs: ["", "", WINDOWS_OFFLINE_SUBSTITUTE, ""],
        recovery: [
            "retain both sources and restart from BackupsReady",
            "retain both sources and restart from BackupsReady",
            "run only after an operator proves all stores offline",
            "stop because directory durability is unproven",
        ],
        parity_gate: "PR3-BACKUP-EVIDENCE",
        cutover_gate: [
            "PR33R-BACKUP",
            "PR33R-BACKUP",
            "PR33R-OFFLINE-ONLY",
            "PR33R-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "consolidation-apply",
        production_owner: "src/migrate/consolidate/mod.rs#apply_with_faults",
        dispositions: [S, S, U, U],
        tests: [APPLY_TESTS, APPLY_TESTS, APPLY_TESTS, NONE],
        substitute_refs: ["", "", WINDOWS_OFFLINE_SUBSTITUTE, ""],
        recovery: [
            "resume the durable consolidation ledger",
            "resume the durable consolidation ledger",
            "stop because production holder discovery is unavailable",
            "stop because holder discovery and durability are unavailable",
        ],
        parity_gate: "PR3-CONSOLIDATION-EVIDENCE",
        cutover_gate: ["PR33R-APPLY", "PR33R-APPLY", "PR33R-BLOCK", "PR33R-BLOCK"],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "daemon-service-install-refresh-status",
        production_owner: "src/daemon/service.rs#install_service",
        dispositions: [S, S, U, U],
        tests: [LINUX_SERVICE_TESTS, MACOS_SERVICE_TESTS, NONE, NONE],
        substitute_refs: ["", "", "", ""],
        recovery: [
            "restore prior systemd enabled, masked, and stopped state",
            "restore prior launchd loaded, disabled, and stopped state",
            "use the foreground daemon because Windows service install is unavailable",
            "use the foreground daemon because no service manager is supported",
        ],
        parity_gate: "PR3-SERVICE-EVIDENCE",
        cutover_gate: [
            "PR24E0-SYSTEMD",
            "PR24E0-LAUNCHD",
            "PR24E0-WINDOWS-SERVICE",
            "PR24E0-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "directory-durability",
        production_owner: "src/migrate/consolidate/files.rs#sync_parent_directory",
        dispositions: [S, S, A, U],
        tests: [APPLY_TESTS, APPLY_TESTS, APPLY_TESTS, NONE],
        substitute_refs: ["", "", "", ""],
        recovery: [
            "resume from the last fsynced ledger state",
            "resume from the last fsynced ledger state",
            "reverify files because parent-directory fsync is unavailable",
            "stop because durable publication is unproven",
        ],
        parity_gate: "PR3-DURABILITY-EVIDENCE",
        cutover_gate: [
            "PR33R-DURABILITY",
            "PR33R-DURABILITY",
            "PR33R-FILE-SYNC",
            "PR33R-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "lifecycle-exclusive-lock",
        production_owner: "src/lifecycle_lease.rs#acquire_exclusive_or_inherited",
        dispositions: [S, S, S, U],
        tests: [LOCK_TESTS, LOCK_TESTS, WINDOWS_LOCK_TESTS, NONE],
        substitute_refs: ["", "", "", ""],
        recovery: [
            "reject stale PID and start identity then reacquire a fresh OS lock",
            "reject stale PID and start identity then reacquire a fresh OS lock",
            "ignore diagnostic sidecar authority then reacquire a fresh OS lock",
            "stop until an OS lock implementation is qualified",
        ],
        parity_gate: "PR3-LIFECYCLE-LOCK-EVIDENCE",
        cutover_gate: [
            "PR24E0-FRESH-LOCK",
            "PR24E0-FRESH-LOCK",
            "PR24E0-FRESH-LOCK",
            "PR24E0-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "migration-plan",
        production_owner: "src/migrate/consolidate/mod.rs#plan",
        dispositions: [S, S, A, U],
        tests: [PLAN_TESTS, PLAN_TESTS, PLAN_TESTS, NONE],
        substitute_refs: ["", "", TEST_SCAN_SUBSTITUTE, ""],
        recovery: [
            "discard the immutable plan and inspect again",
            "discard the immutable plan and inspect again",
            "inspect only; apply remains blocked without holder evidence",
            "emit inventory only and do not authorize apply",
        ],
        parity_gate: "PR3-MIGRATION-PLAN-EVIDENCE",
        cutover_gate: [
            "PR33R-PLAN",
            "PR33R-PLAN",
            "PR33R-INSPECT-ONLY",
            "PR33R-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "open-store-holder-scan",
        production_owner: "src/open_store_holders.rs#scan",
        dispositions: [S, S, U, U],
        tests: [LINUX_HOLDER_TESTS, MACOS_HOLDER_TESTS, NONE, NONE],
        substitute_refs: [
            TEST_SCAN_SUBSTITUTE,
            TEST_SCAN_SUBSTITUTE,
            TEST_SCAN_SUBSTITUTE,
            TEST_SCAN_SUBSTITUTE,
        ],
        recovery: [
            "stop reported holders and rescan",
            "stop reported holders and rescan",
            "stop because no fail-closed production holder discovery exists",
            "stop because no fail-closed production holder discovery exists",
        ],
        parity_gate: "PR3-HOLDER-SCAN-EVIDENCE",
        cutover_gate: [
            "PR33R-NO-HOLDERS",
            "PR33R-NO-HOLDERS",
            "PR33R-BLOCK",
            "PR33R-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "profile-service-and-socket-paths",
        production_owner: "src/daemon/service.rs#default_socket_path",
        dispositions: [S, S, S, U],
        tests: [PATH_TESTS, PATH_TESTS, PATH_TESTS, NONE],
        substitute_refs: ["", "", "", ""],
        recovery: [
            "reresolve the profile-scoped XDG path",
            "reresolve the profile-scoped Application Support path",
            "reresolve the profile-scoped APPDATA path",
            "stop because no canonical platform path is qualified",
        ],
        parity_gate: "PR3-PLATFORM-PATH-EVIDENCE",
        cutover_gate: ["PR24E0-PATH", "PR24E0-PATH", "PR24E0-PATH", "PR24E0-BLOCK"],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "recovery-and-doctor",
        production_owner: "src/doctor.rs#database_recovery_guidance",
        dispositions: [S, S, S, U],
        tests: [RECOVERY_TESTS, RECOVERY_TESTS, RECOVERY_TESTS, NONE],
        substitute_refs: ["", "", WINDOWS_OFFLINE_SUBSTITUTE, ""],
        recovery: [
            "preserve database, WAL, SHM, dirty markers, and ledger",
            "preserve database, WAL, SHM, dirty markers, and ledger",
            "preserve database, WAL, SHM, dirty markers, and ledger",
            "report preserved inputs without automatic repair",
        ],
        parity_gate: "PR3-RECOVERY-EVIDENCE",
        cutover_gate: [
            "PR33R-RECOVERY",
            "PR33R-RECOVERY",
            "PR33R-RECOVERY",
            "PR33R-BLOCK",
        ],
        broad_cfg_exclusion: "",
    },
    OperationSpec {
        id: "test-suite-core-cli-tool-daemon",
        production_owner: "tests/core_cli_suite/main.rs#tool_daemon_test",
        dispositions: [S, S, U, U],
        tests: [CORE_DAEMON_TESTS, CORE_DAEMON_TESTS, NONE, NONE],
        substitute_refs: ["", "", "", ""],
        recovery: [
            "run the complete test module",
            "run the complete test module",
            "replace Unix socket assumptions with named pipe coverage",
            "add a supported daemon transport before claiming coverage",
        ],
        parity_gate: "PR3-NO-BROAD-TEST-EXCLUSION",
        cutover_gate: [
            "PR3-PLATFORM-COMPLETE",
            "PR3-PLATFORM-COMPLETE",
            "PR3-EXPLICIT-GAP",
            "PR3-EXPLICIT-GAP",
        ],
        broad_cfg_exclusion: "broad-cfg-exclusion:tests/core_cli_suite/main.rs:cfg(unix):tool_daemon_test",
    },
    OperationSpec {
        id: "test-suite-daemon-pr-autotrack",
        production_owner: "tests/daemon_suite/main.rs#pr_autotrack_test",
        dispositions: [S, S, U, U],
        tests: [PR_AUTOTRACK_TESTS, PR_AUTOTRACK_TESTS, NONE, NONE],
        substitute_refs: ["", "", "", ""],
        recovery: [
            "run the complete test module",
            "run the complete test module",
            "port filesystem and process notification coverage",
            "add a supported watcher before claiming coverage",
        ],
        parity_gate: "PR3-NO-BROAD-TEST-EXCLUSION",
        cutover_gate: [
            "PR3-PLATFORM-COMPLETE",
            "PR3-PLATFORM-COMPLETE",
            "PR3-EXPLICIT-GAP",
            "PR3-EXPLICIT-GAP",
        ],
        broad_cfg_exclusion: "broad-cfg-exclusion:tests/daemon_suite/main.rs:cfg(unix):pr_autotrack_test",
    },
];
