//! Persistence adapters owned by the use-case layer.

pub mod observation;
/// Transcript-store adapter over an already-open registered global database.
/// Moved to `tracedecay-session-memory`; re-exported here as the cutover seam
/// for consumers still on the old path.
pub use tracedecay_session_memory::transcript;
pub mod vector_generations;

pub use observation::GlobalDbObservationStore;
