
    CREATE TABLE session_relation_receipts (
        session_id TEXT NOT NULL,
        generation INTEGER NOT NULL CHECK(generation > 0),
        scope_kind TEXT NOT NULL
            CHECK(scope_kind IN ('project_sessions', 'profile_sessions')),
        scope_id TEXT NOT NULL,
        expected_graph_watermark TEXT NOT NULL,
        state TEXT NOT NULL CHECK(state IN ('pending', 'applied')),
        graph_watermark TEXT,
        created_at INTEGER NOT NULL,
        applied_at INTEGER,
        PRIMARY KEY(session_id, generation),
        CHECK(
            (state = 'pending' AND graph_watermark IS NULL AND applied_at IS NULL)
            OR (state = 'applied' AND graph_watermark = expected_graph_watermark
                AND applied_at IS NOT NULL)
        ),
        FOREIGN KEY(session_id, generation)
            REFERENCES session_temporal_generations(session_id, generation) ON DELETE CASCADE
    );
    CREATE INDEX idx_session_relation_receipts_pending
        ON session_relation_receipts(state, created_at, session_id, generation);