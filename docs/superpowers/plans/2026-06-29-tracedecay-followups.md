# TraceDecay Followups Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the post-introspection TraceDecay follow-ups with testable, bounded improvements instead of broad rewrites.

**Architecture:** Keep primary project indexing fast and conservative, then add explicit diagnostic and lazy-expansion surfaces around the edges: registry/store doctor output, normalized telemetry ingestion, memory curation proposals, safer transcript search metadata, daemon health summaries, and ignored-dependency symbol candidates. Each slice should produce a useful CLI/MCP behavior on its own.

**Tech Stack:** Rust, Tokio, rusqlite, serde_json, clap, existing TraceDecay CLI/MCP/test harnesses.

---

### Task 1: Registry And Storage Doctor Drift Report

**Files:**
- Modify: `src/doctor.rs`
- Modify: `src/global_db.rs`
- Test: `tests/cli_non_interactive_test.rs`
- Test: `tests/global_registry_test.rs`

- [ ] Add a failing CLI test that creates a global registry row, a project `config.json`, and a `store_manifest.json` with mismatched root/project id metadata, then runs a non-interactive doctor-style command and asserts the report names each mismatch.
- [ ] Add a failing unit/integration test for a pure helper that returns `RegistryDriftFinding` rows for active root vs global registry vs config vs manifest.
- [ ] Implement the helper using existing global registry and manifest read paths, keeping it read-only unless an explicit repair flag is later added.
- [ ] Surface the helper in doctor output with bounded rows and actionable wording.
- [ ] Run `cargo test --test global_registry_test registry_ -- --nocapture` and the targeted CLI test.

### Task 2: Storage Hygiene And Response Handle Inventory

**Files:**
- Modify: `src/doctor.rs`
- Modify: `src/mcp/response_handles.rs`
- Modify: `crates/tracedecay-migrate/src/inventory.rs`
- Test: `tests/cli_non_interactive_test.rs`
- Test: `tests/mcp_handler_test.rs`

- [ ] Add a failing test that creates stale `/tmp` aliases, orphan store manifests, expired response handles, and branch DB retention candidates, then asserts doctor/status output reports counts without exposing payload bodies.
- [ ] Reuse existing inventory scans where possible and add small helper structs for `StorageHygieneReport`.
- [ ] Add a dry-run only report path first; defer destructive cleanup behind existing explicit GC/repair gates.
- [ ] Run targeted doctor/inventory tests.

### Task 3: Normalized Tool, Skill, Hint, And Transport Analytics

**Files:**
- Modify: `src/analytics.rs`
- Modify: `src/hooks/mod.rs`
- Modify: `src/hooks/tool_hints.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/skill_usage/analytics.rs`
- Test: `tests/skill_usage_test.rs`
- Test: `tests/hooks_test.rs`

- [ ] Add failing tests for analytics events carrying `tool_name`, `tool_kind`, `skill_name`, `hint_category`, `hint_id`, `transport`, and `failure_reason`.
- [ ] Extend analytics event parsing/storage to normalize these fields without breaking existing records.
- [ ] Wire hook/hint emission to populate the normalized fields atomically.
- [ ] Add dedupe/concurrency-safe JSONL append coverage where hooks currently write sidecars.
- [ ] Run targeted analytics and hook tests.

### Task 4: Project Memory Curation Automation

**Files:**
- Modify: `crates/tracedecay-agent-hosts/src/automation/memory_curator.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/fact_proposals.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/runner.rs`
- Test: `tests/automation_memory_curator_runner_test.rs`
- Test: `tests/memory_test.rs`

- [ ] Add failing tests where repeated high-confidence session facts produce deduped fact proposals scoped to a project.
- [ ] Add tests that stale/contradictory facts are proposed for merge/prune rather than blindly written.
- [ ] Implement proposal generation that remains host-delegated when configuration says the host owns mutation decisions.
- [ ] Run memory curator and memory store tests.

### Task 5: Transcript Audit Scoping And Cursor Health

**Files:**
- Modify: `crates/tracedecay-sessions/src/runtime/lcm/query.rs`
- Modify: `crates/tracedecay-agent-hosts/src/agents/cursor.rs`
- Modify: `src/doctor.rs`
- Test: `tests/session_lcm_query_test.rs`
- Test: `tests/cursor_transcript_ingest_test.rs`

- [ ] Add failing tests for explicit project scoping defaults in transcript audit queries.
- [ ] Add a failing test for literal `${workspaceFolder}` Cursor startup paths producing a clear health finding instead of silent bad state.
- [ ] Extend LCM query metadata to report provider, source, project key, storage scope, and catch-up behavior.
- [ ] Keep `#` query escaping covered by existing grep tests.
- [ ] Run LCM query and Cursor transcript tests.

### Task 6: Daemon And Scheduler Status Summary

**Files:**
- Modify: `src/daemon.rs`
- Modify: `src/runtime_telemetry.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/scheduler.rs`
- Modify: `src/cli.rs`
- Test: `tests/tool_daemon_test.rs`
- Test: `tests/automation_scheduler_test.rs`

- [ ] Add failing tests for daemon status output containing bounded CPU/RAM, scheduler skip summaries, latest errors, backlog size, and storage sizing.
- [ ] Implement a read-only status snapshot helper that can be rendered by CLI and MCP without starting expensive scans.
- [ ] Add scheduler skip coalescing so repeated skip noise is summarized.
- [ ] Run daemon and scheduler tests.

### Task 7: Lazy Ignored-Dependency Indexing

**Files:**
- Modify: `src/config.rs`
- Modify: `src/tracedecay.rs`
- Modify: `src/db/unresolved.rs`
- Modify: `src/extraction/typescript_extractor.rs`
- Modify: `src/mcp/tools/definitions.rs`
- Modify: `src/mcp/tools/handlers/graph.rs`
- Test: `tests/sync_test.rs`
- Test: `tests/typescript_extraction_test.rs`
- Test: `tests/mcp_handler_test.rs`

- [ ] Add a failing test proving full sync still excludes `node_modules`.
- [ ] Add a failing test where a TypeScript `import type { Foo } from "pkg"` records an ignored dependency candidate instead of requiring broad unignore.
- [ ] Add a failing explicit lazy-index test that indexes only the package type entrypoint or deep import file under `node_modules/pkg`, bounded by file/byte limits.
- [ ] Add MCP/CLI status output that says a missing symbol may live behind an ignored dependency and suggests explicit lazy indexing.
- [ ] Keep dependency nodes marked as dependency-scope metadata, not project-owned source.
- [ ] Run targeted sync, TypeScript extraction, and MCP handler tests.

### Task 8: Final Integration

**Files:**
- Modify as needed based on changed modules.

- [ ] Run `cargo fmt --check`.
- [ ] Run `cargo check --workspace`.
- [ ] Run affected tests from TraceDecay once changes are known.
- [ ] Run `cargo test --workspace --no-run`.
- [ ] Run a full or CI-equivalent suite if time allows.
- [ ] Sync the TraceDecay index and inspect `tracedecay_diff_context` for changed files.
