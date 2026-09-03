//! Persistence adapters owned by the use-case layer.

pub mod observation;
pub mod vector_generations;

pub use observation::GlobalDbObservationStore;
