//! Static Hotpath names for runtime-core durable boundaries.
//!
//! Every helper is a no-op when the `hotpath` feature is off. Names are
//! bounded-cardinality literals: no paths, project ids, SQL, or user text.
//!
//! Lease-lifetime evidence (Agent 20 owns daemon retirement orchestration):
//! after a successful retirement or close, `client_leases`, `reader_leases`,
//! `snapshot_leases`, and `runtime_leases` must return to the pre-acquire
//! baseline, and `lease_releases` must catch `lease_acquires`.

pub(crate) const CLIENT_LEASES: &str = "runtime_core.registry.client_leases";
pub(crate) const READER_LEASES: &str = "runtime_core.registry.reader_leases";
pub(crate) const SNAPSHOT_LEASES: &str = "runtime_core.registry.snapshot_leases";
pub(crate) const RUNTIME_LEASES: &str = "runtime_core.registry.runtime_leases";
pub(crate) const LEASE_ACQUIRES: &str = "runtime_core.registry.lease_acquires";
pub(crate) const LEASE_RELEASES: &str = "runtime_core.registry.lease_releases";
pub(crate) const OPENS_IN_FLIGHT: &str = "runtime_core.registry.opens_in_flight";
pub(crate) const RUNTIMES_READY: &str = "runtime_core.registry.runtimes_ready";
pub(crate) const RETIREMENT_PENDING: &str = "runtime_core.registry.retirement_pending";
pub(crate) const RETIREMENT_COMMITS: &str = "runtime_core.registry.retirement_commits";
pub(crate) const RETIREMENT_BLOCKS: &str = "runtime_core.registry.retirement_blocks";
pub(crate) const PROFILE_PINS: &str = "runtime_core.registry.profile_pins";
pub(crate) const DB_SNAPSHOTS: &str = "runtime_core.db.snapshots_active";
pub(crate) const RESIDENT_RESERVATIONS: &str = "runtime_core.resident.reservations";
pub(crate) const RESIDENT_USED_BYTES: &str = "runtime_core.resident.used_bytes";
pub(crate) const RESIDENT_REFUSALS: &str = "runtime_core.resident.refusals";

#[inline]
pub(crate) fn gauge_inc(name: &'static str) {
    let handle = hotpath::gauge!(name);
    let _ = name;
    handle.inc(1.0);
}

#[inline]
pub(crate) fn gauge_dec(name: &'static str) {
    let handle = hotpath::gauge!(name);
    let _ = name;
    handle.dec(1.0);
}

#[inline]
pub(crate) fn gauge_add(name: &'static str, delta: u64) {
    if delta == 0 {
        return;
    }
    let handle = hotpath::gauge!(name);
    let _ = name;
    handle.inc(delta as f64);
}

#[inline]
pub(crate) fn gauge_sub(name: &'static str, delta: u64) {
    if delta == 0 {
        return;
    }
    let handle = hotpath::gauge!(name);
    let _ = name;
    handle.dec(delta as f64);
}

#[inline]
pub(crate) fn gauge_set(name: &'static str, value: u64) {
    let handle = hotpath::gauge!(name);
    let _ = name;
    handle.set(value as f64);
}

#[inline]
pub(crate) fn client_lease_acquired() {
    gauge_inc(CLIENT_LEASES);
    gauge_inc(LEASE_ACQUIRES);
}

#[inline]
pub(crate) fn client_lease_released() {
    gauge_dec(CLIENT_LEASES);
    gauge_inc(LEASE_RELEASES);
}

#[inline]
pub(crate) fn runtime_lease_acquired() {
    gauge_inc(RUNTIME_LEASES);
    gauge_inc(LEASE_ACQUIRES);
}

#[inline]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn runtime_lease_released() {
    gauge_dec(RUNTIME_LEASES);
    gauge_inc(LEASE_RELEASES);
}

#[inline]
pub(crate) fn runtime_leases_expired(count: usize) {
    let count = u64::try_from(count).unwrap_or(u64::MAX);
    gauge_sub(RUNTIME_LEASES, count);
    gauge_add(LEASE_RELEASES, count);
}

#[inline]
pub(crate) fn reader_lease_acquired() {
    gauge_inc(READER_LEASES);
    gauge_inc(LEASE_ACQUIRES);
}

#[inline]
pub(crate) fn reader_lease_released() {
    gauge_dec(READER_LEASES);
    gauge_inc(LEASE_RELEASES);
}

#[inline]
pub(crate) fn snapshot_lease_acquired() {
    gauge_inc(SNAPSHOT_LEASES);
    gauge_inc(LEASE_ACQUIRES);
}

#[inline]
pub(crate) fn snapshot_lease_released() {
    gauge_dec(SNAPSHOT_LEASES);
    gauge_inc(LEASE_RELEASES);
}

#[inline]
pub(crate) fn other_lease_acquired() {
    gauge_inc(LEASE_ACQUIRES);
}

#[inline]
pub(crate) fn other_lease_released() {
    gauge_inc(LEASE_RELEASES);
}

#[inline]
pub(crate) fn open_started() {
    gauge_inc(OPENS_IN_FLIGHT);
}

#[inline]
pub(crate) fn open_finished() {
    gauge_dec(OPENS_IN_FLIGHT);
}

#[inline]
pub(crate) fn runtime_ready() {
    gauge_inc(RUNTIMES_READY);
}

#[inline]
pub(crate) fn runtime_unmounted() {
    gauge_dec(RUNTIMES_READY);
}

#[inline]
pub(crate) fn retirement_reserved(count: usize) {
    gauge_add(RETIREMENT_PENDING, u64::try_from(count).unwrap_or(u64::MAX));
}

#[inline]
pub(crate) fn retirement_released(count: usize) {
    gauge_sub(RETIREMENT_PENDING, u64::try_from(count).unwrap_or(u64::MAX));
}

#[inline]
pub(crate) fn retirement_committed(count: usize) {
    gauge_add(RETIREMENT_COMMITS, u64::try_from(count).unwrap_or(u64::MAX));
}

#[inline]
pub(crate) fn retirement_blocked() {
    gauge_inc(RETIREMENT_BLOCKS);
}

#[inline]
pub(crate) fn profile_pin_acquired() {
    gauge_inc(PROFILE_PINS);
}

#[inline]
pub(crate) fn profile_pin_released() {
    gauge_dec(PROFILE_PINS);
}

#[inline]
pub(crate) fn db_snapshot_acquired() {
    gauge_inc(DB_SNAPSHOTS);
}

#[inline]
pub(crate) fn db_snapshot_released() {
    gauge_dec(DB_SNAPSHOTS);
}

#[inline]
pub(crate) fn resident_reserved(used_bytes: u64) {
    gauge_inc(RESIDENT_RESERVATIONS);
    gauge_set(RESIDENT_USED_BYTES, used_bytes);
}

#[inline]
pub(crate) fn resident_released(used_bytes: u64) {
    gauge_dec(RESIDENT_RESERVATIONS);
    gauge_set(RESIDENT_USED_BYTES, used_bytes);
}

#[inline]
pub(crate) fn resident_refused() {
    gauge_inc(RESIDENT_REFUSALS);
}
