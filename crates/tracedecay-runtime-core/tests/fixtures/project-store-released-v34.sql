-- The canonical project store (`tracedecay.db`) exactly as every release from
-- v0.1.0-beta.25 through v0.1.0-beta.37 created it.
--
-- Assembled verbatim from the tagged DDL constants that
-- `migrations::create_schema` and `final_shape::build_expected_final_shape`
-- compose, so this is the shape a shipped binary wrote rather than a shape
-- derived from the current contract. Deriving the released shape from the
-- current contract is what let two admission regressions ship green.
--
--   tag                              user_version  objects  inventory digest
--   v0.1.0-beta.25 .. v0.1.0-beta.37        34            183      0126b4dd550109a6
--   (working tree)                   35            190      ebae4fc200dd4e9f
--
-- The digest is sha256 over `name|sql` lines of `sqlite_master`. All 12 tags
-- produce one byte-identical inventory; the objects that differ from the
-- current contract are enumerated in the admission table this fixture's test
-- carries. A store loading this file must also carry `PRAGMA user_version =
-- 34`, which the test sets, because that stamp is what selects the
-- released admission path.

CREATE TABLE IF NOT EXISTS metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS read_cache (
        project_id   TEXT NOT NULL,
        session_id   TEXT NOT NULL,
        file_path    TEXT NOT NULL,
        mtime_ns     INTEGER NOT NULL,
        mode         TEXT NOT NULL,
        args_hash    TEXT NOT NULL,
        digest       TEXT NOT NULL,
        body         BLOB NOT NULL,
        token_count  INTEGER NOT NULL,
        created_at   INTEGER NOT NULL,
        PRIMARY KEY (project_id, session_id, file_path, mode, args_hash)
    );

    CREATE INDEX IF NOT EXISTS idx_read_cache_session
        ON read_cache(session_id, created_at);

CREATE TABLE IF NOT EXISTS retrieval_anchors (
        anchor_id TEXT PRIMARY KEY CHECK(length(anchor_id) > 0),
        anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        projection_generation TEXT NOT NULL CHECK(length(projection_generation) > 0)
    );
    -- SQLite requires an exact unique parent key for the composite owner-bound
    -- alias and evidence foreign keys, even though anchor_id is itself unique.
    CREATE UNIQUE INDEX IF NOT EXISTS idx_retrieval_anchors_owner
        ON retrieval_anchors(anchor_id, owner_json);

CREATE TABLE IF NOT EXISTS retrieval_anchor_aliases (
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        alias_kind TEXT NOT NULL CHECK(length(alias_kind) > 0),
        locator_digest TEXT NOT NULL CHECK(length(locator_digest) > 0),
        anchor_id TEXT NOT NULL,
        PRIMARY KEY(owner_json, alias_kind, locator_digest),
        UNIQUE(anchor_id, alias_kind, locator_digest),
        FOREIGN KEY(anchor_id, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json)
    );

CREATE TABLE IF NOT EXISTS retrieval_anchor_dispositions (
        sequence INTEGER PRIMARY KEY AUTOINCREMENT,
        disposition_id TEXT NOT NULL CHECK(length(disposition_id) > 0),
        anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        state TEXT NOT NULL
            CHECK(state IN (
                'active', 'superseded', 'redacted', 'expired', 'quarantined',
                'deleted', 'unavailable'
            )),
        superseded_by TEXT,
        reason_class TEXT NOT NULL CHECK(reason_class IN (
            'user_request', 'retention', 'redaction', 'quarantine',
            'correction', 'legal_hold', 'source_unavailable'
        )),
        effective_at INTEGER NOT NULL,
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        UNIQUE(owner_json, disposition_id),
        FOREIGN KEY(anchor_id, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json),
        FOREIGN KEY(superseded_by, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json),
        CHECK(
            (state = 'superseded' AND superseded_by IS NOT NULL)
            OR (state <> 'superseded' AND superseded_by IS NULL)
        )
    );
    CREATE INDEX IF NOT EXISTS idx_retrieval_anchor_dispositions_current
        ON retrieval_anchor_dispositions(anchor_id, owner_json, sequence DESC);

    CREATE TABLE IF NOT EXISTS retrieval_anchor_reverse_lineage (
        source_anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        derivative_kind TEXT NOT NULL
            CHECK(derivative_kind IN ('span', 'contribution', 'finding')),
        derivative_id TEXT NOT NULL CHECK(length(derivative_id) > 0),
        direct_evidence INTEGER NOT NULL CHECK(direct_evidence IN (0, 1)),
        PRIMARY KEY(
            source_anchor_id, owner_json, derivative_kind, derivative_id
        ),
        FOREIGN KEY(source_anchor_id, owner_json)
            REFERENCES retrieval_anchors(anchor_id, owner_json)
    );
    CREATE INDEX IF NOT EXISTS idx_retrieval_anchor_reverse_derivative
        ON retrieval_anchor_reverse_lineage(
            owner_json, derivative_kind, derivative_id, direct_evidence
        );

    CREATE TABLE IF NOT EXISTS retrieval_anchor_derivative_tombstones (
        source_anchor_id TEXT NOT NULL,
        owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
        derivative_kind TEXT NOT NULL
            CHECK(derivative_kind IN ('span', 'contribution', 'finding')),
        derivative_id TEXT NOT NULL CHECK(length(derivative_id) > 0),
        disposition_id TEXT NOT NULL,
        effective_at INTEGER NOT NULL,
        PRIMARY KEY(
            source_anchor_id, owner_json, derivative_kind, derivative_id,
            disposition_id
        ),
        FOREIGN KEY(
            source_anchor_id, owner_json, derivative_kind, derivative_id
        ) REFERENCES retrieval_anchor_reverse_lineage(
            source_anchor_id, owner_json, derivative_kind, derivative_id
        )
    );

CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_update
    BEFORE UPDATE ON retrieval_anchors BEGIN
        SELECT RAISE(ABORT, 'retrieval anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchors_immutable_delete
    BEFORE DELETE ON retrieval_anchors BEGIN
        SELECT RAISE(ABORT, 'retrieval anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_update
    BEFORE UPDATE ON retrieval_anchor_aliases BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor aliases are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_aliases_immutable_delete
    BEFORE DELETE ON retrieval_anchor_aliases BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor aliases are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_dispositions_immutable_update
    BEFORE UPDATE ON retrieval_anchor_dispositions BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor dispositions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_dispositions_immutable_delete
    BEFORE DELETE ON retrieval_anchor_dispositions BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor dispositions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_reverse_lineage_immutable_update
    BEFORE UPDATE ON retrieval_anchor_reverse_lineage BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor reverse lineage is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_reverse_lineage_immutable_delete
    BEFORE DELETE ON retrieval_anchor_reverse_lineage BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor reverse lineage is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_derivative_tombstones_immutable_update
    BEFORE UPDATE ON retrieval_anchor_derivative_tombstones BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor derivative tombstones are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS retrieval_anchor_derivative_tombstones_immutable_delete
    BEFORE DELETE ON retrieval_anchor_derivative_tombstones BEGIN
        SELECT RAISE(ABORT, 'retrieval anchor derivative tombstones are immutable');
    END;

CREATE TABLE IF NOT EXISTS memory_v2_facts (
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            identity_json TEXT NOT NULL CHECK(json_valid(identity_json)),
            created_at INTEGER NOT NULL,
            PRIMARY KEY(fact_id, owner_kind, project_id),
            UNIQUE(fact_id, owner_json),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_assertions (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            assertion_header_json TEXT NOT NULL CHECK(json_valid(assertion_header_json)),
            kind_json TEXT NOT NULL CHECK(json_valid(kind_json)),
            payload_reference_json TEXT NOT NULL CHECK(json_valid(payload_reference_json)),
            receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
            asserted_at INTEGER NOT NULL,
            actor_id TEXT,
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id),
            UNIQUE(assertion_id, owner_json),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_supersession (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            superseded_assertion_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id, ordinal),
            UNIQUE(assertion_id, fact_id, owner_kind, project_id, superseded_assertion_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(superseded_assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_payloads (
            rowid INTEGER PRIMARY KEY AUTOINCREMENT,
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
            content TEXT NOT NULL,
            UNIQUE(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id)
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS memory_v2_assertion_payloads_fts USING fts5(
            content,
            content='memory_v2_assertion_payloads',
            content_rowid='rowid'
        );
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_fts_insert
        AFTER INSERT ON memory_v2_assertion_payloads BEGIN
            INSERT INTO memory_v2_assertion_payloads_fts(rowid, content)
            VALUES(NEW.rowid, NEW.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_fts_delete
        AFTER DELETE ON memory_v2_assertion_payloads BEGIN
            INSERT INTO memory_v2_assertion_payloads_fts(
                memory_v2_assertion_payloads_fts, rowid, content
            ) VALUES('delete', OLD.rowid, OLD.content);
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_payloads_no_update
        BEFORE UPDATE ON memory_v2_assertion_payloads BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payloads are immutable');
        END;

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_payload_purges (
            assertion_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_reference_json TEXT NOT NULL CHECK(json_valid(payload_reference_json)),
            detector_revision TEXT NOT NULL CHECK(length(detector_revision) > 0),
            purge_reason TEXT NOT NULL CHECK(purge_reason = 'detector_flagged'),
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id)
        );
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_payload_purges_no_update
        BEFORE UPDATE ON memory_v2_assertion_payload_purges BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payload purge receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_payload_purges_no_delete
        BEFORE DELETE ON memory_v2_assertion_payload_purges BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion payload purge receipts are immutable');
        END;

        CREATE TABLE IF NOT EXISTS memory_v2_evidence (
            evidence_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            anchor_id TEXT NOT NULL,
            evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
            PRIMARY KEY(evidence_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(anchor_id, owner_json)
                REFERENCES retrieval_anchors(anchor_id, owner_json)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_assertion_evidence (
            assertion_id TEXT NOT NULL,
            evidence_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
            PRIMARY KEY(assertion_id, fact_id, owner_kind, project_id, ordinal),
            UNIQUE(assertion_id, fact_id, owner_kind, project_id, evidence_id),
            FOREIGN KEY(assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(evidence_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_evidence(evidence_id, fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_lineage_events (
            event_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
            event_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            event_json TEXT NOT NULL CHECK(json_valid(event_json)),
            occurred_at INTEGER NOT NULL,
            recorded_at INTEGER NOT NULL,
            UNIQUE(event_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_current_facts (
            fact_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL,
            project_id TEXT NOT NULL,
            payload_access TEXT NOT NULL CHECK(payload_access IN (
                'eligible', 'redacted', 'quarantined', 'retention_expired',
                'deleted', 'unavailable', 'ambiguous'
            )),
            trust_score REAL CHECK(
                trust_score IS NULL OR (trust_score >= 0.0 AND trust_score <= 1.0)
            ),
            active_assertion_id TEXT,
            last_event_id TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            retrieval_count INTEGER NOT NULL DEFAULT 0 CHECK(retrieval_count >= 0),
            access_count INTEGER NOT NULL DEFAULT 0 CHECK(access_count >= 0),
            helpful_count INTEGER NOT NULL DEFAULT 0 CHECK(helpful_count >= 0),
            unhelpful_count INTEGER NOT NULL DEFAULT 0 CHECK(unhelpful_count >= 0),
            last_retrieved_at INTEGER,
            last_recalled_at INTEGER,
            last_feedback_at INTEGER,
            PRIMARY KEY(fact_id, owner_kind, project_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(active_assertion_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(last_event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id)
        );

        CREATE TABLE IF NOT EXISTS memory_v2_automatic_fact_receipts (
            apply_id TEXT NOT NULL,
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            owner_json TEXT NOT NULL CHECK(json_valid(owner_json)),
            idempotency_key TEXT NOT NULL,
            request_digest TEXT NOT NULL,
            request_json TEXT NOT NULL CHECK(json_valid(request_json)),
            evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
            state TEXT NOT NULL CHECK(state IN ('applied', 'quarantined')),
            quarantine_reason TEXT,
            applied_fact_id TEXT,
            applied_assertion_id TEXT,
            applied_event_id TEXT,
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(apply_id, owner_kind, project_id),
            UNIQUE(owner_kind, project_id, idempotency_key),
            UNIQUE(owner_kind, project_id, request_digest),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            FOREIGN KEY(applied_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(applied_assertion_id, applied_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_assertions(assertion_id, fact_id, owner_kind, project_id),
            FOREIGN KEY(applied_event_id, applied_fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (state = 'applied'
                    AND quarantine_reason IS NULL
                    AND applied_fact_id IS NOT NULL
                    AND applied_event_id IS NOT NULL) OR
                (state = 'quarantined'
                    AND quarantine_reason IS NOT NULL
                    AND applied_fact_id IS NULL
                    AND applied_assertion_id IS NULL
                    AND applied_event_id IS NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_assertions_fact
            ON memory_v2_assertions(fact_id, owner_kind, project_id, asserted_at);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_events_fact
            ON memory_v2_lineage_events(fact_id, owner_kind, project_id, event_sequence);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_events_as_of
            ON memory_v2_lineage_events(
                fact_id, owner_kind, project_id, occurred_at, event_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_current_page
            ON memory_v2_current_facts(owner_kind, project_id, fact_id);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_evidence_anchor
            ON memory_v2_evidence(anchor_id, owner_json);
        CREATE INDEX IF NOT EXISTS idx_memory_v2_automatic_fact_receipt_list
            ON memory_v2_automatic_fact_receipts(
                owner_kind, project_id, state, recorded_at, apply_id
            );

        CREATE TRIGGER IF NOT EXISTS memory_v2_facts_no_update
        BEFORE UPDATE ON memory_v2_facts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact identities are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_facts_no_delete
        BEFORE DELETE ON memory_v2_facts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 fact identities are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertions_no_update
        BEFORE UPDATE ON memory_v2_assertions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertions are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertions_no_delete
        BEFORE DELETE ON memory_v2_assertions BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertions are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_supersession_no_update
        BEFORE UPDATE ON memory_v2_assertion_supersession BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion supersession is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_supersession_no_delete
        BEFORE DELETE ON memory_v2_assertion_supersession BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion supersession is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_evidence_no_update
        BEFORE UPDATE ON memory_v2_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_evidence_no_delete
        BEFORE DELETE ON memory_v2_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_evidence_no_update
        BEFORE UPDATE ON memory_v2_assertion_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_assertion_evidence_no_delete
        BEFORE DELETE ON memory_v2_assertion_evidence BEGIN
            SELECT RAISE(ABORT, 'memory_v2 assertion evidence is immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_events_no_update
        BEFORE UPDATE ON memory_v2_lineage_events BEGIN
            SELECT RAISE(ABORT, 'memory_v2 lineage events are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_events_no_delete
        BEFORE DELETE ON memory_v2_lineage_events BEGIN
            SELECT RAISE(ABORT, 'memory_v2 lineage events are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_automatic_fact_receipts_no_update
        BEFORE UPDATE ON memory_v2_automatic_fact_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_automatic_fact_receipts_no_delete
        BEFORE DELETE ON memory_v2_automatic_fact_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts are immutable');
        END;

CREATE TABLE IF NOT EXISTS memory_v2_operation_receipts (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            operation_id TEXT NOT NULL CHECK(length(operation_id) > 0),
            operation_kind TEXT NOT NULL CHECK(operation_kind IN (
                'add', 'update', 'remove', 'feedback', 'retrieval',
                'curation', 'merge', 'automatic_fact_apply'
            )),
            request_digest TEXT NOT NULL CHECK(length(request_digest) > 0),
            fact_id TEXT,
            event_id TEXT,
            receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
            recorded_at INTEGER NOT NULL,
            PRIMARY KEY(owner_kind, project_id, operation_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(event_id IS NULL OR fact_id IS NOT NULL),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            )
        );

        CREATE TABLE IF NOT EXISTS memory_v2_feedback_history (
            owner_kind TEXT NOT NULL CHECK(owner_kind IN ('profile', 'project')),
            project_id TEXT NOT NULL,
            fact_id TEXT NOT NULL,
            event_id TEXT NOT NULL,
            action TEXT NOT NULL CHECK(action IN ('helpful', 'unhelpful')),
            old_trust REAL NOT NULL CHECK(old_trust >= 0.0 AND old_trust <= 1.0),
            new_trust REAL NOT NULL CHECK(new_trust >= 0.0 AND new_trust <= 1.0),
            occurred_at INTEGER NOT NULL,
            source TEXT,
            note TEXT,
            details_availability TEXT NOT NULL CHECK(
                details_availability IN ('available', 'redacted', 'unknown')
            ),
            PRIMARY KEY(owner_kind, project_id, fact_id, event_id),
            FOREIGN KEY(fact_id, owner_kind, project_id)
                REFERENCES memory_v2_facts(fact_id, owner_kind, project_id),
            FOREIGN KEY(event_id, fact_id, owner_kind, project_id)
                REFERENCES memory_v2_lineage_events(event_id, fact_id, owner_kind, project_id),
            CHECK(
                (owner_kind = 'profile' AND project_id = '') OR
                (owner_kind = 'project' AND project_id <> '')
            ),
            CHECK(
                details_availability = 'available' OR (source IS NULL AND note IS NULL)
            )
        );

        CREATE INDEX IF NOT EXISTS idx_memory_v2_operation_receipts_fact
            ON memory_v2_operation_receipts(
                fact_id, owner_kind, project_id, recorded_at
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_operation_receipts_automation_run
            ON memory_v2_operation_receipts(
                owner_kind, project_id, operation_kind,
                json_extract(receipt_json, '$.automation_run_id'),
                recorded_at, operation_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_automatic_fact_receipts_automation_run
            ON memory_v2_automatic_fact_receipts(
                owner_kind, project_id,
                json_extract(request_json, '$.automation_run_id'),
                recorded_at, apply_id
            );
        CREATE INDEX IF NOT EXISTS idx_memory_v2_feedback_history_fact
            ON memory_v2_feedback_history(
                owner_kind, project_id, fact_id, occurred_at, event_id
            );
        CREATE TRIGGER IF NOT EXISTS memory_v2_operation_receipts_no_update
        BEFORE UPDATE ON memory_v2_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_operation_receipts_no_delete
        BEFORE DELETE ON memory_v2_operation_receipts BEGIN
            SELECT RAISE(ABORT, 'memory_v2 operation receipts are immutable');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_operation_receipts_no_payload
        BEFORE INSERT ON memory_v2_operation_receipts
        WHEN EXISTS (
            SELECT 1 FROM json_tree(NEW.receipt_json)
            WHERE lower(CAST(key AS TEXT)) IN (
                'content', 'payload', 'payload_json', 'metadata',
                'vector', 'vectors', 'embedding', 'embeddings',
                'vector_watermark', 'vector_watermark_json'
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 operation receipts cannot retain payload data');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_only_redaction
        BEFORE UPDATE ON memory_v2_feedback_history
        WHEN NOT (
            NEW.owner_kind IS OLD.owner_kind
            AND NEW.project_id IS OLD.project_id
            AND NEW.fact_id IS OLD.fact_id
            AND NEW.event_id IS OLD.event_id
            AND NEW.action IS OLD.action
            AND NEW.old_trust IS OLD.old_trust
            AND NEW.new_trust IS OLD.new_trust
            AND NEW.occurred_at IS OLD.occurred_at
            AND NEW.source IS NULL
            AND NEW.note IS NULL
            AND (
                (OLD.details_availability = 'available'
                    AND NEW.details_availability = 'redacted')
                OR (
                    OLD.source IS NULL AND OLD.note IS NULL
                    AND NEW.details_availability IS OLD.details_availability
                )
            )
        ) BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history permits only detail redaction');
        END;
        CREATE TRIGGER IF NOT EXISTS memory_v2_feedback_history_no_delete
        BEFORE DELETE ON memory_v2_feedback_history BEGIN
            SELECT RAISE(ABORT, 'memory_v2 feedback history records are immutable');
        END;

CREATE TRIGGER IF NOT EXISTS memory_v2_automatic_fact_receipts_require_keys
     BEFORE INSERT ON memory_v2_automatic_fact_receipts
     WHEN NEW.idempotency_key IS NULL OR length(NEW.idempotency_key) = 0
       OR NEW.request_digest IS NULL OR length(NEW.request_digest) = 0
     BEGIN
         SELECT RAISE(ABORT, 'memory_v2 automatic fact receipts require idempotency and request digests');
     END;

CREATE INDEX IF NOT EXISTS idx_memory_v2_current_search
         ON memory_v2_current_facts(
             owner_kind, project_id, updated_at DESC, fact_id
         );

CREATE TABLE IF NOT EXISTS generation_diagnostics (
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

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_generation_state
        ON generation_diagnostics (generation_id, record_state);

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_generation_state_anchor
        ON generation_diagnostics (generation_id, record_state, diagnostic_anchor);

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_file
        ON generation_diagnostics (file_occurrence_id, generation_id);

    CREATE INDEX IF NOT EXISTS idx_generation_diagnostics_file_generation_state_anchor
        ON generation_diagnostics (
            file_occurrence_id, generation_id, record_state, diagnostic_anchor
        );

    CREATE TABLE IF NOT EXISTS diagnostic_generation_publications (
        generation_id TEXT PRIMARY KEY,
        record_state TEXT NOT NULL,
        state_generation TEXT,
        published_at INTEGER NOT NULL
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_diagnostic_generation_current
        ON diagnostic_generation_publications (record_state)
        WHERE record_state = 'current';

CREATE TABLE IF NOT EXISTS evidence_source_occurrences (
        occurrence_id TEXT PRIMARY KEY CHECK(length(occurrence_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        timeline_digest TEXT NOT NULL CHECK(length(timeline_digest) > 0),
        source_anchor_id TEXT NOT NULL CHECK(length(source_anchor_id) > 0),
        source_order INTEGER NOT NULL CHECK(source_order >= 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json))
    );
    CREATE INDEX IF NOT EXISTS idx_evidence_occurrences_anchor
        ON evidence_source_occurrences(owner_digest, source_anchor_id);
    CREATE INDEX IF NOT EXISTS idx_evidence_occurrences_timeline
        ON evidence_source_occurrences(owner_digest, timeline_digest, source_order);

    CREATE TABLE IF NOT EXISTS evidence_occurrence_sets (
        occurrence_set_id TEXT PRIMARY KEY CHECK(length(occurrence_set_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json))
    );
    CREATE TABLE IF NOT EXISTS evidence_occurrence_set_members (
        occurrence_set_id TEXT NOT NULL,
        canonical_ordinal INTEGER NOT NULL CHECK(canonical_ordinal >= 0),
        occurrence_id TEXT NOT NULL,
        PRIMARY KEY(occurrence_set_id, canonical_ordinal),
        UNIQUE(occurrence_set_id, occurrence_id),
        FOREIGN KEY(occurrence_set_id)
            REFERENCES evidence_occurrence_sets(occurrence_set_id),
        FOREIGN KEY(occurrence_id)
            REFERENCES evidence_source_occurrences(occurrence_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_spans (
        span_id TEXT PRIMARY KEY CHECK(length(span_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        occurrence_set_id TEXT NOT NULL,
        anchor_id TEXT NOT NULL UNIQUE CHECK(length(anchor_id) > 0),
        producer_kind TEXT NOT NULL CHECK(length(producer_kind) > 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        FOREIGN KEY(occurrence_set_id)
            REFERENCES evidence_occurrence_sets(occurrence_set_id)
    );
    CREATE TABLE IF NOT EXISTS evidence_span_members (
        span_id TEXT NOT NULL,
        assembly_ordinal INTEGER NOT NULL CHECK(assembly_ordinal >= 0),
        run_ordinal INTEGER NOT NULL CHECK(run_ordinal >= 0),
        run_member_ordinal INTEGER NOT NULL CHECK(run_member_ordinal >= 0),
        occurrence_id TEXT NOT NULL,
        PRIMARY KEY(span_id, assembly_ordinal),
        UNIQUE(span_id, occurrence_id),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id),
        FOREIGN KEY(occurrence_id)
            REFERENCES evidence_source_occurrences(occurrence_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_span_projection_receipts (
        projection_receipt_id TEXT PRIMARY KEY CHECK(length(projection_receipt_id) > 0),
        span_id TEXT NOT NULL,
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        UNIQUE(span_id, projection_receipt_id),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_retriever_contributions (
        contribution_id TEXT PRIMARY KEY CHECK(length(contribution_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        span_id TEXT NOT NULL,
        anchor_id TEXT NOT NULL UNIQUE CHECK(length(anchor_id) > 0),
        record_digest TEXT NOT NULL CHECK(length(record_digest) > 0),
        record_json TEXT NOT NULL CHECK(json_valid(record_json)),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_derived_anchors (
        anchor_id TEXT PRIMARY KEY CHECK(length(anchor_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        target_kind TEXT NOT NULL CHECK(
            target_kind IN ('source_occurrence', 'evidence_span', 'retriever_contribution')
        ),
        target_id TEXT NOT NULL CHECK(length(target_id) > 0),
        anchor_json TEXT NOT NULL CHECK(json_valid(anchor_json)),
        UNIQUE(owner_digest, target_kind, target_id)
    );

    CREATE TABLE IF NOT EXISTS evidence_assembly_receipts (
        publication_receipt_id TEXT PRIMARY KEY CHECK(length(publication_receipt_id) > 0),
        owner_digest TEXT NOT NULL CHECK(length(owner_digest) > 0),
        privacy_domain_id TEXT NOT NULL CHECK(length(privacy_domain_id) > 0),
        key_epoch INTEGER NOT NULL CHECK(key_epoch > 0),
        idempotency_key TEXT NOT NULL CHECK(length(idempotency_key) > 0),
        assembly_digest TEXT NOT NULL CHECK(length(assembly_digest) > 0),
        occurrence_set_id TEXT NOT NULL,
        span_id TEXT NOT NULL,
        contribution_id TEXT NOT NULL,
        projection_receipt_id TEXT NOT NULL,
        receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
        UNIQUE(owner_digest, privacy_domain_id, key_epoch, idempotency_key),
        FOREIGN KEY(occurrence_set_id)
            REFERENCES evidence_occurrence_sets(occurrence_set_id),
        FOREIGN KEY(span_id) REFERENCES evidence_spans(span_id),
        FOREIGN KEY(contribution_id)
            REFERENCES evidence_retriever_contributions(contribution_id),
        FOREIGN KEY(projection_receipt_id)
            REFERENCES evidence_span_projection_receipts(projection_receipt_id)
    );

CREATE TRIGGER IF NOT EXISTS evidence_source_occurrences_immutable_update
    BEFORE UPDATE ON evidence_source_occurrences BEGIN
        SELECT RAISE(ABORT, 'evidence source occurrences are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_source_occurrences_immutable_delete
    BEFORE DELETE ON evidence_source_occurrences BEGIN
        SELECT RAISE(ABORT, 'evidence source occurrences are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_sets_immutable_update
    BEFORE UPDATE ON evidence_occurrence_sets BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence sets are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_sets_immutable_delete
    BEFORE DELETE ON evidence_occurrence_sets BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence sets are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_set_members_immutable_update
    BEFORE UPDATE ON evidence_occurrence_set_members BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence set membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_occurrence_set_members_immutable_delete
    BEFORE DELETE ON evidence_occurrence_set_members BEGIN
        SELECT RAISE(ABORT, 'evidence occurrence set membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_spans_immutable_update
    BEFORE UPDATE ON evidence_spans BEGIN
        SELECT RAISE(ABORT, 'evidence spans are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_spans_immutable_delete
    BEFORE DELETE ON evidence_spans BEGIN
        SELECT RAISE(ABORT, 'evidence spans are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_members_immutable_update
    BEFORE UPDATE ON evidence_span_members BEGIN
        SELECT RAISE(ABORT, 'evidence span membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_members_immutable_delete
    BEFORE DELETE ON evidence_span_members BEGIN
        SELECT RAISE(ABORT, 'evidence span membership is immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_projection_receipts_immutable_update
    BEFORE UPDATE ON evidence_span_projection_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence projection receipts are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_span_projection_receipts_immutable_delete
    BEFORE DELETE ON evidence_span_projection_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence projection receipts are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_retriever_contributions_immutable_update
    BEFORE UPDATE ON evidence_retriever_contributions BEGIN
        SELECT RAISE(ABORT, 'retriever contributions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_retriever_contributions_immutable_delete
    BEFORE DELETE ON evidence_retriever_contributions BEGIN
        SELECT RAISE(ABORT, 'retriever contributions are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_derived_anchors_immutable_update
    BEFORE UPDATE ON evidence_derived_anchors BEGIN
        SELECT RAISE(ABORT, 'evidence derived anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_derived_anchors_immutable_delete
    BEFORE DELETE ON evidence_derived_anchors BEGIN
        SELECT RAISE(ABORT, 'evidence derived anchors are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_assembly_receipts_immutable_update
    BEFORE UPDATE ON evidence_assembly_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence assembly receipts are immutable');
    END;
    CREATE TRIGGER IF NOT EXISTS evidence_assembly_receipts_immutable_delete
    BEFORE DELETE ON evidence_assembly_receipts BEGIN
        SELECT RAISE(ABORT, 'evidence assembly receipts are immutable');
    END;

CREATE TABLE IF NOT EXISTS external_source_states_v1 (
    binding_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('project', 'profile')),
    owner_id TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    definition_digest TEXT NOT NULL,
    binding_revision INTEGER NOT NULL CHECK (binding_revision > 0),
    binding_digest TEXT NOT NULL,
    source_frontier_digest TEXT NOT NULL,
    source_frontier_json TEXT NOT NULL,
    projection_frontier_digest TEXT,
    latest_source_receipt_digest TEXT NOT NULL,
    latest_projection_receipt_digest TEXT
);
CREATE INDEX IF NOT EXISTS idx_external_source_states_owner_v1
    ON external_source_states_v1(owner_kind, owner_id, source_id);
CREATE TABLE IF NOT EXISTS external_source_definition_revisions_v1 (
    source_id TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    definition_digest TEXT NOT NULL,
    definition_json TEXT NOT NULL,
    PRIMARY KEY (source_id, definition_revision)
);
CREATE TABLE IF NOT EXISTS external_source_binding_revisions_v1 (
    binding_id TEXT NOT NULL,
    binding_revision INTEGER NOT NULL CHECK (binding_revision > 0),
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    binding_digest TEXT NOT NULL,
    binding_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, binding_revision)
);
CREATE TABLE IF NOT EXISTS external_source_authority_receipts_v1 (
    binding_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    definition_digest TEXT NOT NULL,
    binding_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, idempotency_key)
);
CREATE TABLE IF NOT EXISTS external_source_commit_receipts_v1 (
    binding_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    definition_revision INTEGER NOT NULL CHECK (definition_revision > 0),
    binding_revision INTEGER NOT NULL CHECK (binding_revision > 0),
    predecessor_frontier_digest TEXT NOT NULL,
    successor_frontier_digest TEXT NOT NULL,
    receipt_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, idempotency_key),
    UNIQUE (binding_id, receipt_digest),
    UNIQUE (binding_id, successor_frontier_digest)
);
CREATE TABLE IF NOT EXISTS external_source_mutations_v1 (
    binding_id TEXT NOT NULL,
    mutation_digest TEXT NOT NULL,
    native_object_digest TEXT NOT NULL,
    revision_digest TEXT NOT NULL,
    source_receipt_digest TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, mutation_digest),
    UNIQUE (binding_id, native_object_digest, revision_digest)
);
CREATE TABLE IF NOT EXISTS external_source_lineage_v1 (
    binding_id TEXT NOT NULL,
    lineage_digest TEXT NOT NULL,
    source_receipt_digest TEXT NOT NULL,
    lineage_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, lineage_digest)
);
CREATE TABLE IF NOT EXISTS external_source_objects_v1 (
    binding_id TEXT NOT NULL,
    native_object_digest TEXT NOT NULL,
    partition_digest TEXT NOT NULL,
    mutation_digest TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, native_object_digest)
);
CREATE TABLE IF NOT EXISTS external_source_pending_projections_v1 (
    binding_id TEXT NOT NULL,
    predecessor_frontier_digest TEXT NOT NULL,
    successor_frontier_digest TEXT NOT NULL,
    successor_sequence INTEGER NOT NULL CHECK (successor_sequence > 0),
    source_receipt_digest TEXT NOT NULL,
    PRIMARY KEY (binding_id, predecessor_frontier_digest),
    UNIQUE (binding_id, successor_frontier_digest),
    UNIQUE (binding_id, source_receipt_digest)
);
CREATE TABLE IF NOT EXISTS external_source_projection_publications_v1 (
    binding_id TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    source_receipt_digest TEXT NOT NULL,
    predecessor_frontier_digest TEXT NOT NULL,
    successor_frontier_digest TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, projection_digest),
    UNIQUE (binding_id, source_receipt_digest),
    UNIQUE (binding_id, successor_frontier_digest)
);
CREATE TABLE IF NOT EXISTS external_source_projection_effects_v1 (
    binding_id TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    effect_index INTEGER NOT NULL CHECK (effect_index >= 0),
    native_object_digest TEXT NOT NULL,
    effect_json TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, projection_digest, effect_index)
);
CREATE TABLE IF NOT EXISTS external_source_projection_lineage_v1 (
    binding_id TEXT NOT NULL,
    projection_digest TEXT NOT NULL,
    lineage_index INTEGER NOT NULL CHECK (lineage_index >= 0),
    lineage_digest TEXT NOT NULL,
    lineage_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, projection_digest, lineage_index)
);
CREATE TABLE IF NOT EXISTS external_source_projected_objects_v1 (
    binding_id TEXT NOT NULL,
    native_object_digest TEXT NOT NULL,
    mutation_json TEXT NOT NULL,
    PRIMARY KEY (binding_id, native_object_digest)
);
CREATE TABLE IF NOT EXISTS external_source_acquisition_queue_v1 (
    binding_id TEXT PRIMARY KEY,
    state_digest TEXT NOT NULL,
    not_before_micros INTEGER,
    state_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_external_source_acquisition_ready_v1
    ON external_source_acquisition_queue_v1(not_before_micros, binding_id)
    WHERE not_before_micros IS NOT NULL;

CREATE TABLE IF NOT EXISTS graph_publication_replay_v1 (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    generation TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    dependency_generation_closure_digest TEXT NOT NULL,
    direct_dependency_bytes INTEGER NOT NULL
        CHECK (direct_dependency_bytes >= 2
            AND direct_dependency_bytes <= 1048576),
    expected_prior_head TEXT,
    expected_recovered_digest TEXT NOT NULL,
    canonical_replay_source_digest TEXT NOT NULL,
    canonical_replay_source BLOB NOT NULL
        CHECK (length(canonical_replay_source) > 0
            AND length(canonical_replay_source) <= 4194304
            AND length(canonical_replay_source)
                + direct_dependency_bytes <= 4194304),
    UNIQUE (shard_id, namespace, projection, generation),
    UNIQUE (shard_id, namespace, projection, idempotency_key),
    UNIQUE (sequence, shard_id, namespace, projection, generation)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_graph_publication_replay_projection_sequence
    ON graph_publication_replay_v1(shard_id, namespace, projection, sequence);

CREATE TABLE IF NOT EXISTS graph_publication_replay_dependencies_v1 (
    owner_replay_sequence INTEGER NOT NULL
        REFERENCES graph_publication_replay_v1(sequence) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    dependency_replay_sequence INTEGER NOT NULL,
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    generation TEXT NOT NULL,
    PRIMARY KEY (owner_replay_sequence, ordinal),
    UNIQUE (owner_replay_sequence, shard_id, namespace, projection),
    FOREIGN KEY (
        dependency_replay_sequence, shard_id, namespace, projection, generation
    ) REFERENCES graph_publication_replay_v1(
        sequence, shard_id, namespace, projection, generation
    ) ON DELETE RESTRICT
) STRICT;

CREATE INDEX IF NOT EXISTS idx_graph_publication_dependency_replay
    ON graph_publication_replay_dependencies_v1(dependency_replay_sequence);

CREATE TABLE IF NOT EXISTS graph_publication_replay_tombstones_v1 (
    replay_sequence INTEGER PRIMARY KEY,
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    generation TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    dependency_generation_closure_digest TEXT NOT NULL,
    direct_dependency_bytes INTEGER NOT NULL
        CHECK (direct_dependency_bytes >= 2
            AND direct_dependency_bytes <= 1048576),
    expected_prior_head TEXT,
    expected_recovered_digest TEXT NOT NULL,
    canonical_replay_source_digest TEXT NOT NULL,
    UNIQUE (shard_id, namespace, projection, generation),
    UNIQUE (shard_id, namespace, projection, idempotency_key)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_graph_publication_tombstone_projection
    ON graph_publication_replay_tombstones_v1(shard_id, namespace, projection);

CREATE TABLE IF NOT EXISTS graph_publication_replay_tombstone_dependencies_v1 (
    tombstone_replay_sequence INTEGER NOT NULL
        REFERENCES graph_publication_replay_tombstones_v1(replay_sequence)
        ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    generation TEXT NOT NULL,
    PRIMARY KEY (tombstone_replay_sequence, ordinal),
    UNIQUE (tombstone_replay_sequence, shard_id, namespace, projection)
) STRICT;

CREATE TABLE IF NOT EXISTS graph_verified_heads_v1 (
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    replay_sequence INTEGER NOT NULL UNIQUE
        REFERENCES graph_publication_replay_v1(sequence) ON DELETE RESTRICT,
    recovered_digest TEXT NOT NULL,
    PRIMARY KEY (shard_id, namespace, projection)
) STRICT;

CREATE TABLE IF NOT EXISTS semantic_vector_stages (
    stage_id INTEGER PRIMARY KEY AUTOINCREMENT,
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    build_id TEXT NOT NULL,
    plan_digest TEXT NOT NULL,
    semantic_generation_id TEXT NOT NULL,
    base_generation TEXT,
    publication_generation TEXT NOT NULL,
    publication_idempotency_key TEXT NOT NULL,
    source_scope TEXT NOT NULL,
    source_generation TEXT NOT NULL,
    source_dependency TEXT NOT NULL CHECK (json_valid(source_dependency)),
    source_manifest_digest TEXT NOT NULL,
    embedding_projection_digest TEXT NOT NULL,
    embedding_dimension INTEGER NOT NULL
        CHECK (embedding_dimension > 0 AND embedding_dimension <= 4096),
    model_artifact_digest TEXT NOT NULL,
    projection_manifest_digest TEXT NOT NULL,
    privacy_domain_digest TEXT NOT NULL,
    privacy_key_epoch INTEGER NOT NULL CHECK (privacy_key_epoch > 0),
    expected_chunk_manifest_digest TEXT NOT NULL,
    expected_chunk_count INTEGER NOT NULL
        CHECK (expected_chunk_count >= 0 AND expected_chunk_count <= 100000),
    expected_prior_verified_head TEXT,
    writer_binding TEXT NOT NULL CHECK (json_valid(writer_binding)),
    code_scope_hash TEXT NOT NULL
        CHECK (length(code_scope_hash) = 64
            AND code_scope_hash NOT GLOB '*[^0-9a-f]*'),
    plan_json TEXT NOT NULL CHECK (json_valid(plan_json)),
    state TEXT NOT NULL CHECK (state IN ('pending', 'ready_to_publish', 'published', 'cancelled')),
    next_ordinal INTEGER NOT NULL CHECK (next_ordinal >= 0),
    checkpoint_digest TEXT NOT NULL,
    recorded_chunk_count INTEGER NOT NULL
        CHECK (recorded_chunk_count >= 0
            AND recorded_chunk_count <= expected_chunk_count),
    applied_ordinal INTEGER CHECK (applied_ordinal >= 0),
    applied_receipt_digest TEXT,
    applied_checkpoint_digest TEXT,
    applied_graph_batch_digest TEXT,
    expected_recovered_digest TEXT,
    publication_intent_digest TEXT,
    CHECK (
        (applied_ordinal IS NULL
            AND applied_receipt_digest IS NULL
            AND applied_checkpoint_digest IS NULL
            AND applied_graph_batch_digest IS NULL)
        OR
        (applied_ordinal IS NOT NULL
            AND applied_receipt_digest IS NOT NULL
            AND applied_checkpoint_digest IS NOT NULL
            AND applied_graph_batch_digest IS NOT NULL)
    ),
    CHECK (
        (state IN ('ready_to_publish', 'published')
            AND expected_recovered_digest IS NOT NULL
            AND publication_intent_digest IS NOT NULL)
        OR
        (state NOT IN ('ready_to_publish', 'published')
            AND expected_recovered_digest IS NULL
            AND publication_intent_digest IS NULL)
    ),
    UNIQUE (shard_id, namespace, projection, build_id),
    UNIQUE (shard_id, namespace, projection, plan_digest)
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_vector_one_pending_stage
    ON semantic_vector_stages(shard_id, namespace, projection)
    WHERE state IN ('pending', 'ready_to_publish');

-- Cancelled attempts stay durable for audit but release their publication
-- identity so the same semantic generation can be rebuilt under a new plan.
CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_vector_live_semantic_generation
    ON semantic_vector_stages(shard_id, namespace, projection, semantic_generation_id)
    WHERE state != 'cancelled';

CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_vector_live_publication_generation
    ON semantic_vector_stages(shard_id, namespace, projection, publication_generation)
    WHERE state != 'cancelled';

CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_vector_live_publication_idempotency
    ON semantic_vector_stages(shard_id, namespace, projection, publication_idempotency_key)
    WHERE state != 'cancelled';

CREATE INDEX IF NOT EXISTS idx_semantic_vector_live_base_generation
    ON semantic_vector_stages(shard_id, base_generation)
    WHERE state IN ('pending', 'ready_to_publish', 'published')
      AND base_generation IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_semantic_vector_live_source_generation
    ON semantic_vector_stages(shard_id, source_generation)
    WHERE state IN ('pending', 'ready_to_publish', 'published');

CREATE INDEX IF NOT EXISTS idx_semantic_vector_live_source_scope
    ON semantic_vector_stages(shard_id, source_scope)
      WHERE state IN ('pending', 'ready_to_publish', 'published');

CREATE INDEX IF NOT EXISTS idx_semantic_vector_code_scope_binding
    ON semantic_vector_stages(shard_id, code_scope_hash, source_scope)
    WHERE state IN ('pending', 'ready_to_publish', 'published');

CREATE INDEX IF NOT EXISTS idx_semantic_vector_published_project_generation
    ON semantic_vector_stages(shard_id, semantic_generation_id)
    WHERE state = 'published';

CREATE INDEX IF NOT EXISTS idx_semantic_vector_project_census
    ON semantic_vector_stages(shard_id, stage_id);

CREATE INDEX IF NOT EXISTS idx_semantic_vector_projection_census
    ON semantic_vector_stages(shard_id, namespace, projection, stage_id);

CREATE TABLE IF NOT EXISTS semantic_vector_stage_census_authority (
    shard_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT;

CREATE TABLE IF NOT EXISTS semantic_vector_stage_adoption_authority (
    shard_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL CHECK (revision > 0)
) STRICT;

CREATE TABLE IF NOT EXISTS semantic_vector_source_scope_bindings (
    shard_id TEXT NOT NULL,
    code_scope_hash TEXT NOT NULL
        CHECK (length(code_scope_hash) = 64
            AND code_scope_hash NOT GLOB '*[^0-9a-f]*'),
    source_scope TEXT NOT NULL CHECK (json_valid(source_scope)),
    PRIMARY KEY (shard_id, code_scope_hash),
    UNIQUE (shard_id, source_scope)
) WITHOUT ROWID, STRICT;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_scope_binding_insert
AFTER INSERT ON semantic_vector_source_scope_bindings
BEGIN
    INSERT INTO semantic_vector_stage_census_authority(shard_id,revision)
    VALUES(NEW.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_scope_binding_delete
AFTER DELETE ON semantic_vector_source_scope_bindings
BEGIN
    INSERT INTO semantic_vector_stage_census_authority(shard_id,revision)
    VALUES(OLD.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_source_scope_binding_immutable
BEFORE UPDATE ON semantic_vector_source_scope_bindings
BEGIN
    SELECT RAISE(ABORT, 'semantic vector source-scope binding is immutable');
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_stage_insert
AFTER INSERT ON semantic_vector_stages
BEGIN
    INSERT INTO semantic_vector_stage_census_authority(shard_id,revision)
    VALUES(NEW.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
    INSERT INTO semantic_vector_stage_adoption_authority(shard_id,revision)
    VALUES(NEW.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_stage_update
AFTER UPDATE ON semantic_vector_stages
BEGIN
    INSERT INTO semantic_vector_stage_census_authority(shard_id,revision)
    VALUES(NEW.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_adoption_after_state_update
AFTER UPDATE OF state ON semantic_vector_stages
WHEN OLD.state != NEW.state
BEGIN
    INSERT INTO semantic_vector_stage_adoption_authority(shard_id,revision)
    VALUES(NEW.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_stage_delete
AFTER DELETE ON semantic_vector_stages
BEGIN
    INSERT INTO semantic_vector_stage_census_authority(shard_id,revision)
    VALUES(OLD.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
    INSERT INTO semantic_vector_stage_adoption_authority(shard_id,revision)
    VALUES(OLD.shard_id,1)
    ON CONFLICT(shard_id) DO UPDATE SET revision=revision+1;
END;

CREATE TABLE IF NOT EXISTS semantic_vector_retirement_cleanup (
    cleanup_id INTEGER PRIMARY KEY AUTOINCREMENT,
    shard_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    projection TEXT NOT NULL,
    semantic_generation_id TEXT NOT NULL,
    publication_generation TEXT NOT NULL,
    publication_idempotency_key TEXT NOT NULL,
    retirement_json TEXT NOT NULL CHECK (json_valid(retirement_json)),
    UNIQUE (shard_id, namespace, projection, semantic_generation_id),
    UNIQUE (shard_id, namespace, projection, publication_generation),
    UNIQUE (shard_id, namespace, projection, publication_idempotency_key)
) STRICT;

CREATE INDEX IF NOT EXISTS idx_semantic_vector_pending_retirement_cleanup
    ON semantic_vector_retirement_cleanup(shard_id, cleanup_id);

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_publication_identity_guard
BEFORE INSERT ON semantic_vector_stages
WHEN EXISTS (
    SELECT 1 FROM graph_publication_replay_v1
    WHERE shard_id=NEW.shard_id
      AND namespace=NEW.namespace
      AND projection=NEW.projection
      AND (
          generation=NEW.publication_generation
          OR idempotency_key=NEW.publication_idempotency_key
      )
    UNION ALL
    SELECT 1 FROM graph_publication_replay_tombstones_v1
    WHERE shard_id=NEW.shard_id
      AND namespace=NEW.namespace
      AND projection=NEW.projection
      AND (
          generation=NEW.publication_generation
          OR idempotency_key=NEW.publication_idempotency_key
      )
)
BEGIN
    SELECT RAISE(ABORT, 'semantic vector publication identity is already retained');
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_replay_stage_identity_guard
BEFORE INSERT ON graph_publication_replay_v1
WHEN EXISTS (
    SELECT 1 FROM semantic_vector_stages
    WHERE shard_id=NEW.shard_id
      AND namespace=NEW.namespace
      AND projection=NEW.projection
      AND (
          publication_generation=NEW.generation
          OR publication_idempotency_key=NEW.idempotency_key
      )
      AND NOT (
          state='ready_to_publish'
          AND
          publication_generation=NEW.generation
          AND publication_idempotency_key=NEW.idempotency_key
      )
)
BEGIN
    SELECT RAISE(ABORT, 'graph replay conflicts with a semantic vector publication identity');
END;

CREATE TABLE IF NOT EXISTS semantic_vector_stage_batches (
    batch_id INTEGER PRIMARY KEY AUTOINCREMENT,
    stage_id INTEGER NOT NULL
        REFERENCES semantic_vector_stages(stage_id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    expected_checkpoint_digest TEXT NOT NULL,
    input_digest TEXT NOT NULL,
    output_digest TEXT NOT NULL,
    receipt_digest TEXT NOT NULL,
    checkpoint_digest TEXT NOT NULL,
    chunk_count INTEGER NOT NULL CHECK (chunk_count >= 0 AND chunk_count <= 512),
    receipt_json TEXT NOT NULL CHECK (json_valid(receipt_json)),
    UNIQUE (stage_id, ordinal),
    UNIQUE (stage_id, receipt_digest)
) STRICT;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_batch_insert
AFTER INSERT ON semantic_vector_stage_batches
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT shard_id FROM semantic_vector_stages WHERE stage_id=NEW.stage_id
    );
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_batch_update
AFTER UPDATE ON semantic_vector_stage_batches
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT shard_id FROM semantic_vector_stages WHERE stage_id=NEW.stage_id
    );
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_batch_delete
AFTER DELETE ON semantic_vector_stage_batches
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT shard_id FROM semantic_vector_stages WHERE stage_id=OLD.stage_id
    );
END;

CREATE TABLE IF NOT EXISTS semantic_vector_stage_chunk_receipts (
    stage_id INTEGER NOT NULL
        REFERENCES semantic_vector_stages(stage_id) ON DELETE RESTRICT,
    batch_id INTEGER NOT NULL
        REFERENCES semantic_vector_stage_batches(batch_id) ON DELETE RESTRICT,
    effect_ordinal INTEGER NOT NULL CHECK (effect_ordinal >= 0),
    chunk_id TEXT NOT NULL,
    chunk_digest TEXT NOT NULL,
    operation TEXT NOT NULL CHECK (operation IN ('embed', 'reuse', 'tombstone')),
    output_digest TEXT,
    CHECK (
        (operation = 'embed' AND output_digest IS NOT NULL)
        OR (operation IN ('reuse', 'tombstone') AND output_digest IS NULL)
    ),
    PRIMARY KEY (batch_id, effect_ordinal),
    UNIQUE (stage_id, chunk_id)
) WITHOUT ROWID, STRICT;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_chunk_insert
AFTER INSERT ON semantic_vector_stage_chunk_receipts
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT shard_id FROM semantic_vector_stages WHERE stage_id=NEW.stage_id
    );
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_chunk_update
AFTER UPDATE ON semantic_vector_stage_chunk_receipts
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT shard_id FROM semantic_vector_stages WHERE stage_id=NEW.stage_id
    );
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_chunk_delete
AFTER DELETE ON semantic_vector_stage_chunk_receipts
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT shard_id FROM semantic_vector_stages WHERE stage_id=OLD.stage_id
    );
END;

CREATE TABLE IF NOT EXISTS semantic_vector_stage_graph_effects (
    outbox_sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    batch_id INTEGER NOT NULL UNIQUE
        REFERENCES semantic_vector_stage_batches(batch_id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (state IN ('pending', 'applied', 'failed', 'cancelled')),
    terminal_digest TEXT,
    CHECK (
        (state = 'pending' AND terminal_digest IS NULL)
        OR (state = 'cancelled' AND terminal_digest IS NULL)
        OR (state IN ('applied', 'failed') AND terminal_digest IS NOT NULL)
    )
) STRICT;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_effect_insert
AFTER INSERT ON semantic_vector_stage_graph_effects
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT s.shard_id
        FROM semantic_vector_stage_batches b
        JOIN semantic_vector_stages s ON s.stage_id=b.stage_id
        WHERE b.batch_id=NEW.batch_id
    );
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_effect_update
AFTER UPDATE ON semantic_vector_stage_graph_effects
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT s.shard_id
        FROM semantic_vector_stage_batches b
        JOIN semantic_vector_stages s ON s.stage_id=b.stage_id
        WHERE b.batch_id=NEW.batch_id
    );
END;

CREATE TRIGGER IF NOT EXISTS semantic_vector_stage_census_after_effect_delete
AFTER DELETE ON semantic_vector_stage_graph_effects
BEGIN
    UPDATE semantic_vector_stage_census_authority
    SET revision=revision+1
    WHERE shard_id=(
        SELECT s.shard_id
        FROM semantic_vector_stage_batches b
        JOIN semantic_vector_stages s ON s.stage_id=b.stage_id
        WHERE b.batch_id=OLD.batch_id
    );
END;

CREATE INDEX IF NOT EXISTS idx_semantic_vector_pending_effects
    ON semantic_vector_stage_graph_effects(state, outbox_sequence);

CREATE TABLE IF NOT EXISTS handoff_open_grants_v1 (
    token_digest TEXT NOT NULL PRIMARY KEY,
    issued_request_id TEXT NOT NULL UNIQUE,
    grant_payload TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL CHECK (expires_at > issued_at),
    consumed_request_id TEXT,
    consumed_input_digest TEXT,
    consumption_payload TEXT,
    CHECK (
        (consumed_request_id IS NULL
         AND consumed_input_digest IS NULL
         AND consumption_payload IS NULL)
        OR
        (consumed_request_id IS NOT NULL
         AND consumed_input_digest IS NOT NULL
         AND consumption_payload IS NOT NULL)
    )
) STRICT;

CREATE TABLE IF NOT EXISTS td_runtime_writer_checkpoint_v1 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    commit_sequence INTEGER NOT NULL CHECK (commit_sequence > 0),
    watermark_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    original_receipt_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS td_runtime_writer_idempotency_v1 (
    shard_json TEXT NOT NULL,
    incarnation INTEGER NOT NULL CHECK (incarnation > 0),
    authority_epoch INTEGER NOT NULL CHECK (authority_epoch > 0),
    idempotency_key TEXT NOT NULL,
    request_digest TEXT NOT NULL,
    original_receipt_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (shard_json, incarnation, authority_epoch, idempotency_key)
) WITHOUT ROWID;

CREATE TABLE IF NOT EXISTS td_runtime_writer_outbox_v1 (
    source_shard_json TEXT NOT NULL,
    source_incarnation INTEGER NOT NULL CHECK (source_incarnation > 0),
    source_authority_epoch INTEGER NOT NULL CHECK (source_authority_epoch > 0),
    effect_id TEXT NOT NULL,
    ordering_key TEXT NOT NULL,
    source_sequence INTEGER NOT NULL CHECK (source_sequence >= 0),
    state TEXT NOT NULL CHECK (
        state IN ('pending', 'dispatched', 'effect_unknown', 'acknowledged')
    ),
    entry_json TEXT NOT NULL,
    source_receipt_json TEXT NOT NULL,
    transaction_scope_json TEXT NOT NULL,
    operation_id TEXT NOT NULL,
    durability_json TEXT NOT NULL,
    updated_at_micros INTEGER NOT NULL,
    PRIMARY KEY (source_shard_json, source_incarnation, source_authority_epoch, effect_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS td_runtime_writer_outbox_ordering_v1
ON td_runtime_writer_outbox_v1 (
    source_shard_json,
    source_incarnation,
    source_authority_epoch,
    ordering_key,
    source_sequence,
    effect_id
);

CREATE UNIQUE INDEX IF NOT EXISTS td_runtime_writer_outbox_effect_v1
ON td_runtime_writer_outbox_v1 (source_shard_json, effect_id);

CREATE INDEX IF NOT EXISTS td_runtime_writer_outbox_state_v1
ON td_runtime_writer_outbox_v1 (
    source_shard_json,
    source_incarnation,
    source_authority_epoch,
    state,
    updated_at_micros
);

CREATE TABLE IF NOT EXISTS td_runtime_writer_inbox_v1 (
    target_shard_json TEXT NOT NULL,
    target_incarnation INTEGER NOT NULL CHECK (target_incarnation > 0),
    target_authority_epoch INTEGER NOT NULL CHECK (target_authority_epoch > 0),
    effect_id TEXT NOT NULL,
    ordering_key TEXT NOT NULL,
    source_sequence INTEGER NOT NULL CHECK (source_sequence >= 0),
    target_sequence INTEGER NOT NULL CHECK (target_sequence > 0),
    identity_json TEXT NOT NULL,
    receipt_json TEXT NOT NULL,
    committed_at_micros INTEGER NOT NULL,
    PRIMARY KEY (target_shard_json, target_incarnation, target_authority_epoch, effect_id)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS td_runtime_writer_inbox_ordering_v1
ON td_runtime_writer_inbox_v1 (
    target_shard_json,
    target_incarnation,
    target_authority_epoch,
    ordering_key,
    source_sequence,
    effect_id
);

CREATE UNIQUE INDEX IF NOT EXISTS td_runtime_writer_inbox_effect_v1
ON td_runtime_writer_inbox_v1 (target_shard_json, effect_id);

