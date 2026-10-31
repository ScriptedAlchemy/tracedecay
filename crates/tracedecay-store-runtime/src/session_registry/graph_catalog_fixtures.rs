//! Interactive graph-catalog release fixtures shared by store-runtime,
//! daemon-service, and composition-root tests.
//!
//! The session registry owns the background catalog warm and residency passes
//! that hold a mounted store's interactive catalog, so waiting for that
//! catalog to go idle belongs beside the registry rather than in each caller.

use std::time::Duration;

use tracedecay_code_index::graph_projection::{
    CodeGraphCatalogReleaseV1, CodeGraphProjectionStore,
};

/// Releases `store`'s interactive catalog once nothing holds it. `Busy` means
/// ask again later: after a test drops its own readers, a background catalog
/// warm or residency pass can still hold the catalog for a moment.
pub async fn release_idle_interactive_catalog(
    store: &CodeGraphProjectionStore,
) -> CodeGraphCatalogReleaseV1 {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match store.release_interactive_catalog() {
            CodeGraphCatalogReleaseV1::Busy if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            release => return release,
        }
    }
}
