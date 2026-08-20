use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use tracedecay_store::{StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

use crate::location::PersistentGraphStoreState;
#[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
use crate::{GraphCancellation, GraphDbLocation, GraphDurability, GraphFormatVersion};
use crate::{GraphDb, GraphDbError, GraphDbOpenOptions, GraphDbRuntimeState, GraphSnapshot};

static NEXT_GRAPH_DB_OWNER_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GraphDbOwnerId(u64);

/// Opaque identity for one native graph runtime allocation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GraphDbRuntimeIdentityV1(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GraphDbOwnerAttachmentId(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct GraphDbRetirementReservationId(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct GraphDbLeaseId(u64);

/// Opaque client authority for one registered graph runtime.
///
/// An independently issued lease contributes one exact client token to the
/// owning runtime. Clones share that token, so they remain one client until
/// the last clone is dropped. The raw `Arc<GraphDb>` remains private to the
/// token.
#[derive(Clone)]
pub struct GraphDbLeaseV1 {
    token: Arc<GraphDbLeaseToken>,
}

struct GraphDbLeaseToken {
    source: Arc<GraphDbOwnerSource>,
    lease_id: GraphDbLeaseId,
}

impl Drop for GraphDbLeaseToken {
    fn drop(&mut self) {
        self.source.state.lock().leases.remove(&self.lease_id);
    }
}

impl std::fmt::Debug for GraphDbLeaseV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbLeaseV1")
            .field("state", &self.runtime_state())
            .finish_non_exhaustive()
    }
}

impl Deref for GraphDbLeaseV1 {
    type Target = GraphDb;

    fn deref(&self) -> &Self::Target {
        &self.token.source.database
    }
}

impl GraphDbLeaseV1 {
    /// Opens a zero-copy read snapshot without exposing shared raw ownership.
    pub fn snapshot(&self) -> Result<GraphSnapshot, GraphDbError> {
        let mut snapshot = self.token.source.database.snapshot()?;
        snapshot.retain_client(self.clone());
        Ok(snapshot)
    }

    #[must_use]
    pub fn shares_runtime_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token.source.database, &other.token.source.database)
    }

    #[must_use]
    pub fn runtime_identity(&self) -> GraphDbRuntimeIdentityV1 {
        GraphDbRuntimeIdentityV1(self.token.source.owner_id.0)
    }
}

/// Opaque map-owned authority for a graph runtime.
///
/// The registry owns this value directly and never exposes its native graph
/// allocation. Callers receive independently counted [`GraphDbLeaseV1`]
/// client leases or one [`GraphDbOwnerAttachmentV1`] selected by the owning
/// map. It deliberately implements neither `Clone` nor `Deref`.
pub struct GraphDbOwner {
    source: Arc<GraphDbOwnerSource>,
}

struct GraphDbOwnerSource {
    database: Arc<GraphDb>,
    owner_id: GraphDbOwnerId,
    state: Mutex<GraphDbOwnerState>,
}

struct GraphDbOwnerState {
    lifecycle: GraphDbOwnerLifecycle,
    next_lease_id: u64,
    next_attachment_id: u64,
    next_reservation_id: u64,
    leases: BTreeMap<GraphDbLeaseId, ()>,
    owner_attachment: Option<OwnerAttachmentState>,
    #[cfg(test)]
    attachment_reservation_failure: Option<GraphDbError>,
    #[cfg(test)]
    retirement_begin_failure: Option<GraphDbError>,
    #[cfg(test)]
    retirement_finish_failure: Option<GraphDbError>,
}

#[derive(Clone, Copy)]
enum GraphDbOwnerLifecycle {
    Ready,
    RetirementFenced(GraphDbRetirementReservationId),
    Closing(GraphDbRetirementReservationId),
    Closed,
    Failed,
    DurabilityUncertain,
}

#[derive(Clone, Copy)]
enum OwnerAttachmentState {
    MapOwned(GraphDbOwnerAttachmentId),
    OwnerReserved {
        attachment_id: GraphDbOwnerAttachmentId,
        reservation_id: GraphDbRetirementReservationId,
    },
}

/// Non-cloneable identity for the graph authority retained by a map owner.
///
/// This attachment is intentionally distinct from client lease tokens. Its
/// retirement target can reclassify only this exact attachment; every normal
/// [`GraphDbLeaseV1`] and snapshot remains a close blocker.
pub struct GraphDbOwnerAttachmentV1 {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    token: Arc<GraphDbOwnerAttachmentToken>,
}

struct GraphDbOwnerAttachmentToken {
    source: Arc<GraphDbOwnerSource>,
    owner_id: GraphDbOwnerId,
    attachment_id: GraphDbOwnerAttachmentId,
}

impl Drop for GraphDbOwnerAttachmentToken {
    fn drop(&mut self) {
        let mut state = self.source.state.lock();
        if matches!(
            state.owner_attachment,
            Some(OwnerAttachmentState::MapOwned(attachment_id)) if attachment_id == self.attachment_id
        ) {
            state.owner_attachment = None;
        }
    }
}

impl std::fmt::Debug for GraphDbOwnerAttachmentV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbOwnerAttachmentV1")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .finish_non_exhaustive()
    }
}

impl GraphDbOwnerAttachmentV1 {
    #[must_use]
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    #[must_use]
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    #[must_use]
    pub fn runtime_identity(&self) -> GraphDbRuntimeIdentityV1 {
        GraphDbRuntimeIdentityV1(self.token.owner_id.0)
    }

    #[must_use]
    pub fn shares_runtime_with(&self, lease: &GraphDbLeaseV1) -> bool {
        Arc::ptr_eq(&self.token.source.database, &lease.token.source.database)
    }

    /// Issues one ordinary, separately counted client lease.
    pub fn issue_lease(&self) -> Result<GraphDbLeaseV1, GraphDbError> {
        issue_client_lease(&self.token.source)
    }

    #[must_use]
    pub fn retirement_target(&self) -> GraphDbRetirementTarget {
        GraphDbRetirementTarget {
            binding: self.binding.clone(),
            verified_locator: self.verified_locator.clone(),
            owner_id: self.token.owner_id,
            attachment_id: self.token.attachment_id,
            retained_attachment: Arc::clone(&self.token),
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_retirement_reservation_failure(&self, error: GraphDbError) {
        self.token
            .source
            .state
            .lock()
            .attachment_reservation_failure = Some(error);
    }

    #[cfg(test)]
    pub(crate) fn inject_stale_owner_attachment_id(&self) -> Result<(), GraphDbError> {
        let mut state = self.token.source.state.lock();
        if !matches!(state.lifecycle, GraphDbOwnerLifecycle::Ready) {
            return Err(GraphDbError::Conflict);
        }
        let attachment_id =
            next_local_id(&mut state.next_attachment_id).map(GraphDbOwnerAttachmentId)?;
        state.owner_attachment = Some(OwnerAttachmentState::MapOwned(attachment_id));
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn inject_retirement_finish_failure(&self, error: GraphDbError) {
        self.token.source.state.lock().retirement_finish_failure = Some(error);
    }

    #[cfg(test)]
    pub(crate) fn inject_retirement_begin_failure(&self, error: GraphDbError) {
        self.token.source.state.lock().retirement_begin_failure = Some(error);
    }
}

/// Exact graph map-owner attachment selected for retirement.
///
/// Construct it only through [`GraphDbOwnerAttachmentV1::retirement_target`].
/// The target privately retains the attachment's exact identity for the whole
/// reservation; it never carries or reclassifies a client lease token.
#[derive(Clone)]
pub struct GraphDbRetirementTarget {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    owner_id: GraphDbOwnerId,
    attachment_id: GraphDbOwnerAttachmentId,
    retained_attachment: Arc<GraphDbOwnerAttachmentToken>,
}

impl PartialEq for GraphDbRetirementTarget {
    fn eq(&self, other: &Self) -> bool {
        self.binding == other.binding
            && self.verified_locator == other.verified_locator
            && self.owner_id == other.owner_id
            && self.attachment_id == other.attachment_id
            && Arc::ptr_eq(&self.retained_attachment, &other.retained_attachment)
    }
}

impl Eq for GraphDbRetirementTarget {}

impl std::fmt::Debug for GraphDbRetirementTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbRetirementTarget")
            .field("binding", &self.binding)
            .field("verified_locator", &self.verified_locator)
            .finish_non_exhaustive()
    }
}

impl GraphDbRetirementTarget {
    #[must_use]
    pub fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    #[must_use]
    pub fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    pub(crate) fn owner_id(&self) -> GraphDbOwnerId {
        self.owner_id
    }

    pub(crate) fn attachment_id(&self) -> GraphDbOwnerAttachmentId {
        self.attachment_id
    }

    fn matches_source(&self, source: &Arc<GraphDbOwnerSource>) -> bool {
        Arc::ptr_eq(&self.retained_attachment.source, source)
            && self.retained_attachment.owner_id == self.owner_id
            && self.retained_attachment.attachment_id == self.attachment_id
    }
}

impl std::fmt::Debug for GraphDbOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GraphDbOwner")
            .field("state", &self.runtime_state())
            .finish_non_exhaustive()
    }
}

impl GraphDbOwner {
    #[cfg(any(feature = "test-helpers", feature = "eval-helpers"))]
    pub fn memory(cancellation: Arc<dyn GraphCancellation>) -> Result<Self, GraphDbError> {
        Self::open(GraphDbOpenOptions {
            location: GraphDbLocation::Memory,
            expected_format: GraphFormatVersion::current(),
            durability: GraphDurability::Memory,
            cancellation,
        })
    }

    #[cfg(any(test, feature = "test-helpers", feature = "eval-helpers"))]
    pub(crate) fn open(options: GraphDbOpenOptions) -> Result<Self, GraphDbError> {
        GraphDb::open(options).and_then(Self::from_database)
    }

    pub(crate) fn open_registered(
        options: GraphDbOpenOptions,
        persistent_store_state: PersistentGraphStoreState,
    ) -> Result<Self, GraphDbError> {
        GraphDb::open_with_store_state(options, Some(persistent_store_state))
            .and_then(Self::from_database)
    }

    /// Issues an ordinary counted client lease while the owner is ready.
    pub fn issue_lease(&self) -> Result<GraphDbLeaseV1, GraphDbError> {
        issue_client_lease(&self.source)
    }

    #[must_use]
    pub fn runtime_state(&self) -> GraphDbRuntimeState {
        self.source.database.runtime_state()
    }

    pub fn close(&self) -> Result<(), GraphDbError> {
        let result = self.source.database.close();
        let mut state = self.source.state.lock();
        if matches!(state.lifecycle, GraphDbOwnerLifecycle::Ready) {
            state.lifecycle = terminal_lifecycle(&result);
        }
        result
    }

    pub(crate) fn owner_id(&self) -> GraphDbOwnerId {
        self.source.owner_id
    }

    pub(crate) fn issue_owner_attachment(
        &self,
        binding: StoreRuntimeBindingV1,
        verified_locator: VerifiedStoreLocatorV1,
    ) -> Result<GraphDbOwnerAttachmentV1, GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(state.lifecycle, GraphDbOwnerLifecycle::Ready)
            || state.owner_attachment.is_some()
        {
            return Err(GraphDbError::Conflict);
        }
        let attachment_id = next_local_id(&mut state.next_attachment_id)?;
        state.owner_attachment = Some(OwnerAttachmentState::MapOwned(attachment_id));
        drop(state);
        Ok(GraphDbOwnerAttachmentV1 {
            binding,
            verified_locator,
            token: Arc::new(GraphDbOwnerAttachmentToken {
                source: Arc::clone(&self.source),
                owner_id: self.source.owner_id,
                attachment_id,
            }),
        })
    }

    pub(crate) fn can_reserve_owner_attachment(
        &self,
        target: &GraphDbRetirementTarget,
    ) -> Result<(), GraphDbError> {
        let state = self.source.state.lock();
        self.require_map_owned_attachment(&state, target)?;
        if !state.leases.is_empty() {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }

    pub(crate) fn reserve_owner_attachment(
        &self,
        target: &GraphDbRetirementTarget,
    ) -> Result<GraphDbRetirementReservationId, GraphDbError> {
        let mut state = self.source.state.lock();
        self.require_map_owned_attachment(&state, target)?;
        if !state.leases.is_empty() {
            return Err(GraphDbError::Conflict);
        }
        #[cfg(test)]
        if let Some(error) = state.attachment_reservation_failure.take() {
            return Err(error);
        }
        let reservation_id =
            next_local_id(&mut state.next_reservation_id).map(GraphDbRetirementReservationId)?;
        state.lifecycle = GraphDbOwnerLifecycle::RetirementFenced(reservation_id);
        state.owner_attachment = Some(OwnerAttachmentState::OwnerReserved {
            attachment_id: target.attachment_id,
            reservation_id,
        });
        Ok(reservation_id)
    }

    pub(crate) fn restore_owner_attachment(
        &self,
        target: &GraphDbRetirementTarget,
        reservation_id: GraphDbRetirementReservationId,
    ) -> Result<(), GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(
            state.lifecycle,
            GraphDbOwnerLifecycle::RetirementFenced(current) if current == reservation_id
        ) || !matches!(
            state.owner_attachment,
            Some(OwnerAttachmentState::OwnerReserved {
                attachment_id,
                reservation_id: current,
            }) if attachment_id == target.attachment_id && current == reservation_id
        ) || !target.matches_source(&self.source)
        {
            return Err(GraphDbError::Conflict);
        }
        state.lifecycle = GraphDbOwnerLifecycle::Ready;
        state.owner_attachment = Some(OwnerAttachmentState::MapOwned(target.attachment_id));
        Ok(())
    }

    pub(crate) fn restore_owner_attachment_before_native_close(
        &self,
        target: &GraphDbRetirementTarget,
        reservation_id: GraphDbRetirementReservationId,
    ) -> Result<(), GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(
            state.lifecycle,
            GraphDbOwnerLifecycle::RetirementFenced(current)
                | GraphDbOwnerLifecycle::Closing(current)
                if current == reservation_id
        ) || !matches!(
            state.owner_attachment,
            Some(OwnerAttachmentState::OwnerReserved {
                attachment_id,
                reservation_id: current,
            }) if attachment_id == target.attachment_id && current == reservation_id
        ) || !target.matches_source(&self.source)
        {
            return Err(GraphDbError::Conflict);
        }
        state.lifecycle = GraphDbOwnerLifecycle::Ready;
        state.owner_attachment = Some(OwnerAttachmentState::MapOwned(target.attachment_id));
        Ok(())
    }

    pub(crate) fn begin_owner_attachment_close(
        &self,
        target: &GraphDbRetirementTarget,
        reservation_id: GraphDbRetirementReservationId,
    ) -> Result<(), GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(
            state.lifecycle,
            GraphDbOwnerLifecycle::RetirementFenced(current) if current == reservation_id
        ) || !matches!(
            state.owner_attachment,
            Some(OwnerAttachmentState::OwnerReserved {
                attachment_id,
                reservation_id: current,
            }) if attachment_id == target.attachment_id && current == reservation_id
        ) || !target.matches_source(&self.source)
        {
            return Err(GraphDbError::Conflict);
        }
        #[cfg(test)]
        if let Some(error) = state.retirement_begin_failure.take() {
            return Err(error);
        }
        state.lifecycle = GraphDbOwnerLifecycle::Closing(reservation_id);
        Ok(())
    }

    pub(crate) fn finish_owner_attachment_close(
        &self,
        reservation_id: GraphDbRetirementReservationId,
        result: &Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(
            state.lifecycle,
            GraphDbOwnerLifecycle::Closing(current) if current == reservation_id
        ) {
            return Err(GraphDbError::Conflict);
        }
        #[cfg(test)]
        if let Some(error) = state.retirement_finish_failure.take() {
            return Err(error);
        }
        state.lifecycle = terminal_lifecycle(result);
        Ok(())
    }

    pub(crate) fn reserve_unleased_close(
        &self,
    ) -> Result<GraphDbRetirementReservationId, GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(state.lifecycle, GraphDbOwnerLifecycle::Ready)
            || state.owner_attachment.is_some()
            || !state.leases.is_empty()
        {
            return Err(GraphDbError::Conflict);
        }
        let reservation_id =
            next_local_id(&mut state.next_reservation_id).map(GraphDbRetirementReservationId)?;
        state.lifecycle = GraphDbOwnerLifecycle::RetirementFenced(reservation_id);
        Ok(reservation_id)
    }

    pub(crate) fn restore_unleased_close(
        &self,
        reservation_id: GraphDbRetirementReservationId,
    ) -> Result<(), GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(
            state.lifecycle,
            GraphDbOwnerLifecycle::RetirementFenced(current) if current == reservation_id
        ) || state.owner_attachment.is_some()
        {
            return Err(GraphDbError::Conflict);
        }
        state.lifecycle = GraphDbOwnerLifecycle::Ready;
        Ok(())
    }

    pub(crate) fn begin_unleased_close(
        &self,
        reservation_id: GraphDbRetirementReservationId,
    ) -> Result<(), GraphDbError> {
        let mut state = self.source.state.lock();
        if !matches!(
            state.lifecycle,
            GraphDbOwnerLifecycle::RetirementFenced(current) if current == reservation_id
        ) || state.owner_attachment.is_some()
        {
            return Err(GraphDbError::Conflict);
        }
        state.lifecycle = GraphDbOwnerLifecycle::Closing(reservation_id);
        Ok(())
    }

    pub(crate) fn finish_unleased_close(
        &self,
        reservation_id: GraphDbRetirementReservationId,
        result: &Result<(), GraphDbError>,
    ) -> Result<(), GraphDbError> {
        self.finish_owner_attachment_close(reservation_id, result)
    }

    /// Once native close has started, this owner is terminal even if later
    /// registry bookkeeping cannot complete.
    pub(crate) fn force_terminal_after_close(&self, result: &Result<(), GraphDbError>) {
        self.source.state.lock().lifecycle = terminal_lifecycle(result);
    }

    #[cfg(test)]
    pub(crate) fn inject_close_finish_failure(&self, error: GraphDbError) {
        self.source.state.lock().retirement_finish_failure = Some(error);
    }

    pub(crate) fn is_unleased(&self) -> bool {
        let state = self.source.state.lock();
        state.owner_attachment.is_none() && state.leases.is_empty()
    }

    fn from_database(database: Arc<GraphDb>) -> Result<Self, GraphDbError> {
        let owner_id = NEXT_GRAPH_DB_OWNER_ID
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(GraphDbOwnerId)
            .map_err(|_| {
                GraphDbError::unavailable("graph database owner identity space exhausted")
            })?;
        Ok(Self {
            source: Arc::new(GraphDbOwnerSource {
                database,
                owner_id,
                state: Mutex::new(GraphDbOwnerState {
                    lifecycle: GraphDbOwnerLifecycle::Ready,
                    next_lease_id: 1,
                    next_attachment_id: 1,
                    next_reservation_id: 1,
                    leases: BTreeMap::new(),
                    owner_attachment: None,
                    #[cfg(test)]
                    attachment_reservation_failure: None,
                    #[cfg(test)]
                    retirement_begin_failure: None,
                    #[cfg(test)]
                    retirement_finish_failure: None,
                }),
            }),
        })
    }

    fn require_map_owned_attachment(
        &self,
        state: &GraphDbOwnerState,
        target: &GraphDbRetirementTarget,
    ) -> Result<(), GraphDbError> {
        if !matches!(state.lifecycle, GraphDbOwnerLifecycle::Ready)
            || !matches!(
                state.owner_attachment,
                Some(OwnerAttachmentState::MapOwned(attachment_id)) if attachment_id == target.attachment_id
            )
            || target.owner_id != self.source.owner_id
            || !target.matches_source(&self.source)
        {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }
}

fn issue_client_lease(source: &Arc<GraphDbOwnerSource>) -> Result<GraphDbLeaseV1, GraphDbError> {
    let mut state = source.state.lock();
    if !matches!(state.lifecycle, GraphDbOwnerLifecycle::Ready) {
        return Err(GraphDbError::Conflict);
    }
    let lease_id = next_local_id(&mut state.next_lease_id).map(GraphDbLeaseId)?;
    state.leases.insert(lease_id, ());
    drop(state);
    Ok(GraphDbLeaseV1 {
        token: Arc::new(GraphDbLeaseToken {
            source: Arc::clone(source),
            lease_id,
        }),
    })
}

fn next_local_id(next: &mut u64) -> Result<u64, GraphDbError> {
    let current = *next;
    *next = next
        .checked_add(1)
        .ok_or_else(|| GraphDbError::unavailable("graph database identity space exhausted"))?;
    Ok(current)
}

fn terminal_lifecycle(result: &Result<(), GraphDbError>) -> GraphDbOwnerLifecycle {
    match result {
        Ok(()) | Err(GraphDbError::Closed) => GraphDbOwnerLifecycle::Closed,
        Err(GraphDbError::DurabilityUncertain { .. }) => GraphDbOwnerLifecycle::DurabilityUncertain,
        Err(_) => GraphDbOwnerLifecycle::Failed,
    }
}
