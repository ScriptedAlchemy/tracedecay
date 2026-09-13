# Shipped persisted shapes and how this binary admits them

Index of every admission in the workspace that pins a persisted shape — an
exact object inventory, a digest, a trigger body, a version marker, a file
format revision, `PRAGMA user_version`, or the contents of a migrations table —
together with the shape each released binary actually wrote and whether this
binary admits it.

The released shapes are reconstructed from the tagged sources, never from the
current contract. A fixture derived from the current contract agrees with
whatever the admission expects, including an expectation no release ever wrote,
which is how five of these regressions reached a real profile one install at a
time.

## Release window

`v0.1.0-beta.25` through `v0.1.0-beta.37` (the newest tag; `beta.28` was never
tagged). Every marker constant in `crates/**` outside tests was compared
between `v0.1.0-beta.37` and this tree: of 46 markers present in both, five
differ, and only three of those describe persisted state
(`migrations::SCHEMA_VERSION` 34 → 35, `FINAL_CONFIGURATION_SCHEMA_DIGEST`,
`GIT_CORRELATION_SCHEMA_VERSION` 4 → 5). The other two are an in-memory cache
and a pagination cursor.

## Admissions

| admission site | store | shipped shape (beta.25 … beta.37) | verdict before | verdict now | test |
| --- | --- | --- | --- | --- | --- |
| `runtime_core::db::migrations::final_shape::{require_exact_final_shape, require_final_shape_except_payload_digests}` | project `tracedecay.db` | one byte-identical inventory: `user_version` 34, 183 objects, digest `0126b4dd550109a6` | **refused**: `database schema has incompatible table 'diagnostic_generation_publications'` | admitted, converged by `released_shape::converge_released_project_schema`, every row retained | `db::migrations::tests::final_shape::released_project_store_migrates_and_retains_every_row` |
| `runtime_core::db::migrations::unsupported_schema_version` | project `tracedecay.db` | — | refused for any stamp but 34 or 35 | unchanged: no release wrote another stamp | `db::migrations::tests::final_shape::stamped_final_store_with_missing_or_tampered_required_shape_is_reset_required` |
| `global_db::session_temporal_schema::admission::classify_registered_schema_admission` | `global.db`, profile and project `sessions.db` | schema marker 3, one 81-trigger authority inventory | **refused**: `released v3 authority trigger contracts are absent or incompatible` | admitted as `ReleasedV3` (fixed in `29def5d8bb`) | `tests::lcm_schema::temporal_catalog::admission::published_v3_authority_triggers_migrate_and_retain_every_session` |
| `global_db::schema_stages` configuration digest | profile configuration | `FINAL_CONFIGURATION_SCHEMA_DIGEST` `sha256:99b8f5f5…` | **refused**: unreleased `2b3eab89e2` re-pinned the digest after `f01d6da607` dropped `configuration_credential_references` | admitted through `RELEASED_CONFIGURATION_SCHEMA_DIGEST` | `configuration::schema` suite |
| `runtime_core::db::migrations::install_runtime_writer_ledger` / `global_db::schema_stages::converge_runtime_writer_ledger` | project `tracedecay.db`, profile `user-sessions.db`, project `sessions.db` | `td_runtime_writer_{checkpoint_v1, idempotency_v1, inbox_v1, outbox_v1}` | **refused**: canonical shape carries `idempotency_v2` | admitted, `v1` folded into `v2` in bounded pages (`b703616d7d`, `7f7cd14dd0`, `63ab366df5`) | `db::migrations::tests::final_shape::runtime_writer_ledger_is_part_of_the_final_shape` |
| `global_db::schema_stages` workflow contracts | `global.db` | `WORKFLOW_SCHEMA_VERSION_V1` 1, byte-identical `WORKFLOW_TABLE_CONTRACTS_V1` | admitted | unchanged | `schema_stages` workflow suite |
| `lcm::schema::require_admissible_lcm_schema` | profile `user-sessions.db` | `session_schema_migrations.lcm` 8 | admitted | unchanged | `lcm::schema` suite |
| `sessions::runtime::git_correlation::ensure_git_correlation_receipt_schema_in_transaction` | `sessions.db` | `session_schema_migrations.git_correlation` 4 | admitted: the install is `CREATE … IF NOT EXISTS` plus a marker upsert, so 4 converges to 5 additively | unchanged | `git_correlation` schema suite |
| `sessions::runtime::workflow_index::ensure_workflow_index_schema` | `sessions.db` | `session_schema_migrations.workflow_indexing` 1 | admitted | unchanged | `workflow_index` suite |
| `global_db::observation::schema::require_admitted_observation_shape` | `global.db` observation authority | canonical columns plus `global_schema_migrations` `observations-v2-canonical-autoincrement` | admitted: the native-source-scheme marker is enrolled on open for every authority that cannot double-count. A populated authority that does carry Cline-like native sources is refused on purpose — re-offering `<task>:ui_messages` would admit its events twice — and the remedy resets only the derived observation authority | unchanged | `observation::schema` suite |
| `global_db::registered_legacy_relations::require_admissible_legacy_relations` | registered session shard | none: the listed tables were already retired at `beta.25` | admitted | unchanged | `registered_legacy_relations` suite |
| `global_db::project_registry` | profile registry | — | admitted: both refusals read row content (a non-canonical `projects.path` key, two ids claiming one root), not a shape | unchanged | `project_registry` suite |
| `code_index::production::sealed_codec::sealed_generation_format_revision_is_compatible` | `code-index-v1` sealed generations, segments, read bundles | format revision 6 at every tag | admitted: `MINIMUM_SEALED_GENERATION_FORMAT_REVISION` is 6 and the compatible set is {6, 7} | unchanged | `sealed_codec` suite |
| `host_admission::spool::frames` | private-fs framed spool | `FRAME_MAGIC` `TDHA`, `FORMAT_VERSION` 1 | admitted: both unchanged | unchanged | `spool::frames` suite |
| `hooks::spool`, `hooks::admission_ledger` | hook spool and admission ledger | `TDH2`/`TDHC`/`TDL1`, spool format 1, checkpoint format 2, ledger format 1 | admitted: all unchanged | unchanged | `hooks::spool` suite |
| `maintenance::profile_backup` | profile backup archive | identity schema version 2 | admitted: unchanged | unchanged | `profile_backup` suite |
| `automation_runtime::automatic_facts` | automatic fact proposals | proposal schema markers unchanged | admitted | unchanged | `automatic_facts` suite |
| `dashboard_api::graph_structure_api::graph_reset_required` | published graph generations | — | admitted: the refusals validate a row's lineage and binding at read time rather than pinning a shape | unchanged | `graph_structure_api` suite |

## Fixtures

| fixture | shape |
| --- | --- |
| `crates/tracedecay-runtime-core/tests/fixtures/project-store-released-v34.sql` | the whole released project store, assembled from the tagged DDL constants |
| `crates/tracedecay-global-db/tests/fixtures/session-temporal-released-v3-triggers.sql` | the released session-temporal authority triggers, extracted verbatim from the tag |
| `crates/tracedecay-global-db/tests/fixtures/session-relation-receipts-before-recovery.sql` | the session-relation receipt shape that predates receipt recovery |

## Not a persisted shape

`temporal_query::cursor::CURSOR_FORMAT_VERSION` moved from `"2"` to `"3"`. A
pagination cursor is an opaque continuation handed back to a caller within one
session, not persisted state, so an older cursor is refused rather than
migrated and the caller re-runs its query.
