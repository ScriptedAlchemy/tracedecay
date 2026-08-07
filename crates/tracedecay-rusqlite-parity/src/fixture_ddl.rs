//! Session-store fixture DDL shared verbatim by the unit and subprocess
//! test harnesses so the closed-table schemas cannot drift apart.
//!
//! These fixtures are deliberately weaker than production: they restate column
//! sets and primary keys but omit CHECK, FOREIGN KEY, and non-primary UNIQUE
//! clauses. That is sound for the tables this crate only probes, but two of
//! them — `retrieval_anchors` and `generation_diagnostics` — are real
//! production tables whose canonical DDL now lives in
//! `tracedecay_store::schema`. Their definitions below are therefore known
//! divergences, not authoritative: `retrieval_anchors` here has no
//! `CHECK(length(anchor_id) > 0)`, no `json_valid` checks on `anchor_json` and
//! `owner_json`, and no composite `UNIQUE(anchor_id, owner_json)`.
//!
//! Installing them from `tracedecay_store::schema` instead is the intended
//! end state. It requires this crate — today a dependency-light, process-
//! isolated probe with no `tracedecay-*` dependency other than the parity
//! protocol — to take a dependency on `tracedecay-store`, and requires
//! rechecking the schema-shape probes in `closed_sql.rs` against the stricter
//! definitions. Do not hand-copy the production constraints here instead; that
//! recreates the drift this note exists to record.

/// Faithful column set/order and primary keys for every closed session-store
/// table except `observations` (whose foreign-key clause differs between the
/// two harnesses). CHECK/FK clauses are omitted per the harness convention;
/// see the module note for the two tables where that convention diverges from
/// a real production schema.
#[doc(hidden)]
pub const SESSION_STORE_FIXTURE_TABLES_DDL: &str = "
            CREATE TABLE source_cursors (
                source_json TEXT NOT NULL,
                scope_json TEXT NOT NULL,
                cursor_json TEXT NOT NULL,
                PRIMARY KEY(source_json, scope_json)
            );
            CREATE TABLE sessions (
                provider TEXT NOT NULL,
                session_id TEXT NOT NULL,
                project_key TEXT NOT NULL,
                project_path TEXT NOT NULL,
                title TEXT,
                started_at INTEGER,
                ended_at INTEGER,
                transcript_path TEXT,
                metadata_json TEXT,
                parent_session_id TEXT,
                is_subagent INTEGER NOT NULL DEFAULT 0,
                agent_id TEXT,
                parent_tool_use_id TEXT,
                PRIMARY KEY(provider, session_id)
            );
            CREATE TABLE session_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                role TEXT NOT NULL,
                timestamp INTEGER,
                ordinal INTEGER NOT NULL,
                text TEXT NOT NULL,
                kind TEXT,
                model TEXT,
                tool_names TEXT,
                source_path TEXT,
                source_offset INTEGER,
                metadata_json TEXT,
                PRIMARY KEY(provider, message_id),
                FOREIGN KEY(provider, session_id)
                    REFERENCES sessions(provider, session_id) ON DELETE CASCADE
            );
            CREATE TABLE session_schema_migrations (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL
            );
            CREATE TABLE lcm_raw_messages (
                provider TEXT NOT NULL,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                store_id INTEGER PRIMARY KEY AUTOINCREMENT,
                role TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                timestamp INTEGER,
                content TEXT,
                content_hash TEXT NOT NULL,
                storage_kind TEXT NOT NULL,
                payload_ref TEXT,
                snippet_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                legacy_source INTEGER NOT NULL DEFAULT 0,
                legacy_truncated INTEGER NOT NULL DEFAULT 0,
                metadata_json TEXT,
                UNIQUE(provider, message_id),
                FOREIGN KEY(provider, session_id)
                    REFERENCES sessions(provider, session_id) ON DELETE CASCADE
            );
            CREATE TABLE session_temporal_schema_migrations (
                name TEXT PRIMARY KEY,
                version INTEGER NOT NULL,
                applied_at INTEGER NOT NULL
            );
            CREATE TABLE session_temporal_generations (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                state TEXT NOT NULL,
                frozen_watermarks_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                ready_at INTEGER,
                activated_at INTEGER,
                completed_at INTEGER,
                PRIMARY KEY(session_id, generation)
            );
            CREATE TABLE session_temporal_observation_effects (
                observation_id TEXT PRIMARY KEY,
                observation_sequence INTEGER NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                receipt_id TEXT NOT NULL,
                effect_digest TEXT NOT NULL,
                output_count INTEGER NOT NULL,
                recorded_at INTEGER NOT NULL
            );
            CREATE TABLE session_temporal_projection_receipts (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                batch_ordinal INTEGER NOT NULL,
                batch_digest TEXT NOT NULL,
                frozen_watermarks_json TEXT NOT NULL,
                source_through INTEGER NOT NULL,
                projection_through INTEGER NOT NULL,
                occurrence_count INTEGER NOT NULL,
                occurrence_digest TEXT NOT NULL,
                dimension_count INTEGER NOT NULL,
                dimension_digest TEXT NOT NULL,
                copy_count INTEGER NOT NULL,
                copy_digest TEXT NOT NULL,
                assertion_count INTEGER NOT NULL,
                assertion_digest TEXT NOT NULL,
                supersession_count INTEGER NOT NULL,
                supersession_digest TEXT NOT NULL,
                current_count INTEGER NOT NULL,
                current_digest TEXT NOT NULL,
                fts_count INTEGER NOT NULL,
                fts_digest TEXT NOT NULL,
                committed_at INTEGER NOT NULL,
                PRIMARY KEY(session_id, generation, batch_ordinal),
                UNIQUE(session_id, generation, batch_digest)
            );
            CREATE TABLE session_occurrences (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                occurrence_id TEXT NOT NULL,
                source_observation_id TEXT NOT NULL,
                source_provider TEXT NOT NULL,
                projection_output_ordinal INTEGER NOT NULL,
                retrieval_anchor_id TEXT NOT NULL,
                thread_id TEXT,
                thread_grouping_json TEXT,
                turn_id TEXT,
                turn_grouping_json TEXT,
                message_id TEXT,
                agent_id TEXT,
                role TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                valid_time_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                sanitized_content_digest TEXT NOT NULL,
                sanitized_content_bytes INTEGER NOT NULL,
                snippet_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                PRIMARY KEY(session_id, generation, occurrence_id)
            );
            CREATE TABLE session_assertions (
                session_id TEXT NOT NULL,
                generation INTEGER NOT NULL,
                assertion_id TEXT NOT NULL,
                assertion_kind TEXT NOT NULL,
                subject_anchor_id TEXT NOT NULL,
                object_anchor_id TEXT NOT NULL,
                knowledge_at INTEGER NOT NULL,
                valid_time_json TEXT NOT NULL,
                evidence_json TEXT NOT NULL,
                PRIMARY KEY(session_id, generation, assertion_id)
            );
            CREATE TABLE session_summary_nodes (
                summary_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                summary_anchor_id TEXT NOT NULL,
                summary_text TEXT NOT NULL,
                index_text TEXT NOT NULL,
                source_horizon_json TEXT NOT NULL,
                publication_json TEXT,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE memory_v2_facts (
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                identity_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY(fact_id, owner_kind, project_id)
            );
            CREATE TABLE memory_v2_assertions (
                assertion_id TEXT NOT NULL,
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                assertion_header_json TEXT NOT NULL,
                kind_json TEXT NOT NULL,
                payload_reference_json TEXT NOT NULL,
                receipt_json TEXT NOT NULL,
                asserted_at INTEGER NOT NULL,
                actor_id TEXT,
                PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id)
            );
            CREATE TABLE memory_v2_lineage_events (
                event_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL,
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                event_json TEXT NOT NULL,
                occurred_at INTEGER NOT NULL,
                recorded_at INTEGER NOT NULL
            );
            CREATE TABLE memory_v2_current_facts (
                fact_id TEXT NOT NULL,
                owner_kind TEXT NOT NULL,
                project_id TEXT NOT NULL,
                payload_access TEXT NOT NULL,
                trust_score REAL,
                active_assertion_id TEXT,
                last_event_id TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                retrieval_count INTEGER NOT NULL DEFAULT 0,
                access_count INTEGER NOT NULL DEFAULT 0,
                helpful_count INTEGER NOT NULL DEFAULT 0,
                unhelpful_count INTEGER NOT NULL DEFAULT 0,
                last_retrieved_at INTEGER,
                last_recalled_at INTEGER,
                last_feedback_at INTEGER,
                projection_state TEXT NOT NULL DEFAULT 'unavailable',
                vector_watermark_json TEXT,
                PRIMARY KEY(fact_id, owner_kind, project_id)
            );
            CREATE TABLE retrieval_anchors (
                anchor_id TEXT PRIMARY KEY,
                anchor_json TEXT NOT NULL,
                owner_json TEXT NOT NULL,
                projection_generation TEXT NOT NULL
            );
            CREATE TABLE generation_diagnostics (
                diagnostic_anchor TEXT PRIMARY KEY,
                generation_id TEXT NOT NULL,
                repository TEXT NOT NULL,
                worktree TEXT,
                reference TEXT,
                source_revision TEXT,
                file_occurrence_id TEXT NOT NULL,
                content_digest TEXT NOT NULL,
                symbol_occurrence_id TEXT,
                span_start INTEGER NOT NULL,
                span_end INTEGER NOT NULL,
                code TEXT NOT NULL,
                severity TEXT NOT NULL,
                message TEXT NOT NULL,
                message_digest TEXT NOT NULL,
                producer_kind TEXT NOT NULL,
                producer TEXT NOT NULL,
                analyzer_revision TEXT NOT NULL,
                configuration_revision TEXT NOT NULL,
                sanitization_receipt TEXT,
                evidence_class TEXT NOT NULL,
                collected_at INTEGER NOT NULL,
                record_state TEXT NOT NULL DEFAULT 'current',
                state_generation TEXT,
                persisted_at INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE diagnostic_generation_publications (
                generation_id TEXT PRIMARY KEY,
                record_state TEXT NOT NULL,
                state_generation TEXT,
                published_at INTEGER NOT NULL
            );
            CREATE TABLE configuration_revisions (
                revision_id TEXT PRIMARY KEY,
                parent_revision_id TEXT,
                snapshot_id TEXT NOT NULL,
                effective_behavior_digest TEXT NOT NULL,
                resolution_provenance_digest TEXT NOT NULL,
                actor_id TEXT NOT NULL,
                operation_kind TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE configuration_entries (
                revision_id TEXT NOT NULL,
                key TEXT NOT NULL,
                layer_kind TEXT NOT NULL,
                layer_id TEXT,
                schema_revision INTEGER NOT NULL,
                typed_value TEXT NOT NULL,
                PRIMARY KEY(revision_id, key, layer_kind, layer_id)
            );
            CREATE TABLE configuration_mutation_receipts (
                receipt_id TEXT PRIMARY KEY,
                plan_id TEXT,
                actor_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                base_revision_id TEXT NOT NULL,
                result_revision_id TEXT NOT NULL,
                operation_digest TEXT NOT NULL,
                authorization_policy_digest TEXT NOT NULL,
                activation_status TEXT NOT NULL,
                receipt_digest TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE configuration_audit_events (
                event_id TEXT PRIMARY KEY,
                actor_id TEXT NOT NULL,
                idempotency_key TEXT,
                operation_kind TEXT NOT NULL,
                base_revision_id TEXT NOT NULL,
                result_revision_id TEXT,
                sealed_target_reference BLOB,
                event_scoped_target_commitment TEXT NOT NULL,
                receipt_digest TEXT,
                correlation_id TEXT,
                safe_reason_code TEXT,
                occurred_at INTEGER NOT NULL
            );";
