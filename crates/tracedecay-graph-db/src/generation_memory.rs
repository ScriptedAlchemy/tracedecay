//! Resident-memory planning for native graph-generation mutations.

/// Whether a persistent generation can prove a finite native-heap peak before
/// applying any Grafeo mutation.
///
/// Grafeo 0.5.42's LPG store grows capacity-backed MVCC, adjacency, property,
/// label, and catalog structures without a hard allocation grant or spill
/// boundary. Consequently, a non-empty mutation set has no finite upper bound
/// that TraceDecay can reserve truthfully before publication.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphGenerationMemoryPlanV1 {
    NoNativeMutations,
    NativeHeapUpperBoundUnavailable,
}

impl GraphGenerationMemoryPlanV1 {
    #[must_use]
    pub const fn for_native_mutation_count(mutation_count: usize) -> Self {
        if mutation_count == 0 {
            Self::NoNativeMutations
        } else {
            Self::NativeHeapUpperBoundUnavailable
        }
    }
}

#[cfg(test)]
mod tests {
    use super::GraphGenerationMemoryPlanV1;

    #[test]
    fn only_an_empty_native_mutation_set_has_a_finite_zero_byte_plan() {
        assert_eq!(
            GraphGenerationMemoryPlanV1::for_native_mutation_count(0),
            GraphGenerationMemoryPlanV1::NoNativeMutations
        );
        assert_eq!(
            GraphGenerationMemoryPlanV1::for_native_mutation_count(1),
            GraphGenerationMemoryPlanV1::NativeHeapUpperBoundUnavailable
        );
    }
}
