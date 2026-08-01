use tracedecay_domain::DurableObservationV1;
use tracedecay_store::{
    ProjectionStoreError, ProjectionStoreResult, SESSION_MESSAGE_PROJECTOR_VERSION,
    SessionMessageProjection, SessionMessageRecord, WorkflowFactProjection, WorkflowFactRecord,
};

use tracedecay_runtime_core::db::engine::{Executor, QueryExecutor, params};

use super::state::{
    has_other_projector_output_owner, protected_message_rows_compatible, same_projection_lineage,
    storage,
};

const LIVE_WORKFLOW_FACT_INSERT: &str = "WITH ignored_generation(generation) AS (VALUES (?2))
     INSERT INTO observation_workflow_facts (
        projector_version, observation_id, fact_ordinal, retrieval_anchor_id, receipt_id,
        observation_sequence, provider, session_id, semantic_kind, provider_reference, item_id,
        parent_reference, list_reference, state, status, item_order, native_revision,
        event_sequence, source_sequence, native_timestamp, ordering_domain, content_json,
        content_text, output_digest
     ) VALUES (
        ?1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
        ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
     ) ON CONFLICT DO NOTHING";

const STAGED_WORKFLOW_FACT_INSERT: &str =
    "INSERT OR IGNORE INTO observation_projection_rebuild_workflow_facts (
        projector_version, generation, observation_id, fact_ordinal, retrieval_anchor_id,
        receipt_id, observation_sequence, provider, session_id, semantic_kind,
        provider_reference, item_id, parent_reference, list_reference, state, status,
        item_order, native_revision, event_sequence, source_sequence, native_timestamp,
        ordering_domain, content_json, content_text, output_digest
     ) VALUES (
        ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
        ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25
     )";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MessageTransition {
    Insert,
    Retain,
    Supersede,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct MessageTransitionState {
    latest_sequence: u64,
    same_lineage: bool,
    same_generation: bool,
    projector_owned: bool,
}

impl MessageTransitionState {
    pub(super) fn new(
        observation: &DurableObservationV1,
        latest_observation: &DurableObservationV1,
        latest_sequence: u64,
        projector_owned: bool,
    ) -> Self {
        Self {
            latest_sequence,
            same_lineage: same_projection_lineage(observation, latest_observation),
            same_generation: observation.identity().generation()
                == latest_observation.identity().generation(),
            projector_owned,
        }
    }
}

pub(super) struct WorkflowFactTransition<'a> {
    projection: &'a WorkflowFactProjection,
    observation_sequence: i64,
    item_order: Option<i64>,
    event_sequence: Option<i64>,
    source_sequence: Option<i64>,
}

#[derive(Clone, Copy)]
pub(super) enum WorkflowFactTarget<'a> {
    Live,
    Staged { generation: &'a str },
}

impl<'a> WorkflowFactTarget<'a> {
    fn sql(self) -> &'static str {
        match self {
            Self::Live => LIVE_WORKFLOW_FACT_INSERT,
            Self::Staged { .. } => STAGED_WORKFLOW_FACT_INSERT,
        }
    }

    fn generation(self) -> &'a str {
        match self {
            Self::Live => "",
            Self::Staged { generation } => generation,
        }
    }

    fn projector_version<'b>(self, transition: &'b WorkflowFactTransition<'_>) -> &'b str {
        match self {
            Self::Live => transition.projection().provenance().projector_version(),
            Self::Staged { .. } => SESSION_MESSAGE_PROJECTOR_VERSION,
        }
    }

    fn operation(self) -> &'static str {
        match self {
            Self::Live => "insert projected workflow fact",
            Self::Staged { .. } => "stage projection workflow fact",
        }
    }
}

impl<'a> WorkflowFactTransition<'a> {
    pub(super) fn new(
        sequence: u64,
        projection: &'a WorkflowFactProjection,
    ) -> ProjectionStoreResult<Self> {
        let fact = projection.fact();
        Ok(Self {
            projection,
            observation_sequence: i64::try_from(sequence)
                .map_err(|_| ProjectionStoreError::SequenceOverflow(sequence))?,
            item_order: optional_sequence(fact.item_order)?,
            event_sequence: optional_sequence(fact.event_sequence)?,
            source_sequence: optional_sequence(fact.source_sequence)?,
        })
    }

    pub(super) fn projection(&self) -> &WorkflowFactProjection {
        self.projection
    }

    pub(super) fn fact(&self) -> &WorkflowFactRecord {
        self.projection.fact()
    }

    pub(super) fn observation_sequence(&self) -> i64 {
        self.observation_sequence
    }

    pub(super) fn item_order(&self) -> Option<i64> {
        self.item_order
    }

    pub(super) fn event_sequence(&self) -> Option<i64> {
        self.event_sequence
    }

    pub(super) fn source_sequence(&self) -> Option<i64> {
        self.source_sequence
    }
}

fn optional_sequence(sequence: Option<u64>) -> ProjectionStoreResult<Option<i64>> {
    sequence
        .map(|value| {
            i64::try_from(value).map_err(|_| ProjectionStoreError::SequenceOverflow(value))
        })
        .transpose()
}

pub(super) async fn write_workflow_fact_transition(
    conn: &impl Executor,
    target: WorkflowFactTarget<'_>,
    transition: &WorkflowFactTransition<'_>,
    semantic_kind: &str,
    content_json: Option<&str>,
) -> ProjectionStoreResult<u64> {
    let projection = transition.projection();
    let provenance = projection.provenance();
    let fact = transition.fact();
    let output_digest = projection.output_digest()?.as_str();
    conn.execute(
        target.sql(),
        params![
            target.projector_version(transition),
            target.generation(),
            provenance.observation_id().as_str(),
            i64::from(fact.fact_ordinal),
            provenance.retrieval_anchor_id().as_str(),
            provenance.receipt_id(),
            transition.observation_sequence(),
            projection.session().provider.as_str(),
            projection.session().session_id.as_str(),
            semantic_kind,
            fact.provider_reference.as_deref(),
            fact.item_id.as_deref(),
            fact.parent_reference.as_deref(),
            fact.list_reference.as_deref(),
            fact.state.as_deref(),
            fact.status.as_deref(),
            transition.item_order(),
            fact.native_revision.as_deref(),
            transition.event_sequence(),
            transition.source_sequence(),
            fact.native_timestamp,
            fact.ordering_domain.as_str(),
            content_json,
            fact.content_text.as_str(),
            output_digest,
        ],
    )
    .await
    .map_err(|error| storage(target.operation(), error))
}

pub(super) async fn message_transition(
    conn: &impl QueryExecutor,
    sequence: u64,
    projection: &SessionMessageProjection,
    existing: Option<&SessionMessageRecord>,
    state: Option<MessageTransitionState>,
) -> ProjectionStoreResult<(MessageTransition, bool)> {
    let protected_compatibility = match existing {
        Some(actual) => {
            protected_message_rows_compatible(conn, actual, projection.message()).await?
        }
        None => false,
    };
    let classified_existing = if protected_compatibility {
        Some(projection.message())
    } else {
        existing
    };
    let transition =
        classify_message_transition(sequence, projection.message(), classified_existing, state)?;
    if transition == MessageTransition::Supersede
        && has_other_projector_output_owner(conn, projection).await?
    {
        return Err(output_collision(projection.message()));
    }
    Ok((transition, protected_compatibility))
}

fn classify_message_transition(
    sequence: u64,
    message: &SessionMessageRecord,
    existing: Option<&SessionMessageRecord>,
    state: Option<MessageTransitionState>,
) -> ProjectionStoreResult<MessageTransition> {
    match (existing, state) {
        (Some(actual), Some(state)) => {
            if !state.same_lineage {
                return Err(output_collision(message));
            }
            if sequence < state.latest_sequence {
                return Ok(MessageTransition::Retain);
            }
            if state.same_generation {
                return if actual == message {
                    Ok(MessageTransition::Retain)
                } else {
                    Err(output_collision(message))
                };
            }
            if !state.projector_owned || actual == message {
                Ok(MessageTransition::Retain)
            } else {
                Ok(MessageTransition::Supersede)
            }
        }
        (Some(actual), None) if actual == message => Ok(MessageTransition::Retain),
        (Some(_), None) | (None, Some(_)) => Err(output_collision(message)),
        (None, None) => Ok(MessageTransition::Insert),
    }
}

fn output_collision(message: &SessionMessageRecord) -> ProjectionStoreError {
    ProjectionStoreError::OutputCollision {
        provider: message.provider.clone(),
        message_id: message.message_id.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(text: &str) -> SessionMessageRecord {
        SessionMessageRecord {
            provider: "test".to_owned(),
            message_id: "message-1".to_owned(),
            session_id: "session-1".to_owned(),
            role: "assistant".to_owned(),
            timestamp: Some(1),
            ordinal: 0,
            text: text.to_owned(),
            kind: None,
            model: None,
            tool_names: None,
            source_path: None,
            source_offset: None,
            metadata_json: None,
        }
    }

    fn state(
        latest_sequence: u64,
        same_lineage: bool,
        same_generation: bool,
        projector_owned: bool,
    ) -> MessageTransitionState {
        MessageTransitionState {
            latest_sequence,
            same_lineage,
            same_generation,
            projector_owned,
        }
    }

    #[test]
    fn missing_rows_have_one_consistent_transition() {
        let expected = message("expected");
        assert_eq!(
            classify_message_transition(1, &expected, None, None).unwrap(),
            MessageTransition::Insert
        );
        assert!(matches!(
            classify_message_transition(
                1,
                &expected,
                Some(&expected),
                Some(state(1, true, true, true))
            ),
            Ok(MessageTransition::Retain)
        ));
        assert!(matches!(
            classify_message_transition(1, &expected, None, Some(state(1, true, true, true))),
            Err(ProjectionStoreError::OutputCollision { .. })
        ));
    }

    #[test]
    fn same_generation_requires_compatible_rows() {
        let actual = message("actual");
        let expected = message("expected");
        assert!(matches!(
            classify_message_transition(
                2,
                &expected,
                Some(&actual),
                Some(state(1, true, true, true))
            ),
            Err(ProjectionStoreError::OutputCollision { .. })
        ));
        assert!(matches!(
            classify_message_transition(
                2,
                &expected,
                Some(&expected),
                Some(state(1, false, true, true))
            ),
            Err(ProjectionStoreError::OutputCollision { .. })
        ));
    }

    #[test]
    fn rollover_only_supersedes_exclusively_owned_rows() {
        let actual = message("actual");
        let expected = message("expected");
        assert_eq!(
            classify_message_transition(
                2,
                &expected,
                Some(&actual),
                Some(state(1, true, false, true))
            )
            .unwrap(),
            MessageTransition::Supersede
        );
        assert_eq!(
            classify_message_transition(
                2,
                &expected,
                Some(&actual),
                Some(state(1, true, false, false))
            )
            .unwrap(),
            MessageTransition::Retain
        );
        assert_eq!(
            classify_message_transition(
                0,
                &expected,
                Some(&actual),
                Some(state(1, true, false, true))
            )
            .unwrap(),
            MessageTransition::Retain
        );
    }
}
