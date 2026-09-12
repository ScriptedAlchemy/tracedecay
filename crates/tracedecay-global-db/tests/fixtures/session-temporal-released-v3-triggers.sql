-- Authority-invariant trigger bodies exactly as published by every release
-- that persisted session-temporal schema version 3.
--
-- Extracted verbatim from
-- `crates/tracedecay-global-db/src/schema_contract/invariants/triggers.rs`
-- at tag v0.1.0-beta.37. Every v3 release published one identical
-- eighty-one-trigger authority inventory, so this file is the shipped shape
-- for all of them:
--
--   tag                            marker  authority triggers  inventory digest
--   v0.1.0-beta.25 .. beta.37      3       81 (43 temporal)    2018f3e031bd...
--   (working tree, marker 4)       4       81 (43 temporal)    d5c94eef0337...
--
-- The inventory digest is sha256 over `name|table|normalized_sql` lines of the
-- temporal-table triggers. Names are identical in every row above; the v3 and
-- v4 inventories differ only in the three bodies below, so a store written by
-- any v3 release carries exactly these and matches the current contract
-- everywhere else.
--
-- Loading this file converts a store created at the current contract into the
-- published v3 trigger shape. It is deliberately independent of the production
-- reconstruction in `released_v3_invariant_triggers_intact`: a test that
-- derived the released bodies from the current contract could not detect a
-- reconstruction that drifted from what shipped.

DROP TRIGGER IF EXISTS session_refresh_progress_insert_guard_v1;
CREATE TRIGGER session_refresh_progress_insert_guard_v1
            BEFORE INSERT ON session_refresh_progress
            WHEN NOT EXISTS (
                SELECT 1
                FROM session_refresh_operations AS operation
                JOIN session_refresh_bindings AS binding
                  ON binding.session_id = operation.session_id
                 AND binding.operation_id = operation.operation_id
                JOIN session_temporal_generations AS generation
                  ON generation.session_id = binding.session_id
                 AND generation.generation = binding.generation
                WHERE operation.session_id = NEW.session_id
                  AND operation.operation_id = NEW.operation_id
                  AND operation.state = 'running'
                  AND NEW.recorded_at >= operation.created_at
                  AND binding.source_frontier =
                      json_extract(operation.target_frontier_json, '$.committed_through')
                  AND binding.target_frontier =
                      json_extract(operation.target_frontier_json, '$.observed_through')
                  AND generation.state = 'building'
                  AND generation.frozen_watermarks_json = binding.frozen_watermarks_json
                  AND json_type(NEW.frontier_json, '$.observed_through') IS 'integer'
                  AND json_type(NEW.frontier_json, '$.committed_through') IS 'integer'
                  AND json_extract(NEW.frontier_json, '$.observed_through')
                      = binding.target_frontier
                  AND json_extract(NEW.frontier_json, '$.committed_through')
                      BETWEEN binding.source_frontier AND binding.target_frontier
                  AND json_type(NEW.coverage_json, '$.visible') IS 'integer'
                  AND json_type(NEW.coverage_json, '$.hidden') IS 'integer'
                  AND json_type(NEW.coverage_json, '$.unknown') IS 'integer'
                  AND json_type(NEW.coverage_json, '$.redacted') IS 'integer'
                  AND json_extract(NEW.coverage_json, '$.visible') >= 0
                  AND json_extract(NEW.coverage_json, '$.hidden') >= 0
                  AND json_extract(NEW.coverage_json, '$.unknown') >= 0
                  AND json_extract(NEW.coverage_json, '$.redacted') >= 0
                  AND NEW.committed_records =
                      json_extract(NEW.coverage_json, '$.visible')
                      + json_extract(NEW.coverage_json, '$.hidden')
                      + json_extract(NEW.coverage_json, '$.unknown')
                      + json_extract(NEW.coverage_json, '$.redacted')
                  AND (
                    (
                      NEW.progress_ordinal = 0
                      AND NEW.committed_batches = 0
                      AND NEW.committed_records = 0
                      AND json_extract(NEW.frontier_json, '$.committed_through')
                          = binding.source_frontier
                      AND NOT EXISTS (
                          SELECT 1
                          FROM session_refresh_progress AS seeded
                          WHERE seeded.session_id = NEW.session_id
                            AND seeded.operation_id = NEW.operation_id
                      )
                    )
                    OR (
                      NEW.committed_batches > 0
                      AND NEW.progress_ordinal = NEW.committed_batches - 1
                      AND EXISTS (
                          SELECT 1
                          FROM session_temporal_projection_receipts AS receipt
                          WHERE receipt.session_id = binding.session_id
                            AND receipt.generation = binding.generation
                            AND receipt.batch_ordinal = NEW.progress_ordinal
                            AND length(receipt.batch_digest) = 71
                            AND receipt.batch_digest GLOB 'sha256:[0-9a-f]*'
                            AND substr(receipt.batch_digest, 8) NOT GLOB '*[^0-9a-f]*'
                            AND receipt.projection_through =
                                json_extract(NEW.frontier_json, '$.committed_through')
                            AND NEW.committed_records =
                                receipt.occurrence_count
                                + receipt.copy_count
                                + receipt.assertion_count
                            AND (
                              (
                                NEW.progress_ordinal = 0
                                AND receipt.source_through >= binding.source_frontier
                                AND receipt.source_through <=
                                    json_extract(NEW.frontier_json, '$.committed_through')
                                AND NOT EXISTS (
                                    SELECT 1
                                    FROM session_refresh_progress AS first_previous
                                    WHERE first_previous.session_id = NEW.session_id
                                      AND first_previous.operation_id = NEW.operation_id
                                )
                              )
                              OR EXISTS (
                                  SELECT 1
                                  FROM session_refresh_progress AS previous
                                  WHERE previous.session_id = NEW.session_id
                                    AND previous.operation_id = NEW.operation_id
                                    AND previous.progress_ordinal =
                                        NEW.progress_ordinal - 1
                                    AND previous.progress_ordinal = (
                                        SELECT MAX(latest.progress_ordinal)
                                        FROM session_refresh_progress AS latest
                                        WHERE latest.session_id = NEW.session_id
                                          AND latest.operation_id = NEW.operation_id
                                    )
                                    AND NEW.committed_batches =
                                        previous.committed_batches + 1
                                    AND NEW.committed_records >= previous.committed_records
                                    AND NEW.recorded_at > previous.recorded_at
                                    AND json_extract(
                                        NEW.frontier_json, '$.committed_through'
                                    ) > json_extract(
                                        previous.frontier_json, '$.committed_through'
                                    )
                                    AND receipt.source_through >= json_extract(
                                        previous.frontier_json, '$.committed_through'
                                    )
                                    AND receipt.source_through <= json_extract(
                                        NEW.frontier_json, '$.committed_through'
                                    )
                                    AND json_extract(NEW.coverage_json, '$.visible')
                                        >= json_extract(previous.coverage_json, '$.visible')
                                    AND json_extract(NEW.coverage_json, '$.hidden')
                                        >= json_extract(previous.coverage_json, '$.hidden')
                                    AND json_extract(NEW.coverage_json, '$.unknown')
                                        >= json_extract(previous.coverage_json, '$.unknown')
                                    AND json_extract(NEW.coverage_json, '$.redacted')
                                        >= json_extract(previous.coverage_json, '$.redacted')
                              )
                            )
                      )
                  )
            )
            )
            BEGIN SELECT RAISE(ABORT, 'invalid session refresh progress'); END;

DROP TRIGGER IF EXISTS projection_output_audit_invalidate_update_v1;
CREATE TRIGGER projection_output_audit_invalidate_update_v1
            AFTER UPDATE ON session_messages
            WHEN EXISTS (
                SELECT 1 FROM observation_projection_provenance
                WHERE projector_version = 'claude-session-message-v4'
                  AND output_provider = OLD.provider
                  AND output_message_id = OLD.message_id
            ) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END;

DROP TRIGGER IF EXISTS projection_output_audit_invalidate_delete_v1;
CREATE TRIGGER projection_output_audit_invalidate_delete_v1
            AFTER DELETE ON session_messages
            WHEN EXISTS (
                SELECT 1 FROM observation_projection_provenance
                WHERE projector_version = 'claude-session-message-v4'
                  AND output_provider = OLD.provider
                  AND output_message_id = OLD.message_id
            ) BEGIN
                DELETE FROM authority_audit_checkpoints
                WHERE audit_name = 'observation-authority';
            END;

