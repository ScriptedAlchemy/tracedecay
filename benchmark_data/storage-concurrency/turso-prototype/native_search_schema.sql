CREATE TABLE memory_v2_assertions(assertion_id TEXT,fact_id TEXT,owner_kind TEXT,project_id TEXT,PRIMARY KEY(assertion_id,fact_id,owner_kind,project_id));
CREATE TABLE sessions(provider TEXT,session_id TEXT,PRIMARY KEY(provider,session_id));
CREATE TABLE session_temporal_generations(session_id TEXT,generation INTEGER,PRIMARY KEY(session_id,generation));
CREATE TABLE observations(observation_id TEXT PRIMARY KEY);
CREATE TABLE retrieval_anchors(anchor_id TEXT PRIMARY KEY);

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

CREATE TABLE IF NOT EXISTS lcm_raw_messages (
            provider TEXT NOT NULL,
            message_id TEXT NOT NULL,
            session_id TEXT NOT NULL,
            store_id INTEGER PRIMARY KEY AUTOINCREMENT,
            role TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            timestamp INTEGER,
            content TEXT,
            content_hash TEXT NOT NULL,
            storage_kind TEXT NOT NULL CHECK(storage_kind IN ('inline', 'external')),
            payload_ref TEXT,
            placeholder_text TEXT,
            snippet_text TEXT NOT NULL GENERATED ALWAYS AS (
                CASE
                    WHEN content IS NULL THEN COALESCE(placeholder_text, '')
                    WHEN length(content) <= 4096 THEN content
                    ELSE substr(content, 1, 4054)
                        || char(10) || '[derived snippet truncated by tracedecay]'
                END
            ) VIRTUAL,
            index_text TEXT NOT NULL GENERATED ALWAYS AS (
                CASE
                    WHEN content IS NULL THEN COALESCE(placeholder_text, '')
                    WHEN length(content) <= 65536 THEN content
                    ELSE substr(content, 1, 65494)
                        || char(10) || '[derived snippet truncated by tracedecay]'
                END
            ) VIRTUAL,
            metadata_json TEXT,
            kind TEXT,
            model TEXT,
            tool_names TEXT,
            source_path TEXT,
            source_offset INTEGER,
            UNIQUE(provider, message_id),
            FOREIGN KEY(provider, session_id)
                REFERENCES sessions(provider, session_id) ON DELETE CASCADE
        );

CREATE TABLE IF NOT EXISTS session_occurrences (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL,
        occurrence_id TEXT NOT NULL,
        source_observation_id TEXT NOT NULL,
        source_sequence INTEGER NOT NULL CHECK(source_sequence > 0),
        source_provider TEXT NOT NULL CHECK(
            source_provider <> ''
            AND length(source_provider) <= 512
            AND source_provider = trim(source_provider)
        ),
        projection_output_ordinal INTEGER NOT NULL CHECK(projection_output_ordinal >= 0),
        retrieval_anchor_id TEXT NOT NULL,
        thread_id TEXT,
        thread_grouping_json TEXT CHECK(thread_grouping_json IS NULL OR json_valid(thread_grouping_json)),
        turn_id TEXT,
        turn_grouping_json TEXT CHECK(turn_grouping_json IS NULL OR json_valid(turn_grouping_json)),
        message_id TEXT,
        agent_id TEXT,
        parent_message_id TEXT,
        parent_agent_id TEXT,
        parent_session_id TEXT,
        copied_from_anchor_ids_json TEXT NOT NULL CHECK(
            json_valid(copied_from_anchor_ids_json)
            AND json_type(copied_from_anchor_ids_json) = 'array'
        ),
        role TEXT NOT NULL,
        knowledge_at INTEGER NOT NULL,
        valid_time_json TEXT NOT NULL CHECK(
            json_valid(valid_time_json)
            AND json_type(valid_time_json, '$.kind') IS 'text'
            AND (
                (
                    json_extract(valid_time_json, '$.kind') = 'unknown'
                    AND json_type(valid_time_json, '$.valid_at') IS NULL
                )
                OR (
                    json_extract(valid_time_json, '$.kind') = 'known'
                    AND json_type(valid_time_json, '$.valid_at') IS 'integer'
                )
            )
        ),
        evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
        sanitized_content_digest TEXT NOT NULL CHECK(
            length(sanitized_content_digest) = 64
            AND sanitized_content_digest NOT GLOB '*[^0-9a-f]*'
        ),
        sanitized_content_bytes INTEGER NOT NULL CHECK(sanitized_content_bytes >= 0),
        snippet_text TEXT NOT NULL GENERATED ALWAYS AS (index_text) VIRTUAL,
        index_text TEXT NOT NULL,
        PRIMARY KEY(session_id, occurrence_id),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE,
        FOREIGN KEY(source_observation_id) REFERENCES observations(observation_id),
        FOREIGN KEY(retrieval_anchor_id) REFERENCES retrieval_anchors(anchor_id)
    );

CREATE TABLE IF NOT EXISTS session_summary_nodes (
        summary_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        provider TEXT NOT NULL,
        conversation_id TEXT NOT NULL,
        depth INTEGER NOT NULL,
        summary_anchor_id TEXT NOT NULL,
        summary_text TEXT NOT NULL,
        summary_hash TEXT NOT NULL,
        summary_token_count INTEGER NOT NULL,
        source_token_count INTEGER NOT NULL,
        source_time_start INTEGER,
        source_time_end INTEGER,
        expand_hint TEXT,
        metadata_json TEXT,
        source_horizon_json TEXT NOT NULL CHECK(json_valid(source_horizon_json)),
        publication_json TEXT CHECK(publication_json IS NULL OR json_valid(publication_json)),
        created_at INTEGER NOT NULL,
        FOREIGN KEY(summary_anchor_id) REFERENCES retrieval_anchors(anchor_id)
    );

CREATE INDEX IF NOT EXISTS memory_v2_assertion_payloads_fts ON memory_v2_assertion_payloads USING fts (content);
CREATE INDEX IF NOT EXISTS lcm_raw_messages_fts ON lcm_raw_messages USING fts (index_text, role, kind, model, tool_names) WITH (weights = 'index_text=10.0,role=2.0,kind=1.0,model=1.0,tool_names=1.0');
CREATE INDEX IF NOT EXISTS session_occurrences_fts ON session_occurrences USING fts (index_text);
CREATE INDEX IF NOT EXISTS session_summary_nodes_fts ON session_summary_nodes USING fts (summary_text);
CREATE TRIGGER memory_v2_payloads_no_update BEFORE UPDATE ON memory_v2_assertion_payloads BEGIN SELECT RAISE(ABORT, 'memory_v2 assertion payloads are immutable'); END;
