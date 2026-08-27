//! Epoch-based arena allocator for MVCC.
//!
//! This is how Grafeo manages memory for versioned data. Each epoch gets its
//! own arena, and when all readers from an old epoch finish, we free the whole
//! thing at once. Much faster than tracking individual allocations.
//!
//! Use [`ArenaAllocator`] to manage multiple epochs, or [`Arena`] directly
//! if you're working with a single epoch.

// Arena allocators require unsafe code for memory management
#![allow(unsafe_code)]

use std::alloc::{Layout, alloc, dealloc};
use std::fmt;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};

use parking_lot::RwLock;

use crate::types::EpochId;

/// Default chunk size for arena allocations (1 MB).
const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;

/// Errors from arena allocation operations.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AllocError {
    /// The system allocator returned null (out of memory).
    OutOfMemory,
    /// The requested epoch does not exist.
    EpochNotFound(EpochId),
    /// Arena chunk has insufficient space for the allocation.
    InsufficientSpace,
    /// Alignment must be a non-zero power of two.
    InvalidAlignment(usize),
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => write!(f, "arena allocation failed: out of memory"),
            Self::EpochNotFound(id) => write!(f, "epoch {id} not found in arena allocator"),
            Self::InsufficientSpace => {
                write!(f, "arena chunk has insufficient space for allocation")
            }
            Self::InvalidAlignment(align) => {
                write!(f, "alignment must be a non-zero power of two, got {align}")
            }
        }
    }
}

impl std::error::Error for AllocError {}

impl From<AllocError> for crate::Error {
    fn from(e: AllocError) -> Self {
        match e {
            AllocError::OutOfMemory | AllocError::InsufficientSpace => {
                crate::Error::Storage(crate::utils::error::StorageError::Full)
            }
            AllocError::EpochNotFound(id) => {
                crate::Error::Internal(format!("epoch {id} not found in arena allocator"))
            }
            AllocError::InvalidAlignment(align) => crate::Error::Internal(format!(
                "alignment must be a non-zero power of two, got {align}"
            )),
        }
    }
}

/// A memory chunk in the arena.
struct Chunk {
    /// Pointer to the start of the chunk.
    ptr: NonNull<u8>,
    /// Total capacity of the chunk.
    capacity: usize,
    /// Current allocation offset.
    offset: AtomicUsize,
}

impl Chunk {
    /// Creates a new chunk with the given capacity.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if the system allocator fails.
    fn new(capacity: usize) -> Result<Self, AllocError> {
        if capacity > u32::MAX as usize {
            return Err(AllocError::OutOfMemory);
        }
        let layout = Layout::from_size_align(capacity, 16).map_err(|_| AllocError::OutOfMemory)?;
        // SAFETY: We're allocating a valid layout
        let ptr = unsafe { alloc(layout) };
        let ptr = NonNull::new(ptr).ok_or(AllocError::OutOfMemory)?;

        Ok(Self {
            ptr,
            capacity,
            offset: AtomicUsize::new(0),
        })
    }

    /// Tries to allocate `size` bytes with the given alignment.
    /// Returns None if there's not enough space.
    fn try_alloc(&self, size: usize, align: usize) -> Option<NonNull<u8>> {
        self.try_alloc_with_offset(size, align).map(|(_, ptr)| ptr)
    }

    /// Tries to allocate `size` bytes with the given alignment.
    /// Returns (offset, ptr) where offset is the aligned offset within this chunk.
    /// Returns None if there's not enough space.
    fn try_alloc_with_offset(&self, size: usize, align: usize) -> Option<(u32, NonNull<u8>)> {
        // Alignment must be a power of two; checked_sub handles align == 0 below,
        // but non-power-of-two values produce invalid bitmasks silently.
        debug_assert!(
            align.is_power_of_two(),
            "alignment must be a power of two, got {align}"
        );
        let base_addr = self.ptr.as_ptr() as usize;
        loop {
            let current = self.offset.load(Ordering::Relaxed);

            // Align the absolute address (base + offset), then convert back to an
            // offset. This is correct for any requested alignment, even when it
            // exceeds the chunk's own 16-byte alignment.
            let align_mask = align.checked_sub(1)?;
            let current_addr = base_addr.checked_add(current)?;
            let aligned_addr = current_addr.checked_add(align_mask)? & !align_mask;
            let aligned = aligned_addr - base_addr;
            let new_offset = aligned.checked_add(size)?;

            if new_offset > self.capacity {
                return None;
            }

            // Try to reserve the space
            match self.offset.compare_exchange_weak(
                current,
                new_offset,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    // SAFETY: We've reserved this range exclusively
                    let ptr = unsafe { self.ptr.as_ptr().add(aligned) };
                    // reason: aligned <= capacity, and chunk sizes are well under u32::MAX (4 GiB)
                    #[allow(clippy::cast_possible_truncation)]
                    return Some((aligned as u32, NonNull::new(ptr)?));
                }
                Err(_) => continue, // Retry
            }
        }
    }

    /// Returns the amount of memory used in this chunk.
    fn used(&self) -> usize {
        self.offset.load(Ordering::Relaxed)
    }
}

impl Drop for Chunk {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.capacity, 16).expect("Invalid layout");
        // SAFETY: We allocated this memory with the same layout
        unsafe { dealloc(self.ptr.as_ptr(), layout) };
    }
}

// SAFETY: Chunk uses atomic operations for thread-safe allocation
unsafe impl Send for Chunk {}
unsafe impl Sync for Chunk {}

/// A single epoch's memory arena.
///
/// Allocates by bumping a pointer forward - extremely fast. You can't free
/// individual allocations; instead, drop the whole arena when the epoch
/// is no longer needed.
///
/// Thread-safe: multiple threads can allocate concurrently using atomics.
///
/// # Addressing (`tiered-storage`)
///
/// TraceDecay patch. Every chunk in `chunks` is exactly `chunk_size` bytes,
/// which lets [`Arena::alloc_value_with_offset`] hand out a *flat* address —
/// `chunk_index * chunk_size + offset_in_chunk` — instead of a raw offset into
/// one chunk. That is the whole reason `oversized` exists: a single allocation
/// too large for a standard chunk still needs somewhere to live, but letting it
/// sit in `chunks` would break the uniform stride the flat address depends on.
pub struct Arena {
    /// The epoch this arena belongs to.
    epoch: EpochId,
    /// Uniformly sized memory chunks, each exactly `chunk_size` bytes.
    ///
    /// Append-only for the arena's lifetime: a chunk's index in this vector is
    /// half of every flat address handed out by `alloc_value_with_offset`, so
    /// entries are never removed or reordered.
    chunks: RwLock<Vec<Chunk>>,
    /// Chunks larger than `chunk_size`, each created for one allocation that
    /// no standard chunk could hold. Kept out of `chunks` to preserve the
    /// uniform stride; still allocated from and still counted by `stats`.
    oversized: RwLock<Vec<Chunk>>,
    /// Default chunk size for new allocations.
    chunk_size: usize,
    /// Total bytes allocated.
    total_allocated: AtomicUsize,
}

impl Arena {
    /// Creates a new arena for the given epoch.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if the initial chunk allocation fails.
    pub fn new(epoch: EpochId) -> Result<Self, AllocError> {
        Self::with_chunk_size(epoch, DEFAULT_CHUNK_SIZE)
    }

    /// Creates a new arena with a custom chunk size.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if the initial chunk allocation fails.
    pub fn with_chunk_size(epoch: EpochId, chunk_size: usize) -> Result<Self, AllocError> {
        let initial_chunk = Chunk::new(chunk_size)?;
        Ok(Self {
            epoch,
            chunks: RwLock::new(vec![initial_chunk]),
            oversized: RwLock::new(Vec::new()),
            chunk_size,
            total_allocated: AtomicUsize::new(chunk_size),
        })
    }

    /// Returns the epoch this arena belongs to.
    #[must_use]
    pub fn epoch(&self) -> EpochId {
        self.epoch
    }

    /// Allocates `size` bytes with the given alignment.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if a new chunk is needed and
    /// the system allocator fails.
    pub fn alloc(&self, size: usize, align: usize) -> Result<NonNull<u8>, AllocError> {
        if align == 0 || !align.is_power_of_two() {
            return Err(AllocError::InvalidAlignment(align));
        }
        // First try to allocate from existing chunks
        {
            let chunks = self.chunks.read();
            for chunk in chunks.iter().rev() {
                if let Some(ptr) = chunk.try_alloc(size, align) {
                    return Ok(ptr);
                }
            }
        }

        // Then the oversized chunks. Before the flat-addressing patch these
        // lived in `chunks` and were scanned by the loop above; keep using
        // their leftover space rather than growing the arena again.
        {
            let oversized = self.oversized.read();
            for chunk in oversized.iter().rev() {
                if let Some(ptr) = chunk.try_alloc(size, align) {
                    return Ok(ptr);
                }
            }
        }

        // Need a new chunk
        self.alloc_new_chunk(size, align)
    }

    /// Allocates a value of type T.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if allocation fails.
    pub fn alloc_value<T>(&self, value: T) -> Result<&mut T, AllocError> {
        let ptr = self.alloc(std::mem::size_of::<T>(), std::mem::align_of::<T>())?;
        // SAFETY: We've allocated the correct size and alignment
        Ok(unsafe {
            let typed_ptr = ptr.as_ptr() as *mut T;
            typed_ptr.write(value);
            &mut *typed_ptr
        })
    }

    /// Allocates a slice of values.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if allocation fails.
    pub fn alloc_slice<T: Copy>(&self, values: &[T]) -> Result<&mut [T], AllocError> {
        if values.is_empty() {
            return Ok(&mut []);
        }

        let size = std::mem::size_of::<T>()
            .checked_mul(values.len())
            .ok_or(AllocError::OutOfMemory)?;
        let align = std::mem::align_of::<T>();
        let ptr = self.alloc(size, align)?;

        // SAFETY: We've allocated the correct size and alignment
        Ok(unsafe {
            let typed_ptr = ptr.as_ptr() as *mut T;
            std::ptr::copy_nonoverlapping(values.as_ptr(), typed_ptr, values.len());
            std::slice::from_raw_parts_mut(typed_ptr, values.len())
        })
    }

    /// Splits a flat arena offset into `(chunk index, offset within chunk)`.
    ///
    /// TraceDecay patch. The `u32` an epoch hands out is a flat address over
    /// the arena's uniform chunks, not an offset into a single chunk, so every
    /// reader has to divide by the stride before touching memory.
    #[cfg(feature = "tiered-storage")]
    #[inline]
    fn split_offset(&self, offset: u32) -> (usize, usize) {
        debug_assert!(self.chunk_size > 0, "arena chunk size must be non-zero");
        let flat = offset as usize;
        (flat / self.chunk_size, flat % self.chunk_size)
    }

    /// Appends one standard chunk, unless another thread got there first.
    ///
    /// `observed` is the chunk count the caller saw while holding only a read
    /// lock. If the vector has grown since, the caller's retry will find the
    /// new chunk and nothing is allocated here.
    #[cfg(feature = "tiered-storage")]
    fn grow_uniform_chunks(&self, observed: usize) -> Result<(), AllocError> {
        let mut chunks = self.chunks.write();
        if chunks.len() != observed {
            return Ok(());
        }

        // A flat address is a u32, so the arena can address at most 4 GiB of
        // uniform chunks per epoch. Past that, report it instead of handing
        // back an address that would alias an earlier record.
        let max_chunks = (1_u64 << 32) / self.chunk_size as u64;
        if chunks.len() as u64 >= max_chunks {
            return Err(AllocError::InsufficientSpace);
        }

        let chunk = Chunk::new(self.chunk_size)?;
        self.total_allocated
            .fetch_add(self.chunk_size, Ordering::Relaxed);
        chunks.push(chunk);
        Ok(())
    }

    /// Allocates a value and returns its flat offset within the epoch's arena.
    ///
    /// This is used by tiered storage to store values in the arena and track
    /// their locations via compact u32 offsets in `HotVersionRef`.
    ///
    /// TraceDecay patch. Upstream 0.5.42 only ever allocated into
    /// `chunks.first()`, which capped an epoch at one `chunk_size` chunk —
    /// about 32 Ki 32-byte node records at the default 1 MiB — and then failed
    /// every later insert. The returned `u32` is now a flat address over all of
    /// the arena's uniform chunks (`chunk_index * chunk_size + offset`), so an
    /// epoch spans as many chunks as it needs, up to the 4 GiB a `u32` can
    /// address. Chunks are appended and never moved, so an address stays valid
    /// for the arena's lifetime exactly as before.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::InsufficientSpace` if the value is too large to sit
    /// inside a single chunk, or if the epoch has exhausted the 4 GiB flat
    /// address space. Returns `AllocError::InvalidAlignment` if `T`'s alignment
    /// does not divide the chunk size, because a flat address only preserves
    /// alignment when the stride is a multiple of it.
    /// Returns `AllocError::OutOfMemory` if a new chunk cannot be allocated.
    #[cfg(feature = "tiered-storage")]
    pub fn alloc_value_with_offset<T>(&self, value: T) -> Result<(u32, &mut T), AllocError> {
        let size = std::mem::size_of::<T>();
        let align = std::mem::align_of::<T>();

        if self.chunk_size == 0 || size.saturating_add(align) > self.chunk_size {
            // A value that cannot fit a chunk has nowhere to go: an allocation
            // may not straddle two chunks, or the flat address would not
            // describe it.
            return Err(AllocError::InsufficientSpace);
        }
        if !self.chunk_size.is_multiple_of(align) {
            return Err(AllocError::InvalidAlignment(align));
        }

        loop {
            let (observed, allocated) = {
                let chunks = self.chunks.read();
                let observed = chunks.len();
                // Bump into the newest chunk. Older ones are full by
                // construction, so there is nothing to scan.
                let allocated = chunks
                    .last()
                    .and_then(|chunk| chunk.try_alloc_with_offset(size, align))
                    .map(|(offset, ptr)| (observed - 1, offset, ptr));
                (observed, allocated)
            };

            if let Some((index, offset, ptr)) = allocated {
                let flat = (index as u64) * (self.chunk_size as u64) + u64::from(offset);
                let flat = u32::try_from(flat).map_err(|_| AllocError::InsufficientSpace)?;

                // SAFETY: We've allocated the correct size and alignment
                return Ok(unsafe {
                    let typed_ptr = ptr.as_ptr().cast::<T>();
                    typed_ptr.write(value);
                    (flat, &mut *typed_ptr)
                });
            }

            self.grow_uniform_chunks(observed)?;
        }
    }

    /// Reads a value at the given flat offset in the epoch's arena.
    ///
    /// # Safety
    ///
    /// - The offset must have been returned by a previous `alloc_value_with_offset` call
    /// - The type T must match what was stored at that offset
    /// - The arena must not have been dropped
    ///
    /// # Panics
    ///
    /// Panics if the offset does not name a live allocation in this arena.
    #[cfg(feature = "tiered-storage")]
    pub unsafe fn read_at<T>(&self, offset: u32) -> &T {
        let (index, in_chunk) = self.split_offset(offset);
        let chunks = self.chunks.read();
        let chunk = chunks.get(index).unwrap_or_else(|| {
            panic!(
                "read_at: offset {} names chunk {} but the arena has {}",
                offset,
                index,
                chunks.len()
            )
        });

        assert!(
            in_chunk + std::mem::size_of::<T>() <= chunk.used(),
            "read_at: offset {} + size_of::<{}>() = {} exceeds chunk used bytes {}",
            offset,
            std::any::type_name::<T>(),
            in_chunk + std::mem::size_of::<T>(),
            chunk.used()
        );
        assert!(
            in_chunk.is_multiple_of(std::mem::align_of::<T>()),
            "read_at: offset {} is not aligned for {} (alignment {})",
            offset,
            std::any::type_name::<T>(),
            std::mem::align_of::<T>()
        );

        // SAFETY: Caller guarantees offset is valid and T matches stored type
        unsafe {
            let ptr = chunk.ptr.as_ptr().add(in_chunk).cast::<T>();
            &*ptr
        }
    }

    /// Reads a value mutably at the given flat offset in the epoch's arena.
    ///
    /// # Safety
    ///
    /// - The offset must have been returned by a previous `alloc_value_with_offset` call
    /// - The type T must match what was stored at that offset
    /// - The arena must not have been dropped
    /// - No other references to this value may exist
    ///
    /// # Panics
    ///
    /// Panics if the offset does not name a live allocation in this arena.
    #[cfg(feature = "tiered-storage")]
    pub unsafe fn read_at_mut<T>(&self, offset: u32) -> &mut T {
        let (index, in_chunk) = self.split_offset(offset);
        let chunks = self.chunks.read();
        let chunk = chunks.get(index).unwrap_or_else(|| {
            panic!(
                "read_at_mut: offset {} names chunk {} but the arena has {}",
                offset,
                index,
                chunks.len()
            )
        });

        assert!(
            in_chunk + std::mem::size_of::<T>() <= chunk.capacity,
            "read_at_mut: offset {} + size_of::<{}>() = {} exceeds chunk capacity {}",
            offset,
            std::any::type_name::<T>(),
            in_chunk + std::mem::size_of::<T>(),
            chunk.capacity
        );
        assert!(
            in_chunk.is_multiple_of(std::mem::align_of::<T>()),
            "read_at_mut: offset {} is not aligned for {} (alignment {})",
            offset,
            std::any::type_name::<T>(),
            std::mem::align_of::<T>()
        );

        // SAFETY: Caller guarantees offset is valid, T matches, and no aliasing
        unsafe {
            let ptr = chunk.ptr.as_ptr().add(in_chunk).cast::<T>();
            &mut *ptr
        }
    }

    /// Allocates a new chunk and performs the allocation.
    fn alloc_new_chunk(&self, size: usize, align: usize) -> Result<NonNull<u8>, AllocError> {
        let required = size.checked_add(align).ok_or(AllocError::OutOfMemory)?;
        let chunk_size = self.chunk_size.max(required);
        let chunk = Chunk::new(chunk_size)?;

        self.total_allocated
            .fetch_add(chunk_size, Ordering::Relaxed);

        // The chunk was sized to fit this allocation, so this cannot fail.
        let ptr = chunk
            .try_alloc(size, align)
            .expect("fresh chunk sized to fit");

        // A chunk that had to be grown past `chunk_size` goes to `oversized`,
        // so that every entry in `chunks` keeps the uniform stride the flat
        // `alloc_value_with_offset` addressing is built on.
        if chunk_size == self.chunk_size {
            self.chunks.write().push(chunk);
        } else {
            self.oversized.write().push(chunk);
        }

        Ok(ptr)
    }

    /// Returns the total memory allocated by this arena.
    #[must_use]
    pub fn total_allocated(&self) -> usize {
        self.total_allocated.load(Ordering::Relaxed)
    }

    /// Returns the total memory used (not just allocated capacity).
    #[must_use]
    pub fn total_used(&self) -> usize {
        let standard: usize = self.chunks.read().iter().map(Chunk::used).sum();
        let oversized: usize = self.oversized.read().iter().map(Chunk::used).sum();
        standard + oversized
    }

    /// Returns statistics about this arena.
    ///
    /// `chunk_count` and `total_used` cover both the uniform chunks and the
    /// oversized ones, so the numbers mean the same thing they did before the
    /// two lists were split apart.
    #[must_use]
    pub fn stats(&self) -> ArenaStats {
        let chunks = self.chunks.read();
        let oversized = self.oversized.read();
        ArenaStats {
            epoch: self.epoch,
            chunk_count: chunks.len() + oversized.len(),
            total_allocated: self.total_allocated.load(Ordering::Relaxed),
            total_used: chunks.iter().map(Chunk::used).sum::<usize>()
                + oversized.iter().map(Chunk::used).sum::<usize>(),
        }
    }
}

/// Statistics about an arena.
#[derive(Debug, Clone)]
pub struct ArenaStats {
    /// The epoch this arena belongs to.
    pub epoch: EpochId,
    /// Number of chunks allocated.
    pub chunk_count: usize,
    /// Total bytes allocated.
    pub total_allocated: usize,
    /// Total bytes used.
    pub total_used: usize,
}

/// Manages arenas across multiple epochs.
///
/// Use this to create new epochs, allocate in the current epoch, and
/// clean up old epochs when they're no longer needed.
pub struct ArenaAllocator {
    /// Map of epochs to arenas.
    arenas: RwLock<hashbrown::HashMap<EpochId, Arena>>,
    /// Current epoch.
    current_epoch: AtomicUsize,
    /// Default chunk size.
    chunk_size: usize,
}

impl ArenaAllocator {
    /// Creates a new arena allocator.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if the initial arena allocation fails.
    pub fn new() -> Result<Self, AllocError> {
        Self::with_chunk_size(DEFAULT_CHUNK_SIZE)
    }

    /// Creates a new arena allocator with a custom chunk size.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if the initial arena allocation fails.
    pub fn with_chunk_size(chunk_size: usize) -> Result<Self, AllocError> {
        let allocator = Self {
            arenas: RwLock::new(hashbrown::HashMap::new()),
            current_epoch: AtomicUsize::new(0),
            chunk_size,
        };

        // Create the initial epoch
        let epoch = EpochId::INITIAL;
        allocator
            .arenas
            .write()
            .insert(epoch, Arena::with_chunk_size(epoch, chunk_size)?);

        Ok(allocator)
    }

    /// Returns the current epoch.
    #[must_use]
    pub fn current_epoch(&self) -> EpochId {
        EpochId::new(self.current_epoch.load(Ordering::Acquire) as u64)
    }

    /// Creates a new epoch and returns its ID.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if the arena allocation fails.
    pub fn new_epoch(&self) -> Result<EpochId, AllocError> {
        let new_id = self.current_epoch.fetch_add(1, Ordering::AcqRel) as u64 + 1;
        let epoch = EpochId::new(new_id);

        let arena = Arena::with_chunk_size(epoch, self.chunk_size)?;
        self.arenas.write().insert(epoch, arena);

        Ok(epoch)
    }

    /// Gets the arena for a specific epoch.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::EpochNotFound` if the epoch doesn't exist.
    pub fn arena(
        &self,
        epoch: EpochId,
    ) -> Result<impl std::ops::Deref<Target = Arena> + '_, AllocError> {
        let arenas = self.arenas.read();
        if !arenas.contains_key(&epoch) {
            return Err(AllocError::EpochNotFound(epoch));
        }
        Ok(parking_lot::RwLockReadGuard::map(arenas, |arenas| {
            &arenas[&epoch]
        }))
    }

    /// Ensures an arena exists for the given epoch, creating it if necessary.
    /// Returns whether a new arena was created.
    ///
    /// # Errors
    ///
    /// Returns `AllocError::OutOfMemory` if a new arena allocation fails.
    #[cfg(feature = "tiered-storage")]
    pub fn ensure_epoch(&self, epoch: EpochId) -> Result<bool, AllocError> {
        // Fast path: check if epoch already exists
        {
            let arenas = self.arenas.read();
            if arenas.contains_key(&epoch) {
                return Ok(false);
            }
        }

        // Slow path: create the epoch
        let mut arenas = self.arenas.write();
        // Double-check after acquiring write lock
        if arenas.contains_key(&epoch) {
            return Ok(false);
        }

        let arena = Arena::with_chunk_size(epoch, self.chunk_size)?;
        arenas.insert(epoch, arena);
        Ok(true)
    }

    /// Gets or creates an arena for a specific epoch.
    ///
    /// # Errors
    ///
    /// Returns `AllocError` if the arena allocation fails.
    #[cfg(feature = "tiered-storage")]
    pub fn arena_or_create(
        &self,
        epoch: EpochId,
    ) -> Result<impl std::ops::Deref<Target = Arena> + '_, AllocError> {
        self.ensure_epoch(epoch)?;
        self.arena(epoch)
    }

    /// Allocates in the current epoch.
    ///
    /// # Errors
    ///
    /// Returns `AllocError` if allocation fails.
    ///
    /// # Panics
    ///
    /// Panics if the current epoch has no arena (should never happen in normal use).
    pub fn alloc(&self, size: usize, align: usize) -> Result<NonNull<u8>, AllocError> {
        let epoch = self.current_epoch();
        let arenas = self.arenas.read();
        arenas
            .get(&epoch)
            .expect("current epoch always exists")
            .alloc(size, align)
    }

    /// Drops an epoch, freeing all its memory.
    ///
    /// This should only be called when no readers are using this epoch.
    pub fn drop_epoch(&self, epoch: EpochId) {
        self.arenas.write().remove(&epoch);
    }

    /// Returns total memory allocated across all epochs.
    #[must_use]
    pub fn total_allocated(&self) -> usize {
        self.arenas
            .read()
            .values()
            .map(Arena::total_allocated)
            .sum()
    }
}

impl Default for ArenaAllocator {
    /// Creates a default arena allocator.
    ///
    /// # Panics
    ///
    /// Panics if the initial arena allocation fails (out of memory).
    fn default() -> Self {
        Self::new().expect("failed to allocate default arena")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arena_basic_allocation() {
        let arena = Arena::new(EpochId::INITIAL).unwrap();

        // Allocate some bytes
        let ptr1 = arena.alloc(100, 8).unwrap();
        let ptr2 = arena.alloc(200, 8).unwrap();

        // Pointers should be different
        assert_ne!(ptr1.as_ptr(), ptr2.as_ptr());
    }

    #[test]
    fn test_arena_value_allocation() {
        let arena = Arena::new(EpochId::INITIAL).unwrap();

        let value = arena.alloc_value(42u64).unwrap();
        assert_eq!(*value, 42);

        *value = 100;
        assert_eq!(*value, 100);
    }

    #[test]
    fn test_arena_slice_allocation() {
        let arena = Arena::new(EpochId::INITIAL).unwrap();

        let slice = arena.alloc_slice(&[1u32, 2, 3, 4, 5]).unwrap();
        assert_eq!(slice, &[1, 2, 3, 4, 5]);

        slice[0] = 10;
        assert_eq!(slice[0], 10);
    }

    #[test]
    fn test_arena_large_allocation() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 1024).unwrap();

        // Allocate something larger than the chunk size
        let _ptr = arena.alloc(2048, 8).unwrap();

        // Should have created a new chunk
        assert!(arena.stats().chunk_count >= 2);
    }

    #[test]
    fn test_arena_allocator_epochs() {
        let allocator = ArenaAllocator::new().unwrap();

        let epoch0 = allocator.current_epoch();
        assert_eq!(epoch0, EpochId::INITIAL);

        let epoch1 = allocator.new_epoch().unwrap();
        assert_eq!(epoch1, EpochId::new(1));

        let epoch2 = allocator.new_epoch().unwrap();
        assert_eq!(epoch2, EpochId::new(2));

        // Current epoch should be the latest
        assert_eq!(allocator.current_epoch(), epoch2);
    }

    #[test]
    fn test_arena_allocator_allocation() {
        let allocator = ArenaAllocator::new().unwrap();

        let ptr1 = allocator.alloc(100, 8).unwrap();
        let ptr2 = allocator.alloc(100, 8).unwrap();

        assert_ne!(ptr1.as_ptr(), ptr2.as_ptr());
    }

    #[test]
    fn test_arena_drop_epoch() {
        let allocator = ArenaAllocator::new().unwrap();

        let initial_mem = allocator.total_allocated();

        let epoch1 = allocator.new_epoch().unwrap();
        // Allocate some memory in the new epoch
        {
            let arena = allocator.arena(epoch1).unwrap();
            arena.alloc(10000, 8).unwrap();
        }

        let after_alloc = allocator.total_allocated();
        assert!(after_alloc > initial_mem);

        // Drop the epoch
        allocator.drop_epoch(epoch1);

        // Memory should decrease
        let after_drop = allocator.total_allocated();
        assert!(after_drop < after_alloc);
    }

    #[test]
    fn test_arena_stats() {
        let arena = Arena::with_chunk_size(EpochId::new(5), 4096).unwrap();

        let stats = arena.stats();
        assert_eq!(stats.epoch, EpochId::new(5));
        assert_eq!(stats.chunk_count, 1);
        assert_eq!(stats.total_allocated, 4096);
        assert_eq!(stats.total_used, 0);

        arena.alloc(100, 8).unwrap();
        let stats = arena.stats();
        assert!(stats.total_used >= 100);
    }
}

#[cfg(all(test, feature = "tiered-storage"))]
mod tiered_storage_tests {
    use super::*;

    #[test]
    // reason: size_of::<u64>() is 8, fits u32
    #[allow(clippy::cast_possible_truncation)]
    fn test_alloc_value_with_offset_basic() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        let (offset1, val1) = arena.alloc_value_with_offset(42u64).unwrap();
        let (offset2, val2) = arena.alloc_value_with_offset(100u64).unwrap();

        // First allocation should be at offset 0 (aligned)
        assert_eq!(offset1, 0);
        // Second allocation should be after the first
        assert!(offset2 > offset1);
        assert!(offset2 >= std::mem::size_of::<u64>() as u32);

        // Values should be correct
        assert_eq!(*val1, 42);
        assert_eq!(*val2, 100);

        // Mutation should work
        *val1 = 999;
        assert_eq!(*val1, 999);
    }

    #[test]
    fn test_read_at_basic() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        let (offset, _) = arena.alloc_value_with_offset(12345u64).unwrap();

        // Read it back
        // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
        let value: &u64 = unsafe { arena.read_at(offset) };
        assert_eq!(*value, 12345);
    }

    #[test]
    fn test_read_at_mut_basic() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        let (offset, _) = arena.alloc_value_with_offset(42u64).unwrap();

        // Read and modify
        // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
        let value: &mut u64 = unsafe { arena.read_at_mut(offset) };
        assert_eq!(*value, 42);
        *value = 100;

        // Verify modification persisted
        // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
        let value: &u64 = unsafe { arena.read_at(offset) };
        assert_eq!(*value, 100);
    }

    #[test]
    fn test_alloc_value_with_offset_struct() {
        #[derive(Debug, Clone, PartialEq)]
        struct TestNode {
            id: u64,
            name: [u8; 32],
            value: i32,
        }

        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        let node = TestNode {
            id: 12345,
            name: [b'A'; 32],
            value: -999,
        };

        let (offset, stored) = arena.alloc_value_with_offset(node.clone()).unwrap();
        assert_eq!(stored.id, 12345);
        assert_eq!(stored.value, -999);

        // Read it back
        // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
        let read: &TestNode = unsafe { arena.read_at(offset) };
        assert_eq!(read.id, node.id);
        assert_eq!(read.name, node.name);
        assert_eq!(read.value, node.value);
    }

    #[test]
    fn test_alloc_value_with_offset_alignment() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        // Allocate a byte first to potentially misalign
        let (offset1, _) = arena.alloc_value_with_offset(1u8).unwrap();
        assert_eq!(offset1, 0);

        // Now allocate a u64 which requires 8-byte alignment
        let (offset2, val) = arena.alloc_value_with_offset(42u64).unwrap();

        // offset2 should be 8-byte aligned
        assert_eq!(offset2 % 8, 0);
        assert_eq!(*val, 42);
    }

    #[test]
    fn test_alloc_value_with_offset_multiple() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        let mut offsets = Vec::new();
        for i in 0..100u64 {
            let (offset, val) = arena.alloc_value_with_offset(i).unwrap();
            offsets.push(offset);
            assert_eq!(*val, i);
        }

        // All offsets should be unique and in ascending order
        for window in offsets.windows(2) {
            assert!(window[0] < window[1]);
        }

        // Read all values back
        for (i, offset) in offsets.iter().enumerate() {
            // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
            let val: &u64 = unsafe { arena.read_at(*offset) };
            assert_eq!(*val, i as u64);
        }
    }

    #[test]
    fn test_arena_allocator_with_offset() {
        let allocator = ArenaAllocator::with_chunk_size(4096).unwrap();

        let epoch = allocator.current_epoch();
        let arena = allocator.arena(epoch).unwrap();

        let (offset, val) = arena.alloc_value_with_offset(42u64).unwrap();
        assert_eq!(*val, 42);

        // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
        let read: &u64 = unsafe { arena.read_at(offset) };
        assert_eq!(*read, 42);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "exceeds chunk used bytes")]
    fn test_read_at_out_of_bounds() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();
        let (_offset, _) = arena.alloc_value_with_offset(42u64).unwrap();

        // Read way past the allocated region: should panic in debug
        // SAFETY: intentionally invalid offset to test debug assertion
        unsafe {
            let _: &u64 = arena.read_at(4000);
        }
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "is not aligned")]
    fn test_read_at_misaligned() {
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();
        // Allocate a u8 at offset 0
        let (_offset, _) = arena.alloc_value_with_offset(0xFFu8).unwrap();
        // Also allocate some bytes so offset 1 is within used range
        let _ = arena.alloc_value_with_offset(0u64).unwrap();

        // Try to read a u64 at offset 1 (misaligned for u64)
        // SAFETY: intentionally misaligned offset to test debug assertion
        unsafe {
            let _: &u64 = arena.read_at(1);
        }
    }

    #[test]
    #[cfg(not(miri))] // parking_lot uses integer-to-pointer casts incompatible with Miri strict provenance
    fn test_concurrent_read_stress() {
        use std::sync::Arc;

        let arena = Arc::new(Arena::with_chunk_size(EpochId::INITIAL, 1024 * 1024).unwrap());
        let num_threads = 8;
        let values_per_thread = 1000;

        // Each thread allocates values and records offsets
        let mut all_offsets = Vec::new();
        for t in 0..num_threads {
            let base = (t * values_per_thread) as u64;
            let mut offsets = Vec::with_capacity(values_per_thread);
            for i in 0..values_per_thread as u64 {
                let (offset, _) = arena.alloc_value_with_offset(base + i).unwrap();
                offsets.push(offset);
            }
            all_offsets.push(offsets);
        }

        // Now read all values back concurrently from multiple threads
        let mut handles = Vec::new();
        for (t, offsets) in all_offsets.into_iter().enumerate() {
            let arena = Arc::clone(&arena);
            let base = (t * values_per_thread) as u64;
            handles.push(std::thread::spawn(move || {
                for (i, offset) in offsets.iter().enumerate() {
                    // SAFETY: offset was returned by alloc_value_with_offset for the same type and arena
                    let val: &u64 = unsafe { arena.read_at(*offset) };
                    assert_eq!(*val, base + i as u64);
                }
            }));
        }

        for handle in handles {
            handle.join().expect("Thread panicked");
        }
    }

    #[test]
    fn test_alloc_value_with_offset_spans_chunks() {
        // TraceDecay patch: upstream 0.5.42 returned InsufficientSpace here,
        // because it only ever allocated into the arena's first chunk. A full
        // chunk now grows the arena instead, and the flat offset carries the
        // chunk index.
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 64).unwrap();

        let (first, _) = arena.alloc_value_with_offset([1u8; 48]).unwrap();
        assert_eq!(first, 0);

        // Does not fit the remaining 16 bytes, so it lands in a second chunk
        // whose flat address starts one stride in.
        let (second, _) = arena.alloc_value_with_offset([2u8; 32]).unwrap();
        assert_eq!(second, 64);
        assert_eq!(arena.stats().chunk_count, 2);

        // SAFETY: both offsets were returned by alloc_value_with_offset for these types
        unsafe {
            assert_eq!(*arena.read_at::<[u8; 48]>(first), [1u8; 48]);
            assert_eq!(*arena.read_at::<[u8; 32]>(second), [2u8; 32]);
        }
    }

    #[test]
    fn test_alloc_value_with_offset_value_larger_than_chunk() {
        // A single value that cannot fit inside one chunk still has nowhere to
        // go: an allocation may not straddle two chunks. This must be a typed
        // error, never a panic.
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 64).unwrap();

        let result = arena.alloc_value_with_offset([0u8; 128]);
        assert!(matches!(result, Err(AllocError::InsufficientSpace)));
    }

    #[test]
    fn test_alloc_value_with_offset_exhausts_flat_address_space() {
        // The flat address is a u32, so an epoch tops out at 4 GiB. With a
        // 1 GiB stride that is four chunks -- reached here without allocating
        // anything, because `chunks` is only grown when a chunk fills up.
        let arena = Arena::with_chunk_size(EpochId::INITIAL, 1 << 30).unwrap();
        for _ in 0..3 {
            arena.grow_uniform_chunks(arena.stats().chunk_count).unwrap();
        }
        assert_eq!(arena.stats().chunk_count, 4);

        assert!(matches!(
            arena.grow_uniform_chunks(4),
            Err(AllocError::InsufficientSpace)
        ));
    }

    #[test]
    // reason: record counts here are far below u32::MAX
    #[allow(clippy::cast_possible_truncation)]
    fn test_alloc_value_with_offset_beyond_one_chunk_of_records() {
        // Regression for the defect this fork exists to fix. A 32-byte record
        // in a 1 MiB chunk gives 32 Ki records; upstream 0.5.42 failed every
        // allocation past that, which is what made grafeo's tiered-storage
        // feature unusable for a real graph. Allocate comfortably more than one
        // chunk's worth and read every one of them back.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        #[repr(C)]
        struct Record {
            id: u64,
            epoch: u64,
            flags: u64,
            reserved: u64,
        }
        assert_eq!(std::mem::size_of::<Record>(), 32);

        const CHUNK: usize = 1024 * 1024;
        const COUNT: usize = (CHUNK / 32) * 3 + 17;

        let arena = Arena::with_chunk_size(EpochId::INITIAL, CHUNK).unwrap();

        let mut offsets = Vec::with_capacity(COUNT);
        for i in 0..COUNT {
            let (offset, _) = arena
                .alloc_value_with_offset(Record {
                    id: i as u64,
                    epoch: 7,
                    flags: 0xDEAD_BEEF,
                    reserved: 0,
                })
                .expect("allocation past the first chunk must succeed");
            offsets.push(offset);
        }

        // More than one chunk was needed, and every address is distinct and
        // increasing, so no record aliased another.
        assert!(arena.stats().chunk_count >= 4);
        for window in offsets.windows(2) {
            assert!(window[0] < window[1]);
        }

        for (i, offset) in offsets.iter().enumerate() {
            // SAFETY: offset was returned by alloc_value_with_offset for this type and arena
            let record: &Record = unsafe { arena.read_at(*offset) };
            assert_eq!(record.id, i as u64);
            assert_eq!(record.flags, 0xDEAD_BEEF);
        }
    }

    #[test]
    fn test_multi_type_interleaved() {
        #[derive(Debug, Clone, PartialEq)]
        #[repr(C)]
        struct Record {
            id: u64,
            flags: u32,
            weight: f32,
        }

        let arena = Arena::with_chunk_size(EpochId::INITIAL, 4096).unwrap();

        // Interleave different types
        let (off_u8, _) = arena.alloc_value_with_offset(0xAAu8).unwrap();
        let (off_u32, _) = arena.alloc_value_with_offset(0xBBBBu32).unwrap();
        let (off_u64, _) = arena.alloc_value_with_offset(0xCCCCCCCCu64).unwrap();
        let (off_rec, _) = arena
            .alloc_value_with_offset(Record {
                id: 42,
                flags: 0xFF,
                weight: std::f32::consts::PI,
            })
            .unwrap();

        // Read them all back
        // SAFETY: all offsets were returned by alloc_value_with_offset for matching types and arena
        unsafe {
            assert_eq!(*arena.read_at::<u8>(off_u8), 0xAA);
            assert_eq!(*arena.read_at::<u32>(off_u32), 0xBBBB);
            assert_eq!(*arena.read_at::<u64>(off_u64), 0xCCCCCCCC);

            let rec: &Record = arena.read_at(off_rec);
            assert_eq!(rec.id, 42);
            assert_eq!(rec.flags, 0xFF);
            assert!((rec.weight - std::f32::consts::PI).abs() < 0.001);
        }
    }
}
