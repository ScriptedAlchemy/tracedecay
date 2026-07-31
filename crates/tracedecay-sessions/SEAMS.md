# tracedecay-sessions root seams

The one-shot crate split moved the whole former `src/sessions/` tree into
`crates/tracedecay-sessions/src/runtime/`. Per the split doctrine the move did
**not** try to resolve references back into the root binary crate; they are
catalogued here as the aftermath work-list.

Each entry is a `crate::<root module>::…` path that no longer resolves because
`<root module>` still lives in the root crate. Fixing one means either moving
that module into a workspace crate too, or inverting the dependency behind a
port that the root crate implements.

**Total unresolved references: 296** across 25 root modules and 79 files.

## Crate check status

`cargo check -p tracedecay-sessions` reports **220 errors**, all of them either a
seam below or a cascade from one:

| Code | Count | Meaning |
| --- | ---: | --- |
| `E0433` | 167 | `crate::<root module>` path does not resolve |
| `E0432` | 52 | unresolved `use crate::<root module>::…` import |
| `E0223` | 1 | ambiguous associated type, cascading from the unresolved `crate::privacy::SensitiveKeyPolicy` trait |

The error count is smaller than the reference count because a single failed
`use` masks every downstream use of the imported name. Resolving `crate::db`,
`crate::application`, and `crate::privacy` should clear the large majority.

Everything the moved code needed from third-party and sibling workspace crates
is already wired in `Cargo.toml`; nothing outside this table is outstanding.

## Notes on specific seams

- `crate::application` is the **root** `src/application/` tree
  (`host_admission`, `observation`, `session::compatibility`), not the existing
  `tracedecay-application` crate. Only `session::lcm::compression_policy` had a
  crate-side equivalent and was repointed at `crate::lcm::compression_policy`.
- `crate::db` is the root SQLite engine façade (`db::engine::{Connection, Row,
  Executor, …}`). It is the single highest-leverage seam: it blocks every LCM and
  workflow storage module.
- `crate::store` is the root store layer (`GlobalDbTranscriptStore`,
  `TranscriptIngestStore`, …), distinct from the `tracedecay-store` crate that is
  already a dependency.
- `crate::privacy` supplies the sanitization kernel shared by every provider
  parser; it is why the provider modules fail as a block.

## Non-code references to the old path

These name `src/sessions/…` as data or prose and were deliberately left alone;
the benchmark manifests additionally pin SHA-256 digests over the harness files,
so they need a re-seal rather than a path edit.

- `benchmarks/pr5-observation/workload-v1.json` and
  `benchmarks/pr5-observation/result-2026-07-26-dc17dd73.json`
- `benchmarks/pr8-temporal/workload-v1.json` and
  `benchmarks/pr8-temporal/result-provisional.json`
- `tests/fixtures/transcript_golden/cline_like/manifest.json` and
  `tests/fixtures/transcript_golden/cline_like/expected/parser_provenance.json`
- `tests/fixtures/provider_normalization/codex/README.md`
- Twelve `docs/**` files (LCM, memory, and plan documents) that cite the old
  module paths in prose.

The `include_str!`/`include_bytes!` fixture paths inside the moved code were
repointed at the repo root and do resolve.

## Offenders by volume

| Root module | References | Distinct paths | Files |
| --- | ---: | ---: | ---: |
| `crate::application` | 91 | 26 | 41 |
| `crate::db` | 74 | 17 | 34 |
| `crate::privacy` | 34 | 11 | 21 |
| `crate::errors` | 12 | 4 | 3 |
| `crate::config` | 9 | 5 | 6 |
| `crate::global_db` | 9 | 2 | 8 |
| `crate::storage` | 9 | 8 | 7 |
| `crate::store` | 8 | 6 | 8 |
| `crate::worktree` | 8 | 2 | 3 |
| `crate::agents` | 7 | 3 | 4 |
| `crate::accounting` | 6 | 1 | 6 |
| `crate::daemon` | 6 | 2 | 3 |
| `crate::git` | 3 | 2 | 3 |
| `crate::tracedecay` | 3 | 1 | 3 |
| `crate::user_config` | 3 | 2 | 1 |
| `crate::hooks` | 2 | 2 | 2 |
| `crate::os_str_bytes` | 2 | 1 | 1 |
| `crate::serde_util` | 2 | 1 | 1 |
| `crate::timeutil` | 2 | 1 | 2 |
| `crate::context` | 1 | 1 | 1 |
| `crate::dashboard` | 1 | 1 | 1 |
| `crate::repository_provenance` | 1 | 1 | 1 |
| `crate::sqlite_read_snapshot` | 1 | 1 | 1 |
| `crate::text` | 1 | 1 | 1 |
| `crate::windows_file` | 1 | 1 | 1 |

## Full catalog

### `crate::application` (91 references)

- `crates/tracedecay-sessions/src/runtime/claude.rs:256` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/claude.rs:264` — `crate::application::observation::ObservationCancellation::default`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:23` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:26` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:912` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:916` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/runner.rs:12` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/runner.rs:13` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:22` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:24` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:27` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:1195` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::profile`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:1263` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::profile`
- `crates/tracedecay-sessions/src/runtime/codex/observation.rs:21` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/codex/observation.rs:22` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/codex_app_server.rs:21` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:26` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:27` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:1798` — `crate::application::host_admission::HostAdmissionTestRuntimeV1`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/capture.rs:19` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/capture.rs:20` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/ingest.rs:17` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/ingest.rs:18` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/tests.rs:15` — `crate::application::host_admission::HostAdmissionTestRuntimeV1`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/tests.rs:16` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/hermes/coverage.rs:16` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/hermes/coverage.rs:17` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:9` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:10` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/hermes/observation.rs:16` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/hermes/state_db.rs:10` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/hermes/state_db.rs:11` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/hermes/tests.rs:12` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/hermes/tests.rs:13` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/ingest/failure.rs:316` — `crate::application::host_admission::is_bounded_reason_code`
- `crates/tracedecay-sessions/src/runtime/ingest/failure.rs:414` — `crate::application::observation::ObservationApplicationError::Store`
- `crates/tracedecay-sessions/src/runtime/ingest/failure.rs:417` — `crate::application::observation::ObservationApplicationError::Cancelled`
- `crates/tracedecay-sessions/src/runtime/ingest/failure.rs:420` — `crate::application::observation::ObservationApplicationError::PersistedObservationUnavailable`
- `crates/tracedecay-sessions/src/runtime/ingest/failure.rs:423` — `crate::application::observation::ObservationApplicationError::Contract`
- `crates/tracedecay-sessions/src/runtime/ingest/failure.rs:426` — `crate::application::observation::ObservationApplicationError::Privacy`
- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:4` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:5` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/ingest/project_provider.rs:7` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/ingest/project_provider.rs:8` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/ingest/scheduler.rs:5` — `crate::application::host_admission::DEFAULT_MAX_RECORDS`
- `crates/tracedecay-sessions/src/runtime/ingest/startup.rs:3` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/ingest/tests.rs:8` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/ingest/tests.rs:9` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/ingest/user.rs:3` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/ingest/user.rs:4` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/ingest/user_provider.rs:5` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/ingest/user_provider.rs:6` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/jsonl_observation_admission.rs:11` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/jsonl_observation_admission.rs:12` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:30` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:32` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:33` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:1073` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::profile`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:1146` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::profile`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:1224` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::profile`
- `crates/tracedecay-sessions/src/runtime/lcm/compression.rs:5` — `crate::application::session::compatibility::projected_content_hash`
- `crates/tracedecay-sessions/src/runtime/lcm/dag.rs:6` — `crate::application::session::compatibility::projected_content_hash`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:16` — `crate::application::configuration::ProductionUserSettingsDaemonClient`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:17` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/lcm/payload.rs:3` — `crate::application::session::lcm::contracts::validate_payload_ref`
- `crates/tracedecay-sessions/src/runtime/lcm/query/grep.rs:1` — `crate::application::session::compatibility`
- `crates/tracedecay-sessions/src/runtime/lcm/query.rs:73` — `crate::application::session::compatibility::rerank_fetch_limit`
- `crates/tracedecay-sessions/src/runtime/lcm/raw.rs:5` — `crate::application::session::compatibility::derived_text_for_index`
- `crates/tracedecay-sessions/src/runtime/lcm/raw.rs:6` — `crate::application::session::compatibility::derived_text_for_snippet`
- `crates/tracedecay-sessions/src/runtime/lcm/raw.rs:7` — `crate::application::session::compatibility::projected_content_hash`
- `crates/tracedecay-sessions/src/runtime/lcm/schema.rs:1` — `crate::application::session::compatibility::projected_content_hash`
- `crates/tracedecay-sessions/src/runtime/lcm/types.rs:9` — `crate::application::session::compatibility`
- `crates/tracedecay-sessions/src/runtime/lcm/types.rs:12` — `crate::application::session::lcm::contracts`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:35` — `crate::application::context`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:40` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:41` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:42` — `crate::application::session`
- `crates/tracedecay-sessions/src/runtime/snapshot_observation.rs:17` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/snapshot_observation.rs:21` — `crate::application::observation`
- `crates/tracedecay-sessions/src/runtime/source/discovery.rs:9` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/source.rs:44` — `crate::application::host_admission`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1248` — `crate::application::host_admission::HostAdmissionTestRuntimeV1`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1258` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::project`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1270` — `crate::application::host_admission::HostAdmissionScope::Project`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1292` — `crate::application::host_admission::HostAdmissionTestRuntimeV1`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1302` — `crate::application::host_admission::HostAdmissionTestRuntimeV1`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1312` — `crate::application::host_admission::HostAdmissionTestRuntimeV1::project`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1324` — `crate::application::host_admission::HostAdmissionScope::Project`
- `crates/tracedecay-sessions/src/runtime/vibe.rs:25` — `crate::application::host_admission::HostAdmissionFacade`
- `crates/tracedecay-sessions/src/runtime/vibe.rs:26` — `crate::application::observation::ObservationCancellation`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest.rs:15` — `crate::application::host_admission::DEFAULT_MAX_RECORDS`

### `crate::db` (74 references)

- `crates/tracedecay-sessions/src/runtime/cursor_composer/ingest.rs:82` — `crate::db::sqlite_generation_identity`
- `crates/tracedecay-sessions/src/runtime/git_correlation/attribution.rs:1` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/git_correlation/backfill.rs:1` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/git_correlation/store.rs:9` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:3` — `crate::db::engine::TestConnection`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:36` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:36` — `crate::db::engine::Rows`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:38` — `crate::db::engine::IntoParams`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:45` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:47` — `crate::db::engine::IntoParams`
- `crates/tracedecay-sessions/src/runtime/git_correlation/tests.rs:52` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:15` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:16` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:74` — `crate::db::engine::Error`
- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:75` — `crate::db::engine::Error`
- `crates/tracedecay-sessions/src/runtime/hermes/coverage.rs:18` — `crate::db`
- `crates/tracedecay-sessions/src/runtime/ingest/tests.rs:613` — `crate::db::engine::TestConnection::open`
- `crates/tracedecay-sessions/src/runtime/ingest/tests.rs:614` — `crate::db::engine::Connection`
- `crates/tracedecay-sessions/src/runtime/lcm/compression.rs:6` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/dag.rs:7` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:19` — `crate::db::DaemonDatabaseScope`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:20` — `crate::db::engine::params`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:64` — `crate::db::enter_daemon_database_scope`
- `crates/tracedecay-sessions/src/runtime/lcm/doctor.rs:8` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/doctor.rs:9` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/doctor.rs:1255` — `crate::db::engine::TestConnection`
- `crates/tracedecay-sessions/src/runtime/lcm/gc/orphan_scan.rs:5` — `crate::db::engine::Executor`
- `crates/tracedecay-sessions/src/runtime/lcm/gc/pending_delete.rs:5` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/gc/tests.rs:5` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/gc/tests.rs:21` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/lcm/gc/tests.rs:30` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/lcm/gc/tests.rs:37` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/lcm/gc.rs:9` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/gc.rs:10` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/maintenance.rs:10` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/maintenance.rs:170` — `crate::db::engine::TestConnection`
- `crates/tracedecay-sessions/src/runtime/lcm/payload/delete_recovery.rs:10` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/payload/delete_recovery.rs:11` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/payload/rollback_tests.rs:3` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/payload.rs:4` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/query/grep.rs:608` — `crate::db::engine::Row`
- `crates/tracedecay-sessions/src/runtime/lcm/query/grep.rs:644` — `crate::db::engine::Row`
- `crates/tracedecay-sessions/src/runtime/lcm/query/status.rs:757` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/retention/tests.rs:3` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/retention/tests.rs:220` — `crate::db::enter_daemon_database_scope`
- `crates/tracedecay-sessions/src/runtime/lcm/retention/tests.rs:227` — `crate::db::DatabaseAuthority::for_runtime`
- `crates/tracedecay-sessions/src/runtime/lcm/retention.rs:65` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/retention.rs:530` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/lcm/retention.rs:530` — `crate::db::engine::Rows`
- `crates/tracedecay-sessions/src/runtime/lcm/schema.rs:3` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/schema.rs:4` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/lcm/schema.rs:570` — `crate::db::engine::TestConnection`
- `crates/tracedecay-sessions/src/runtime/lcm/types.rs:20` — `crate::db::engine::Error`
- `crates/tracedecay-sessions/src/runtime/lcm/types.rs:21` — `crate::db::engine::Error`
- `crates/tracedecay-sessions/src/runtime/lcm/util.rs:3` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:240` — `crate::db::DaemonDatabaseScope`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:645` — `crate::db::enter_daemon_database_scope`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:37` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1799` — `crate::db::engine::TestConnection::open`
- `crates/tracedecay-sessions/src/runtime/workflow_index/port.rs:9` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:7` — `crate::db::engine::TestConnection`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:9` — `crate::db::engine::TestConnection::open`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:20` — `crate::db::engine::Result`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:20` — `crate::db::engine::Rows`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:22` — `crate::db::engine::IntoParams`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:24` — `crate::db::engine::Error::Runtime`
- `crates/tracedecay-sessions/src/runtime/workflow_index/tests.rs:285` — `crate::db::engine::TestConnection::open`
- `crates/tracedecay-sessions/src/runtime/workflow_index.rs:23` — `crate::db::engine`
- `crates/tracedecay-sessions/src/runtime/workflow_index.rs:61` — `crate::db::engine::Error`
- `crates/tracedecay-sessions/src/runtime/workflow_index.rs:62` — `crate::db::engine::Error`
- `crates/tracedecay-sessions/src/runtime/workflow_index.rs:720` — `crate::db::engine::TestConnection::open`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:19` — `crate::db::DaemonDatabaseScope`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:41` — `crate::db::enter_daemon_database_scope`
- `crates/tracedecay-sessions/src/runtime/workflow_state.rs:14` — `crate::db::engine`

### `crate::privacy` (34 references)

- `crates/tracedecay-sessions/src/runtime/claude/frames.rs:7` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/claude/parser.rs:10` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/claude/source_records.rs:9` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/claude/tests.rs:94` — `crate::privacy::MAX_OBSERVATION_RECORD_BYTES`
- `crates/tracedecay-sessions/src/runtime/claude/tests.rs:669` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/claude/tests.rs:719` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/claude/tests.rs:799` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/claude/tests.rs:883` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/claude.rs:26` — `crate::privacy::protect_sensitive_structural_id`
- `crates/tracedecay-sessions/src/runtime/claude.rs:494` — `crate::privacy::sanitize_provider_metadata_text`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:30` — `crate::privacy::PrivacySanitizerError`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:920` — `crate::privacy::ClaudeRecordSanitizerV1`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:2073` — `crate::privacy::MAX_OBSERVATION_RECORD_BYTES`
- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:2093` — `crate::privacy::MAX_OBSERVATION_RECORD_BYTES`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:29` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/codex/observation.rs:23` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:28` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/capture.rs:23` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/cursor_composer/sqlite.rs:13` — `crate::privacy::MAX_OBSERVATION_RECORD_BYTES`
- `crates/tracedecay-sessions/src/runtime/hermes/observation.rs:17` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/hermes/rows.rs:4` — `crate::privacy::MAX_OBSERVATION_RECORD_BYTES`
- `crates/tracedecay-sessions/src/runtime/hermes/tests.rs:14` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/hermes/tests.rs:293` — `crate::privacy::ClaudeRecordSanitizerV1::observation_v1`
- `crates/tracedecay-sessions/src/runtime/hermes/tests.rs:301` — `crate::privacy::ObservationSanitizationOutcomeV1::Durable`
- `crates/tracedecay-sessions/src/runtime/hermes.rs:21` — `crate::privacy::MAX_OBSERVATION_RECORD_BYTES`
- `crates/tracedecay-sessions/src/runtime/ingest/tests.rs:482` — `crate::privacy::PrivacySanitizerError::InvalidPolicy`
- `crates/tracedecay-sessions/src/runtime/jsonl_observation_admission.rs:15` — `crate::privacy::ParsedObservationRecordV1`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:35` — `crate::privacy::parse_normalized_observation_record_v1`
- `crates/tracedecay-sessions/src/runtime/snapshot_observation.rs:24` — `crate::privacy`
- `crates/tracedecay-sessions/src/runtime/source.rs:72` — `crate::privacy::PrivacySanitizerError`
- `crates/tracedecay-sessions/src/runtime/source.rs:544` — `crate::privacy::PrivacySanitizerError`
- `crates/tracedecay-sessions/src/runtime/source.rs:545` — `crate::privacy::PrivacySanitizerError`
- `crates/tracedecay-sessions/src/runtime/source.rs:546` — `crate::privacy::protect_sensitive_structural_id`
- `crates/tracedecay-sessions/src/runtime/vibe.rs:27` — `crate::privacy`

### `crate::errors` (12 references)

- `crates/tracedecay-sessions/src/runtime/codex_app_server.rs:22` — `crate::errors`
- `crates/tracedecay-sessions/src/runtime/cursor_agent.rs:8` — `crate::errors`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:141` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:167` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:174` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:273` — `crate::errors::TraceDecayError`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:274` — `crate::errors::TraceDecayError::Database`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:504` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1256` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1279` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1286` — `crate::errors::Result`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:1310` — `crate::errors::Result`

### `crate::config` (9 references)

- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:1642` — `crate::config::PinnedUserDataDir::new`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:890` — `crate::config::discover_project_root`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:1584` — `crate::config::discover_project_root`
- `crates/tracedecay-sessions/src/runtime/cursor.rs:1588` — `crate::config::discover_project_root`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:485` — `crate::config::has_project_database`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:538` — `crate::config::has_project_database`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:107` — `crate::config::RetentionConfig::default`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:132` — `crate::config::lock_user_data_dir_test_env`
- `crates/tracedecay-sessions/src/runtime/shared.rs:205` — `crate::config::discover_project_root`

### `crate::global_db` (9 references)

- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:1162` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:6` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/ingest/startup.rs:4` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/ingest/user.rs:5` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:21` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:51` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:52` — `crate::global_db::session_temporal::RegisteredGlobalDbSessionTemporalExecution`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:40` — `crate::global_db::RegisteredGlobalDb`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:8` — `crate::global_db::RegisteredGlobalDb`

### `crate::storage` (9 references)

- `crates/tracedecay-sessions/src/runtime/claude_observation.rs:1643` — `crate::storage::default_profile_root`
- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/runner.rs:20` — `crate::storage`
- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:180` — `crate::storage::read_repository_identity_marker`
- `crates/tracedecay-sessions/src/runtime/lcm/payload/filesystem_authority.rs:1071` — `crate::storage::set_private_dir_permissions`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:54` — `crate::storage`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:778` — `crate::storage::try_acquire_sidecar_lock`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:51` — `crate::storage::write_enrollment_marker`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:53` — `crate::storage::EnrollmentMarker`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:55` — `crate::storage::StorageMode::ProfileSharded`

### `crate::store` (8 references)

- `crates/tracedecay-sessions/src/runtime/git_correlation/backfill.rs:3` — `crate::store::GlobalDbGitCorrelationStore`
- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:10` — `crate::store`
- `crates/tracedecay-sessions/src/runtime/ingest/scheduler.rs:9` — `crate::store::TranscriptIngestStore`
- `crates/tracedecay-sessions/src/runtime/ingest/user.rs:9` — `crate::store::GlobalDbTranscriptStore`
- `crates/tracedecay-sessions/src/runtime/ingest/user_provider.rs:9` — `crate::store::TranscriptIngestStore`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:58` — `crate::store::GlobalDbSessionTemporalStore`
- `crates/tracedecay-sessions/src/runtime/source.rs:53` — `crate::store::TranscriptIngestStore`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest.rs:25` — `crate::store::GlobalDbWorkflowStore`

### `crate::worktree` (8 references)

- `crates/tracedecay-sessions/src/runtime/git_correlation/backfill.rs:569` — `crate::worktree::git_worktree_root`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:484` — `crate::worktree::git_worktree_root`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:537` — `crate::worktree::git_worktree_root`
- `crates/tracedecay-sessions/src/runtime/shared.rs:177` — `crate::worktree::git_worktree_root`
- `crates/tracedecay-sessions/src/runtime/shared.rs:178` — `crate::worktree::git_common_dir`
- `crates/tracedecay-sessions/src/runtime/shared.rs:191` — `crate::worktree::git_worktree_root`
- `crates/tracedecay-sessions/src/runtime/shared.rs:197` — `crate::worktree::git_common_dir`
- `crates/tracedecay-sessions/src/runtime/shared.rs:427` — `crate::worktree::git_worktree_root`

### `crate::agents` (7 references)

- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/runner.rs:1130` — `crate::agents::kiro_data_dir`
- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/runner.rs:1179` — `crate::agents::vscode_data_dir`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:159` — `crate::agents::vscode_data_dir`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:170` — `crate::agents::vscode_data_dir`
- `crates/tracedecay-sessions/src/runtime/cline_like.rs:181` — `crate::agents::vscode_data_dir`
- `crates/tracedecay-sessions/src/runtime/hermes/ingest.rs:8` — `crate::agents::hermes::read_config_pinned_project_root`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:134` — `crate::agents::kiro_data_dir`

### `crate::accounting` (6 references)

- `crates/tracedecay-sessions/src/runtime/claude/record_metadata.rs:6` — `crate::accounting::parser::parse_timestamp`
- `crates/tracedecay-sessions/src/runtime/claude/source_records.rs:8` — `crate::accounting::parser::parse_timestamp`
- `crates/tracedecay-sessions/src/runtime/codex/records.rs:11` — `crate::accounting::parser::parse_timestamp`
- `crates/tracedecay-sessions/src/runtime/kiro.rs:901` — `crate::accounting::parser::parse_timestamp`
- `crates/tracedecay-sessions/src/runtime/transcript_backfill.rs:658` — `crate::accounting::parser::parse_timestamp`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest.rs:14` — `crate::accounting::parser::parse_timestamp`

### `crate::daemon` (6 references)

- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:18` — `crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1`
- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:61` — `crate::daemon::profile_identity::load_or_create`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:50` — `crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:641` — `crate::daemon::profile_identity::load_or_create`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:7` — `crate::daemon::store_runtime::session_registry::DaemonSessionRuntimeRegistryV1`
- `crates/tracedecay-sessions/src/runtime/workflow_ingest/tests.rs:39` — `crate::daemon::profile_identity::load_or_create`

### `crate::git` (3 references)

- `crates/tracedecay-sessions/src/runtime/codex/tests.rs:158` — `crate::git::git_program`
- `crates/tracedecay-sessions/src/runtime/git_correlation/backfill.rs:296` — `crate::git::git_output`
- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:344` — `crate::git::git_program`

### `crate::tracedecay` (3 references)

- `crates/tracedecay-sessions/src/runtime/ingest/tests.rs:621` — `crate::tracedecay::current_timestamp`
- `crates/tracedecay-sessions/src/runtime/lcm/doctor.rs:10` — `crate::tracedecay::current_timestamp`
- `crates/tracedecay-sessions/src/runtime/lcm/payload.rs:5` — `crate::tracedecay::current_timestamp`

### `crate::user_config` (3 references)

- `crates/tracedecay-sessions/src/runtime/lcm/raw.rs:758` — `crate::user_config::UserConfig::load`
- `crates/tracedecay-sessions/src/runtime/lcm/raw.rs:761` — `crate::user_config::UserConfig`
- `crates/tracedecay-sessions/src/runtime/lcm/raw.rs:1133` — `crate::user_config::UserConfig`

### `crate::hooks` (2 references)

- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/baseline.rs:6` — `crate::hooks`
- `crates/tracedecay-sessions/src/runtime/ingest/user.rs:485` — `crate::hooks::schedule_user_session_review`

### `crate::os_str_bytes` (2 references)

- `crates/tracedecay-sessions/src/runtime/claude/cursor.rs:35` — `crate::os_str_bytes::native_os_str_bytes`
- `crates/tracedecay-sessions/src/runtime/claude/cursor.rs:42` — `crate::os_str_bytes::native_os_str_bytes`

### `crate::serde_util` (2 references)

- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:416` — `crate::serde_util::is_default`
- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:418` — `crate::serde_util::is_default`

### `crate::timeutil` (2 references)

- `crates/tracedecay-sessions/src/runtime/claude_observation_benchmark/model.rs:6` — `crate::timeutil::nearest_rank`
- `crates/tracedecay-sessions/src/runtime/session_temporal_benchmark.rs:59` — `crate::timeutil::nearest_rank`

### `crate::context` (1 references)

- `crates/tracedecay-sessions/src/runtime/git_correlation.rs:542` — `crate::context::read_cache::digest_bytes`

### `crate::dashboard` (1 references)

- `crates/tracedecay-sessions/src/runtime/lcm/dashboard_fixes_tests.rs:79` — `crate::dashboard::scope::resolve_dashboard_scope`

### `crate::repository_provenance` (1 references)

- `crates/tracedecay-sessions/src/runtime/ingest/project.rs:7` — `crate::repository_provenance::RepositoryProvenanceAdmissionContext`

### `crate::sqlite_read_snapshot` (1 references)

- `crates/tracedecay-sessions/src/runtime/lcm/maintenance.rs:42` — `crate::sqlite_read_snapshot::backup_live_sqlite_database`

### `crate::text` (1 references)

- `crates/tracedecay-sessions/src/runtime/shared.rs:246` — `crate::text::utf8_prefix_at_or_before`

### `crate::windows_file` (1 references)

- `crates/tracedecay-sessions/src/runtime/source.rs:862` — `crate::windows_file::information`

