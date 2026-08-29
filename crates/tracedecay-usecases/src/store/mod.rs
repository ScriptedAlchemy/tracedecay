//! Persistence adapters owned by the use-case layer.

pub mod observation;
/// Transcript-store adapter over an already-open registered global database.
pub mod transcript;
pub mod vector_generations;

pub use observation::GlobalDbObservationStore;
