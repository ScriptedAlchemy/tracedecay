use crate::{GraphBudgetKind, GraphDbError};

/// Sized for the largest real indexed checkouts, not typical ones: the
/// TraceDecay repository itself seals more than a million entities, and the
/// dashboard's GPU tier serves brains past 1.6M nodes. A generation above
/// these caps is refused as a typed capacity budget at activation, so a cap
/// below real repositories turns them permanently unserveable (observed live:
/// every activation retry exhausted capacity and the census never sealed).
/// Actual memory pressure is governed by the daemon's resident-memory gate;
/// these remain sanity ceilings against runaway projections.
pub const MAX_VERIFIED_GENERATION_ENTITIES: usize = 8_000_000;
pub const MAX_VERIFIED_GENERATION_RELATIONS: usize = 16_000_000;
pub const MAX_VERIFIED_GENERATION_BATCH_MUTATIONS: usize = 4_096;
pub const MAX_GRAPH_VECTOR_DIMENSION: usize = 4_096;
pub const MAX_GRAPH_IDENTIFIER_BYTES: usize = 1_024;
pub const MAX_GRAPH_ENTITY_LABELS: usize = 128;
pub const MAX_GRAPH_ENTITY_LABEL_BYTES: usize = 64 * 1024;
pub const MAX_GRAPH_PROPERTIES: usize = 256;
pub const MAX_GRAPH_PROPERTY_VALUE_BYTES: usize = 1024 * 1024;
pub const MAX_GRAPH_PROPERTY_AGGREGATE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_GRAPH_BATCH_CANONICAL_BYTES: usize = 1024 * 1024 * 1024;
pub const MAX_SEMANTIC_VECTOR_GRAPH_BATCH_CANONICAL_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn require_generation_capacity(
    kind: &'static str,
    current: usize,
    additions: usize,
    maximum: usize,
) -> Result<(), GraphDbError> {
    if current > maximum {
        return Err(GraphDbError::Corrupt {
            message: format!(
                "native graph generation has {current} {kind}, above the product limit {maximum}"
            ),
        });
    }
    if current
        .checked_add(additions)
        .is_none_or(|projected| projected > maximum)
    {
        return Err(GraphDbError::budget_exhausted_count(
            GraphBudgetKind::Capacity,
            maximum,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::require_generation_capacity;
    use crate::{GraphBudgetKind, GraphDbError};

    #[test]
    fn generation_capacity_distinguishes_persisted_corruption_from_input_budget() {
        assert_eq!(
            require_generation_capacity("entities", 11, 0, 10),
            Err(GraphDbError::Corrupt {
                message: "native graph generation has 11 entities, above the product limit 10"
                    .to_owned(),
            })
        );
        assert_eq!(
            require_generation_capacity("entities", 10, 1, 10),
            Err(GraphDbError::budget_exhausted(
                GraphBudgetKind::Capacity,
                10
            ))
        );
        assert_eq!(require_generation_capacity("entities", 9, 1, 10), Ok(()));
    }
}
