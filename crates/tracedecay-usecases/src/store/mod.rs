//! Persistence adapters owned by the use-case layer.

pub mod observation;
pub(crate) mod vector_generations;

pub use observation::GlobalDbObservationStore;
