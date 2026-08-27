//! Memory introspection trait for reporting heap usage.

/// Trait for components that can report their memory usage.
///
/// Implementations should estimate heap memory owned by the component,
/// using `capacity() * element_size` for collections and `size_of::<T>()`
/// for fixed-size fields. The goal is a useful approximation, not an
/// exact byte count.
pub trait MemoryReporter {
    /// Returns estimated heap memory usage in bytes.
    ///
    /// This should include:
    /// - Heap allocations owned by this component (Vec capacity, HashMap buckets, etc.)
    /// - Nested heap allocations (String contents, Box contents)
    ///
    /// This should NOT include:
    /// - The size of `self` on the stack (the caller adds that if needed)
    /// - Memory owned by other components (avoid double-counting)
    fn heap_memory_bytes(&self) -> usize;

    /// Returns the number of logical items stored.
    ///
    /// For a hash map this might be `len()`, for an index the number of entries,
    /// for a cache the number of cached items.
    fn item_count(&self) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedReporter(usize);

    impl MemoryReporter for FixedReporter {
        fn heap_memory_bytes(&self) -> usize {
            self.0
        }
    }

    #[test]
    fn item_count_default_returns_zero() {
        let r = FixedReporter(64);
        assert_eq!(r.item_count(), 0);
    }

    #[test]
    fn heap_memory_bytes_delegates_to_impl() {
        let r = FixedReporter(1024);
        assert_eq!(r.heap_memory_bytes(), 1024);
    }
}
