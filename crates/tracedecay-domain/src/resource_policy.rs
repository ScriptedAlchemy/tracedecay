/// Host-derived width for CPU-heavy work under a caller-owned upper bound.
///
/// A host that cannot report its logical CPU count receives one worker. A
/// zero upper bound is normalized to one so every admitted operation has a
/// usable width.
#[must_use]
pub fn host_cpu_target(maximum: usize) -> usize {
    std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(maximum.max(1))
}
