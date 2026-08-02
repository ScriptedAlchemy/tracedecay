use tracedecay_domain::{FactEventId, PayloadAccessState};

#[derive(Clone)]
pub(super) struct OwnerKey {
    pub(super) kind: &'static str,
    pub(super) project_id: String,
    pub(super) json: String,
}

/// The lineage state the live purge path compares against: the current payload
/// access and the CAS identity of the last recorded event.
pub(super) struct CurrentFactState {
    pub(super) access: PayloadAccessState,
    pub(super) last_event_id: FactEventId,
}
