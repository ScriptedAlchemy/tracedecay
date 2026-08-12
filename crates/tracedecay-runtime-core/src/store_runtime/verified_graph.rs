use std::sync::{Arc, atomic::AtomicBool};

use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    VerifiedGraphSnapshot,
};
use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

/// The sole verified graph publication and recovery authority retained by a
/// canonical relational shard.
pub trait VerifiedGraphRuntimePortV1: Send + Sync {
    /// Exact relational runtime whose replay journal and verified-head CAS
    /// back this graph authority.
    fn relational_binding(&self) -> &StoreRuntimeBindingV1;

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1;

    /// Closes lifecycle admission for background reconciliation owned by this
    /// exact retained runtime. In-flight publication observes the same signal
    /// and remains joinable by its database task owner.
    fn cancel_reconciliation(&self);

    /// Closes the exact derived graph attachment after reconciliation has
    /// joined, releasing its registry owner before relational retirement.
    fn close_reconciliation(&self) -> Result<(), GraphDbError>;

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError>;

    /// Reconciles one canonical manifest under the retained runtime's daemon
    /// lifecycle. Memory commit and mount catch-up callers do not own a
    /// request cancellation token and must not fabricate one.
    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError>;

    /// Recovers the projection's verified head. A projection that has never
    /// published answers `Ok(None)`; an unmounted authority is not represented
    /// by an implementation of this port.
    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError>;
}
