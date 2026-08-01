# `tracedecay-global-db` — unresolved root seams

Produced by the one-shot crate split (`docs/superpowers/plans/2026-07-31-one-shot-crate-split.md`).
`src/global_db{.rs,/}` moved to `crates/tracedecay-global-db` **as-is**. Every
reference below still spells a root-crate path (`crate::db::…`, `crate::errors::…`,
…) that no longer resolves inside the extracted crate. Per the split doctrine
these are **deliberately unsolved**: the kernel mover is extracting
`errors, types, timeutil, storage, db, store, …` into `tracedecay-runtime-core`
in parallel, and the lead repoints them at integration.

Nothing here is a behavior change — the seams are textual and the compiler
catches every one of them.

## How to repoint

The names were left verbatim so the lead can bulk-rewrite by prefix, e.g.

    crate::db::      -> tracedecay_runtime_core::db::
    crate::errors::  -> tracedecay_runtime_core::errors::
    crate::storage:: -> tracedecay_runtime_core::storage::

after which `crates/tracedecay-global-db/Cargo.toml` needs the matching
dependency edges (it currently declares only the crates the moved code already
reached by workspace path).

## Status at hand-off

`cargo check -p tracedecay-global-db` → **498 errors**, all of them the seams
catalogued here. Every error class that was *not* a root seam has been fixed in
place (see "Cheap wins already applied").

## Priority seams

1. **`crate::project_registry` is now ambiguous.** The moved crate has its own
   `project_registry` module (`crates/tracedecay-global-db/src/project_registry.rs`),
   so the four references that meant the **root** `src/project_registry.rs`
   silently retarget the crate-local module. They currently fail to compile
   (`ephemeral_root_rejection` / `alias_key_path` / `ReapEntryKind` /
   `RegistryReapEntry` / `RegistryReapPlan` / `RetainedRegistryEntry` not found),
   so nothing is silently wrong today — but a naive `crate::project_registry::`
   prefix rewrite must not touch the crate-local uses. See the
   `crate::project_registry` section below for the exact sites.
2. **`crate::sessions` (51 sites, 41 distinct items)** is the widest non-kernel
   seam. `sessions/` has its own mover (`tracedecay-sessions`), so this pair has
   to land together; `lib.rs:14` alone imports `SessionMessageRecord`,
   `SessionMessageSearchResult`, `SessionRecord`, and `lcm::LcmSummaryRequest`,
   and `lib.rs:55` re-exports `sessions::workflow_index::WorkflowScopeFilter` as
   part of this crate's public surface.
3. **`crate::application::session::…` (39 sites)** reaches into LCM contracts,
   render, and compatibility helpers that live in root `src/application/`, not in
   the already-extracted `tracedecay-application` crate. Those need to move down
   before this crate closes.
4. **`crate::daemon::…` (6 sites)** is an upward dependency
   (`DaemonWorkRuntimeV1`, `DaemonSessionRuntimeRegistryV1`,
   `profile_identity::load_or_create`). Four of the six are in test harnesses.
   Per the plan's "cycle edges break by moving shared pure-data types DOWN",
   these must not become a real `tracedecay-global-db -> tracedecay-daemon` edge.
5. **`crate::doctor::heal` at `observation/schema.rs:108`** is a prose comment
   reference only — no code depends on it.

## Cheap wins already applied

Fixed in place because they were mechanical, not architectural:

- `crate::global_db::…` -> `crate::…` and `pub(in crate::global_db)` ->
  `pub(crate)` (then `pub`, see below) throughout the moved tree.
- `crate::query::` -> `tracedecay_query::` (root `query` was already a bare
  `pub use tracedecay_query as query;`).
- `crate::types::CostTurn` -> `tracedecay_domain::observability::CostTurn`
  (root `types` is a re-export façade).
- `crate::migrate::durability` -> `tracedecay_migrate::durability`
  (`src/migrate/mod.rs:12` is a bare `pub use`).
- `crate::timeutil::parse_rfc3339_timestamp` ->
  `tracedecay_capture::parse_rfc3339_timestamp` (`src/timeutil.rs:12` re-export).
- `crate::sessions::{SessionMessageRecord, SessionRecord}` ->
  `tracedecay_store::{…}` (`src/sessions/mod.rs:3` re-export); the sibling
  `SessionMessageSearchResult` is genuinely root-owned and stays a seam.
- All `pub(crate)` promoted to `pub`. Inside the root crate these items were
  visible to the whole binary; the root shim (`pub(crate) use
  tracedecay_global_db::*;`) restores exactly that reachability without widening
  the published API.
- Repo-root-relative `include_str!` fixtures and the
  `#[path = "…/tests/session_suite/lcm_schema/mod.rs"]` attribute gained the one
  extra `../` the deeper crate location requires.
- Missing external dependencies added to the new manifest: `getrandom`,
  `schemars`, `tracing` (plus the profile inherited from the moved code:
  `futures-util`, `hex`, `hmac`, `rusqlite`, `serde`, `serde_json`, `sha2`,
  `thiserror`, `tokio`, `zeroize`, `tempfile`).
- `mod tests` (which owns `tests::harness::RegisteredGlobalDbHarness`, used by
  root test code) is now gated on `any(test, feature = "test-helpers")`, and the
  root declares the crate in `[dev-dependencies]` with that feature — otherwise
  the harness would vanish when the crate is built as a dependency.

## Not addressed (aftermath queue)

- Docs still naming `src/global_db/…` paths: `docs/LCM-PAYLOAD-LIFECYCLE.md`,
  `docs/THERMO-NUCLEAR-REVIEW.md`, `docs/REBRAND-COMPATIBILITY-FOLLOW-UP-CHECKLIST.md`,
  `docs/plans/tracedecay-v2/{13,15,23}-*.md`,
  `docs/superpowers/plans/2026-06-29-tracedecay-followups.md`, and the doc
  comments at `crates/tracedecay-migrate/src/durability.rs:126,199`.
- Root feature forwarding: the root `production` / `test-transport` / `lite` /
  `full` feature sets do not yet forward anything to `tracedecay-global-db`.
- `Cargo.lock` was regenerated by the checks in this worktree.

## Full catalogue

Format: `` `item` (site count) — file:line, … `` with paths relative to
`crates/tracedecay-global-db/src/`.

### `crate::db` — 252 reference sites, 32 distinct items

- `crate::db::engine::ReadSnapshot` (6) — checkpoint_tests.rs:3, project_registry.rs:213, registered_sessions.rs:587, session_temporal/expand.rs:371, session_temporal/expand.rs:517, session_temporal/mod.rs:374
- `crate::db::engine::Error` (5) — configuration/schema.rs:14, configuration/store.rs:70, session_temporal/cursor_keys.rs:366, session_temporal/cursor_keys.rs:51, session_temporal/refresh.rs:1113
- `crate::db::engine::TestConnection` (6) — configuration/schema.rs:400, observation/backfill.rs:395, observation_projection/schema.rs:1091, schema_contract/invariants.rs:673, schema_stages.rs:433, session_temporal/schema.rs:1863
- `crate::db::engine::TestConnection::open` (2) — configuration/schema.rs:403, registered.rs:934
- `crate::db::engine::Executor` (7) — configuration/schema.rs:5, git_index_transactions/schema.rs:1, lib.rs:1064, lib.rs:1082, lib.rs:1095, lib.rs:1128, lib.rs:1142
- `crate::db::engine` (59) — configuration/store.rs:53, configuration/store.rs:54, git_index_transactions/store.rs:15, git_index_transactions/store.rs:16, git_index_transactions/tests.rs:1, lib.rs:12, observation/backfill.rs:20, observation/mod.rs:24, observation/persist.rs:7, observation/retention.rs:81, observation/retention/tests.rs:1, observation/schema.rs:3, observation_projection/apply.rs:17, observation_projection/migration.rs:9, observation_projection/migration_tests.rs:29, observation_projection/rebuild.rs:6, observation_projection/schema.rs:3, observation_projection/state.rs:10, observation_projection/state.rs:1124, observation_projection/transition.rs:7, project_registry.rs:4, registered_dashboard.rs:4, schema_contract/invariants.rs:5, schema_contract/invariants/audit.rs:10, schema_contract/invariants/audit.rs:1408, schema_contract/invariants/repair.rs:8, schema_contract/invariants/repair/tests.rs:23, schema_contract/invariants/rows.rs:865, schema_contract/invariants/rows.rs:8, schema_contract/invariants/test_fixture.rs:18, schema_contract/invariants/triggers.rs:1, schema_contract/pragma.rs:3, schema_contract/validation.rs:1, schema_stages.rs:13, session_temporal/cursor_keys.rs:13, session_temporal/doctor_health.rs:15, session_temporal/expand.rs:5, session_temporal/hydration.rs:1191, session_temporal/hydration.rs:18, session_temporal/operations/compatibility.rs:1, session_temporal/operations/generation.rs:3, session_temporal/operations/mod.rs:8, session_temporal/operations/publication.rs:3, session_temporal/operations/sources.rs:3, session_temporal/projection/derived.rs:1, session_temporal/projection/materialize.rs:3, session_temporal/projection/persist.rs:1, session_temporal/projection/receipts.rs:1, session_temporal/projection/tests.rs:27, session_temporal/query.rs:5, session_temporal/rebuild.rs:3, session_temporal/refresh.rs:1, session_temporal/registered_lcm_render.rs:15, session_temporal/retrieval.rs:1, session_temporal/retrieval.rs:3, session_temporal/retrieval/tests.rs:13, session_temporal/schema.rs:1, session_temporal/sql.rs:1, transcript.rs:6
- `crate::db::engine::Result` (45) — git_index_transactions/store.rs:42, git_index_transactions/store.rs:55, git_index_transactions/store.rs:66, git_index_transactions/store.rs:76, git_index_transactions/store.rs:84, lib.rs:1053, lib.rs:1068, lib.rs:1085, lib.rs:1096, lib.rs:1129, lib.rs:1143, observation/retention.rs:523, project_registry.rs:179, project_registry.rs:242, project_registry.rs:251, project_registry.rs:260, project_registry.rs:267, registered.rs:162, registered.rs:535, registered.rs:543, registered.rs:551, registered.rs:556, registered.rs:564, registered.rs:576, registered.rs:585, registered.rs:592, registered.rs:602, registered.rs:609, registered.rs:617, registered.rs:625, registered.rs:630, registered.rs:648, registered.rs:652, registered_lcm.rs:36, registered_lcm.rs:45, registered_lcm.rs:52, session_temporal/doctor_health.rs:1211, session_temporal/doctor_health.rs:1271, session_temporal/doctor_health.rs:1312, session_temporal/doctor_health.rs:1331, session_temporal/doctor_health.rs:1387, session_temporal/doctor_health.rs:1440, session_temporal/doctor_health.rs:1457, session_temporal/doctor_health.rs:831, session_temporal/retrieval/tests.rs:2054
- `crate::db::engine::QueryExecutor` (6) — lib.rs:1050, lib.rs:670, lib.rs:698, lib.rs:731, session_temporal_repair_tests.rs:9, tests.rs:773
- `crate::db::engine::QueryExecutor::query` (10) — lib.rs:1054, lib.rs:675, lib.rs:703, lib.rs:734, session_temporal/retrieval/tests.rs:198, session_temporal/retrieval/tests.rs:230, session_temporal/retrieval/tests.rs:242, session_temporal/retrieval/tests.rs:254, session_temporal/retrieval/tests.rs:270, session_temporal/retrieval/tests.rs:282
- `crate::db::engine::params` (66) — lib.rs:1057, lib.rs:444, lib.rs:590, lib.rs:610, lib.rs:622, lib.rs:680, lib.rs:708, lib.rs:737, observation_projection/migration_tests.rs:1525, observation_projection/migration_tests.rs:1532, observation_projection/migration_tests.rs:1552, observation_projection/migration_tests.rs:1574, observation_projection/migration_tests.rs:1581, observation_projection/migration_tests.rs:1635, project_registry.rs:310, project_registry.rs:442, project_registry.rs:477, project_registry.rs:534, registered.rs:955, registered_accounting.rs:132, registered_accounting.rs:216, registered_accounting.rs:231, registered_accounting.rs:27, registered_accounting.rs:282, registered_accounting.rs:298, registered_accounting.rs:423, registered_accounting.rs:445, registered_accounting.rs:470, registered_accounting.rs:497, registered_accounting.rs:49, registered_accounting.rs:529, registered_accounting.rs:573, registered_accounting.rs:600, registered_analytics.rs:363, registered_analytics.rs:52, registered_dashboard.rs:102, registered_dashboard.rs:164, registered_dashboard.rs:205, registered_dashboard.rs:339, registered_dashboard.rs:359, registered_dashboard.rs:380, registered_dashboard.rs:400, registered_dashboard.rs:419, registered_dashboard.rs:520, registered_dashboard.rs:559, registered_dashboard.rs:586, registered_dashboard.rs:614, registered_sessions.rs:134, registered_sessions.rs:180, registered_sessions.rs:215, registered_sessions.rs:289, registered_sessions.rs:54, schema_contract/invariants/triggers.rs:1802, session_temporal/direct.rs:1, session_temporal/hydration.rs:6, session_temporal/mod.rs:23, session_temporal/projection.rs:1, session_temporal/tests/harness.rs:23, session_temporal_repair_tests.rs:19, tests.rs:14, tests.rs:188, tests.rs:56, tests.rs:745, tests.rs:779, tests.rs:794, tests.rs:806
- `crate::db::engine::Executor::execute` (2) — lib.rs:1069, lib.rs:1117
- `crate::db::engine::Row` (4) — lib.rs:862, registered_sessions.rs:669, registered_sessions.rs:687, registered_sessions.rs:706
- `crate::db::engine::Rows` (4) — observation/retention.rs:523, registered_dashboard.rs:780, session_temporal/registered_lcm_render.rs:880, session_temporal/registered_lcm_render.rs:890
- `crate::db::enter_daemon_database_scope` (5) — observation/retention/tests.rs:243, registered.rs:932, registered.rs:968, session_temporal/tests/harness.rs:46, tests/harness.rs:26
- `crate::db::DatabaseAuthority::for_runtime` (1) — observation/retention/tests.rs:250
- `crate::db::retrieval_anchor_schema::install_retrieval_anchor_schema` (1) — observation/schema.rs:241
- `crate::db::engine::TransactionBehavior::Immediate` (1) — observation_projection/schema.rs:1263
- `crate::db::DatabaseAuthorityRole::Maintenance` (1) — registered.rs:512
- `crate::db::engine::Error::invalid_operation` (4) — registered.rs:559, registered.rs:637, registered.rs:640, registered.rs:655
- `crate::db::engine::DatabaseAttachmentExecutor` (1) — registered.rs:597
- `crate::db::sqlite_generation_identity` (1) — registered.rs:696
- `crate::db::SqliteFileIdentityError` (1) — registered.rs:766
- `crate::db::SqliteFileIdentityError::Open` (1) — registered.rs:768
- `crate::db::SqliteFileIdentityError::Inspect` (1) — registered.rs:769
- `crate::db::SqliteFileIdentityError::Identify` (1) — registered.rs:770
- `crate::db::SqliteFileIdentityError::Unavailable` (1) — registered.rs:771
- `crate::db::engine::TestConnection::open_with_write_authority` (1) — registered.rs:973
- `crate::db::engine::Params` (1) — registered_accounting.rs:599
- `crate::db::engine::Value` (4) — registered_analytics.rs:3, registered_sessions.rs:8, session_temporal/retrieval/candidates.rs:5, session_temporal/retrieval/records.rs:1
- `crate::db::install_external_source_schema` (1) — schema_stages.rs:308
- `crate::db::engine::IntoParams` (1) — session_temporal/registered_lcm_render.rs:882
- `crate::db::DaemonDatabaseScope` (2) — session_temporal/tests/harness.rs:22, tests/harness.rs:7

### `crate::errors` — 194 reference sites, 5 distinct items

- `crate::errors::Result` (182) — git_index_transactions/schema.rs:12, lib.rs:1215, lib.rs:1234, lib.rs:338, lib.rs:400, lib.rs:470, lib.rs:487, lib.rs:502, lib.rs:513, lib.rs:654, lib.rs:671, lib.rs:699, lib.rs:733, observation/backfill.rs:104, observation/backfill.rs:120, observation/backfill.rs:176, observation/backfill.rs:191, observation/backfill.rs:253, observation/backfill.rs:307, observation/backfill.rs:334, observation/backfill.rs:347, observation/backfill.rs:372, observation/backfill.rs:65, observation/backfill.rs:81, observation/backfill.rs:92, observation/schema.rs:157, observation/schema.rs:16, observation/schema.rs:239, observation/schema.rs:30, observation/schema.rs:52, observation/schema.rs:69, observation_projection/migration_tests.rs:38, observation_projection/schema.rs:1096, project_registry.rs:1062, project_registry.rs:1067, project_registry.rs:1171, project_registry.rs:1204, project_registry.rs:1218, project_registry.rs:1232, project_registry.rs:1263, project_registry.rs:1269, project_registry.rs:1445, project_registry.rs:1450, project_registry.rs:1460, project_registry.rs:1488, project_registry.rs:1626, project_registry.rs:221, project_registry.rs:232, project_registry.rs:273, project_registry.rs:280, project_registry.rs:291, project_registry.rs:298, project_registry.rs:435, project_registry.rs:471, project_registry.rs:564, project_registry.rs:572, project_registry.rs:979, registered.rs:110, registered.rs:131, registered.rs:135, registered.rs:146, registered.rs:166, registered.rs:209, registered.rs:219, registered.rs:229, registered.rs:241, registered.rs:254, registered.rs:288, registered.rs:306, registered.rs:333, registered.rs:346, registered.rs:356, registered.rs:370, registered.rs:388, registered.rs:409, registered.rs:423, registered.rs:442, registered.rs:474, registered.rs:664, registered.rs:688, registered.rs:711, registered.rs:732, registered.rs:745, registered.rs:94, registered_accounting.rs:124, registered_accounting.rs:19, registered_sessions.rs:536, schema_contract/invariants.rs:101, schema_contract/invariants.rs:122, schema_contract/invariants.rs:135, schema_contract/invariants.rs:143, schema_contract/invariants.rs:363, schema_contract/invariants.rs:380, schema_contract/invariants.rs:462, schema_contract/invariants.rs:509, schema_contract/invariants.rs:527, schema_contract/invariants.rs:550, schema_contract/invariants.rs:563, schema_contract/invariants.rs:609, schema_contract/invariants.rs:70, schema_contract/invariants.rs:74, schema_contract/invariants.rs:86, schema_contract/invariants/audit.rs:1003, schema_contract/invariants/audit.rs:1042, schema_contract/invariants/audit.rs:1311, schema_contract/invariants/audit.rs:1320, schema_contract/invariants/audit.rs:1332, schema_contract/invariants/audit.rs:152, schema_contract/invariants/audit.rs:206, schema_contract/invariants/audit.rs:260, schema_contract/invariants/audit.rs:343, schema_contract/invariants/audit.rs:423, schema_contract/invariants/audit.rs:463, schema_contract/invariants/audit.rs:515, schema_contract/invariants/audit.rs:52, schema_contract/invariants/audit.rs:549, schema_contract/invariants/audit.rs:569, schema_contract/invariants/audit.rs:583, schema_contract/invariants/audit.rs:607, schema_contract/invariants/audit.rs:628, schema_contract/invariants/audit.rs:661, schema_contract/invariants/audit.rs:690, schema_contract/invariants/audit.rs:708, schema_contract/invariants/audit.rs:727, schema_contract/invariants/audit.rs:807, schema_contract/invariants/audit.rs:856, schema_contract/invariants/audit.rs:864, schema_contract/invariants/audit.rs:888, schema_contract/invariants/audit.rs:921, schema_contract/invariants/audit.rs:972, schema_contract/invariants/repair.rs:168, schema_contract/invariants/repair.rs:219, schema_contract/invariants/repair.rs:27, schema_contract/invariants/repair.rs:288, schema_contract/invariants/repair.rs:312, schema_contract/invariants/repair.rs:334, schema_contract/invariants/repair.rs:362, schema_contract/invariants/repair.rs:43, schema_contract/invariants/rows.rs:100, schema_contract/invariants/rows.rs:108, schema_contract/invariants/rows.rs:116, schema_contract/invariants/rows.rs:205, schema_contract/invariants/rows.rs:292, schema_contract/invariants/rows.rs:310, schema_contract/invariants/rows.rs:441, schema_contract/invariants/rows.rs:74, schema_contract/invariants/triggers.rs:1698, schema_contract/invariants/triggers.rs:1709, schema_contract/invariants/triggers.rs:1723, schema_contract/invariants/triggers.rs:1751, schema_contract/invariants/triggers.rs:1770, schema_contract/invariants/triggers.rs:1785, schema_contract/pragma.rs:143, schema_contract/pragma.rs:49, schema_contract/pragma.rs:95, schema_contract/validation.rs:179, schema_contract/validation.rs:254, schema_contract/validation.rs:301, schema_contract/validation.rs:341, schema_contract/validation.rs:384, schema_contract/validation.rs:395, schema_contract/validation.rs:414, schema_contract/validation.rs:424, schema_contract/validation.rs:63, schema_stages.rs:224, schema_stages.rs:240, schema_stages.rs:365, schema_stages.rs:395, schema_stages.rs:411, schema_stages.rs:418, schema_stages.rs:424, session_temporal/schema.rs:1044, session_temporal/schema.rs:1084, session_temporal/schema.rs:1100, session_temporal/schema.rs:1399, session_temporal/schema.rs:1589, session_temporal/schema.rs:1705, session_temporal/schema.rs:1726, session_temporal/schema.rs:1766, session_temporal/schema.rs:1778, session_temporal/schema.rs:1790, session_temporal/schema.rs:1824
- `crate::errors::TraceDecayError` (4) — lib.rs:13, registered_accounting.rs:151, registered_dashboard.rs:5, schema_contract/invariants/rows.rs:93
- `crate::errors` (1) — observation/retention.rs:84
- `crate::errors::TraceDecayError::Database` (6) — registered_sessions.rs:538, registered_sessions.rs:551, registered_sessions.rs:559, registered_sessions.rs:566, registered_sessions.rs:571, registered_sessions.rs:576
- `crate::errors::Result::Ok` (1) — schema_stages.rs:328

### `crate::sessions` — 51 reference sites, 41 distinct items

- `crate::sessions::git_correlation::AnalyticsSessionTimestampSource` (1) — lib.rs:106
- `crate::sessions::git_correlation::AnalyticsSessionTimestamp` (2) — lib.rs:109, lib.rs:111
- `crate::sessions` (2) — lib.rs:14, transcript.rs:7
- `crate::sessions::workflow_index` (1) — lib.rs:53
- `crate::sessions::workflow_index::WorkflowScopeFilter` (1) — lib.rs:54
- `crate::sessions::lcm::retention` (1) — observation/retention.rs:17
- `crate::sessions::claude` (1) — observation_projection/apply.rs:18
- `crate::sessions::cursor_composer` (1) — observation_projection/migration_tests.rs:31
- `crate::sessions::git_correlation::normalize_worktree` (1) — observation_projection/state.rs:860
- `crate::sessions::codex_app_server::CodexAppServerSummaryConfig` (1) — registered.rs:367
- `crate::sessions::lcm::retention::LcmRetentionConfig` (1) — registered.rs:439
- `crate::sessions::lcm::retention::RetentionMode` (1) — registered.rs:440
- `crate::sessions::lcm::retention::LcmRetentionReport` (1) — registered.rs:442
- `crate::sessions::lcm::retention::run_session_retention_authorized` (1) — registered.rs:450
- `crate::sessions::lcm::LcmError::Db` (1) — registered.rs:461
- `crate::sessions::lcm::dag::summary_node_id` (1) — registered_lcm.rs:411
- `crate::sessions::lcm::types::LcmImmutableSummaryPublication` (1) — registered_lcm.rs:429
- `crate::sessions::lcm::schema::ensure_lcm_schema_in_transaction` (1) — schema_stages.rs:315
- `crate::sessions::git_correlation::ensure_git_correlation_schema_in_transaction` (1) — schema_stages.rs:318
- `crate::sessions::workflow_index::ensure_workflow_index_schema` (1) — schema_stages.rs:323
- `crate::sessions::lcm::payload::read_verified_payload_content` (1) — session_temporal/hydration.rs:27
- `crate::sessions::lcm::types::LcmSourceRef::SummaryNode` (1) — session_temporal/operations/generation.rs:25
- `crate::sessions::lcm::types` (3) — session_temporal/operations/generation.rs:6, session_temporal/operations/mod.rs:13, session_temporal/operations/sources.rs:13
- `crate::sessions::lcm` (2) — session_temporal/operations/publication.rs:8, session_temporal/tests/privacy.rs:32
- `crate::sessions::lcm::payload` (1) — session_temporal/tests/harness.rs:25
- `crate::sessions::lcm::payload::PayloadFileRollback::begin_cancellation_safe` (2) — tests.rs:160, transcript.rs:769
- `crate::sessions::lcm::payload::write_external_payload_tracked` (1) — tests.rs:163
- `crate::sessions::lcm::payload::ExternalPayloadWrite` (1) — tests.rs:165
- `crate::sessions::lcm::payload::payload_dir` (1) — tests.rs:181
- `crate::sessions::lcm::LcmLifecycleUpdate` (1) — tests.rs:251
- `crate::sessions::lcm::LcmMaintenanceDebt::RawBacklog` (1) — tests.rs:258
- `crate::sessions::lcm::compression::update_lifecycle` (2) — tests.rs:268, tests.rs:305
- `crate::sessions::lcm::compression::lifecycle_state` (2) — tests.rs:278, tests.rs:293
- `crate::sessions::lcm::payload::PayloadFileRollback` (1) — transcript.rs:399
- `crate::sessions::lcm::raw::upsert_raw_message_with_payload_tracked` (1) — transcript.rs:407
- `crate::sessions::lcm::dag::insert_summary_node` (1) — transcript.rs:491
- `crate::sessions::git_correlation::CommitSessionRecord` (2) — transcript.rs:716, transcript.rs:757
- `crate::sessions::git_correlation::SpanObservation` (2) — transcript.rs:717, transcript.rs:758
- `crate::sessions::git_correlation::upsert_commit_session` (1) — transcript.rs:817
- `crate::sessions::git_correlation::record_span_observation_in_transaction` (1) — transcript.rs:824
- `crate::sessions::git_correlation::DEFAULT_SPAN_MERGE_GAP_SECS` (1) — transcript.rs:827

### `crate::application` — 39 reference sites, 20 distinct items

- `crate::application::host_admission` (7) — configuration/store.rs:4194, observation_projection/migration_tests.rs:28, registered_sessions.rs:792, session_temporal/hydration.rs:1210, session_temporal/projection/tests.rs:24, session_temporal/registered_lcm_render.rs:901, session_temporal/retrieval/tests.rs:12
- `crate::application::configuration` (1) — configuration/store.rs:42
- `crate::application::configuration::CredentialWriteHandleV1::new` (2) — configuration/store.rs:5199, configuration/store.rs:5247
- `crate::application::session::compatibility::is_inventory_text` (1) — lib.rs:930
- `crate::application::session::compatibility` (3) — observation_projection/apply.rs:14, observation_projection/rebuild.rs:3, registered_sessions.rs:5
- `crate::application::session::compatibility::projected_content_hash` (4) — observation_projection/state.rs:9, session_temporal/operations/mod.rs:12, session_temporal/operations/publication.rs:7, session_temporal/operations/sources.rs:12
- `crate::application::evidence_assembly::RuntimeEvidenceAssemblyStore` (1) — registered.rs:277
- `crate::application::evidence_assembly::RuntimeEvidenceAssemblyStore::new` (1) — registered.rs:279
- `crate::application::external_source_store::RuntimeExternalSourceStore` (1) — registered.rs:400
- `crate::application::external_source_store::RuntimeExternalSourceErrorV1` (1) — registered.rs:401
- `crate::application::external_source_store::RuntimeExternalSourceStore::new` (1) — registered.rs:403
- `crate::application::host_admission::HostAdmissionTestRuntimeV1::profile` (1) — schema_contract/validation.rs:453
- `crate::application::session::SessionTemporalExecutionError` (1) — session_temporal/direct.rs:4
- `crate::application::session::lcm::contracts` (4) — session_temporal/direct.rs:5, session_temporal/mod.rs:28, session_temporal/operations/compatibility.rs:3, session_temporal/registered_lcm_render.rs:7
- `crate::application::session::lcm::contracts::validate_payload_ref` (1) — session_temporal/hydration.rs:17
- `crate::application::session::lcm::render` (2) — session_temporal/mod.rs:13, session_temporal/mod.rs:32
- `crate::application::session` (3) — session_temporal/mod.rs:35, session_temporal/tests/application.rs:20, session_temporal/tests/privacy.rs:25
- `crate::application::session::lcm::render::apply_canonical_content` (1) — session_temporal/registered_lcm_render.rs:14
- `crate::application::context` (2) — session_temporal/tests/application.rs:15, session_temporal/tests/privacy.rs:20
- `crate::application::session::compatibility::derived_text_for_index` (1) — transcript.rs:796

### `crate::config` — 23 reference sites, 10 distinct items

- `crate::config::registry` (1) — configuration/migration.rs:19
- `crate::config::resolver` (2) — configuration/migration.rs:20, configuration/store.rs:51
- `crate::config::SemanticConfig` (4) — configuration/store.rs:2619, configuration/store.rs:2655, configuration/store.rs:4276, configuration/store.rs:4288
- `crate::config::SEMANTIC_RUNTIME_SETTING_KEY` (3) — configuration/store.rs:2740, configuration/store.rs:4250, configuration/store.rs:4307
- `crate::config::registry::ConfigurationRegistry` (3) — configuration/store.rs:4195, configuration/store.rs:50, configuration/store/read.rs:3
- `crate::config::resolver::resolve_configuration` (1) — configuration/store.rs:4196
- `crate::config::DEFAULT_FASTEMBED_MODEL_ID` (3) — configuration/store.rs:4253, configuration/store.rs:4292, configuration/store.rs:4314
- `crate::config::SemanticResourceCeilings::default` (3) — configuration/store.rs:4261, configuration/store.rs:4297, configuration/store.rs:4321
- `crate::config::user_data_dir` (1) — lib.rs:779
- `crate::config::brand_env` (2) — lib.rs:844, lib.rs:851

### `crate::storage` — 13 reference sites, 10 distinct items

- `crate::storage::read_repository_identity_marker` (2) — observation_store.rs:207, project_registry.rs:1234
- `crate::storage::STORE_MANIFEST_FILENAME` (2) — observation_store.rs:287, registered_dashboard.rs:251
- `crate::storage::ValidatedProfileShard::resolve_existing` (1) — observation_store.rs:301
- `crate::storage::ProfileShardValidationError::Unavailable` (1) — observation_store.rs:306
- `crate::storage::ProfileShardValidationError::NonCanonical` (1) — observation_store.rs:313
- `crate::storage::profile_sharded_data_root` (1) — project_registry.rs:1599
- `crate::storage::PrivateStoreIo::create_dir_all` (1) — registered.rs:194
- `crate::storage::StoreManifest` (1) — registered_dashboard.rs:266
- `crate::storage::write_store_manifest_to_path` (2) — registered_dashboard.rs:454, registered_dashboard.rs:468
- `crate::storage::has_sqlite_database_header` (1) — session_temporal/doctor_health.rs:751

### `crate::retention` — 8 reference sites, 5 distinct items

- `crate::retention::RetentionConfig` (2) — lib.rs:1213, lib.rs:1232
- `crate::retention::RetentionTableReport` (2) — lib.rs:1215, lib.rs:1234
- `crate::retention::prune_global_tables` (2) — lib.rs:1217, lib.rs:1236
- `crate::retention::RetentionMode::Apply` (1) — lib.rs:1220
- `crate::retention::RetentionMode::DryRun` (1) — lib.rs:1239

### `crate::daemon` — 6 reference sites, 4 distinct items

- `crate::daemon::work_runtime::DaemonWorkRuntimeV1` (1) — registered.rs:371
- `crate::daemon::work_runtime::DaemonWorkRuntimeV1::new` (1) — registered.rs:376
- `crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1` (2) — session_temporal/tests/harness.rs:21, tests/harness.rs:6
- `crate::daemon::profile_identity::load_or_create` (2) — session_temporal/tests/harness.rs:44, tests/harness.rs:23

### `crate::tracedecay` — 4 reference sites, 1 distinct items

- `crate::tracedecay::current_timestamp` (4) — project_registry.rs:1350, project_registry.rs:665, project_registry.rs:737, project_registry.rs:794

### `crate::project_registry` — 4 reference sites, 3 distinct items

- `crate::project_registry::alias_key_path` (1) — project_registry.rs:1547
- `crate::project_registry` (1) — project_registry.rs:5
- `crate::project_registry::ephemeral_root_rejection` (2) — project_registry.rs:642, project_registry.rs:646

### `crate::context` — 2 reference sites, 1 distinct items

- `crate::context::read_modes::estimate_tokens` (2) — registered_lcm.rs:390, transcript.rs:30

### `crate::worktree` — 1 reference sites, 1 distinct items

- `crate::worktree::git_common_dir` (1) — observation_store.rs:231

### `crate::store` — 1 reference sites, 1 distinct items

- `crate::store::session::GlobalDbSessionTemporalStore` (1) — session_temporal/projection/tests.rs:28

### `crate::os_str_bytes` — 1 reference sites, 1 distinct items

- `crate::os_str_bytes::native_os_str_bytes` (1) — project_registry.rs:80

### `crate::lifecycle_lease` — 1 reference sites, 1 distinct items

- `crate::lifecycle_lease::canonical_or_original` (1) — project_registry.rs:46

### `crate::doctor` — 1 reference sites, 1 distinct items

- `crate::doctor::heal` (1) — observation/schema.rs:109
