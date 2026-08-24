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
