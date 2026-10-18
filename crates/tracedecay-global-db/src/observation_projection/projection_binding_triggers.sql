-- Anchor provenance is invariant across projector versions. Replace the existing
-- trigger definitions on admission so retained stores also enforce this boundary.
DROP TRIGGER IF EXISTS projection_provenance_binding_insert_v4;
CREATE TRIGGER projection_provenance_binding_insert_v4
BEFORE INSERT ON observation_projection_provenance
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid projection provenance binding');
END;

DROP TRIGGER IF EXISTS projection_provenance_binding_update_v4;
CREATE TRIGGER projection_provenance_binding_update_v4
BEFORE UPDATE OF projector_version, observation_id, retrieval_anchor_id, receipt_id
ON observation_projection_provenance
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid projection provenance binding');
END;

DROP TRIGGER IF EXISTS projection_workflow_binding_insert_v4;
CREATE TRIGGER projection_workflow_binding_insert_v4
BEFORE INSERT ON observation_workflow_facts
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid workflow provenance binding');
END;

DROP TRIGGER IF EXISTS projection_workflow_binding_update_v4;
CREATE TRIGGER projection_workflow_binding_update_v4
BEFORE UPDATE OF projector_version, observation_id, retrieval_anchor_id, receipt_id
ON observation_workflow_facts
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid workflow provenance binding');
END;

DROP TRIGGER IF EXISTS projection_rebuild_provenance_binding_insert_v4;
CREATE TRIGGER projection_rebuild_provenance_binding_insert_v4
BEFORE INSERT ON observation_projection_rebuild_provenance
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid rebuild provenance binding');
END;

DROP TRIGGER IF EXISTS projection_rebuild_provenance_binding_update_v4;
CREATE TRIGGER projection_rebuild_provenance_binding_update_v4
BEFORE UPDATE OF projector_version, observation_id, retrieval_anchor_id, receipt_id
ON observation_projection_rebuild_provenance
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid rebuild provenance binding');
END;

DROP TRIGGER IF EXISTS projection_rebuild_workflow_binding_insert_v4;
CREATE TRIGGER projection_rebuild_workflow_binding_insert_v4
BEFORE INSERT ON observation_projection_rebuild_workflow_facts
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid rebuild workflow binding');
END;

DROP TRIGGER IF EXISTS projection_rebuild_workflow_binding_update_v4;
CREATE TRIGGER projection_rebuild_workflow_binding_update_v4
BEFORE UPDATE OF projector_version, observation_id, retrieval_anchor_id, receipt_id
ON observation_projection_rebuild_workflow_facts
WHEN (
    NEW.retrieval_anchor_id IS NULL OR NOT EXISTS (
        SELECT 1
        FROM observation_retrieval_anchors AS binding
        JOIN observations AS observation
          ON observation.observation_id = binding.observation_id
        WHERE binding.observation_id = NEW.observation_id
          AND binding.anchor_id = NEW.retrieval_anchor_id
          AND observation.receipt_id = NEW.receipt_id
    )
)
BEGIN
    SELECT RAISE(ABORT, 'invalid rebuild workflow binding');
END;
