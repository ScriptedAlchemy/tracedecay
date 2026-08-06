//! Persistence adapters owned by the use-case layer.

pub mod observation;
mod vector_generation_inventory;
pub mod vector_generations;

pub use observation::GlobalDbObservationStore;
