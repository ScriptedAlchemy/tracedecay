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
