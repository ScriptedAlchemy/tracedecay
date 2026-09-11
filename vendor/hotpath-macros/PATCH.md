# Hotpath async body pinning

This directory contains the hotpath-macros 0.24.0 crate from
[pawurb/hotpath-rs v0.24.0](https://github.com/pawurb/hotpath-rs/tree/91c48b21f0cb0c33d37c2241ada042d2211dc38f/crates/hotpath-macros).
The upstream MIT license is preserved in LICENSE.txt. Cargo.toml uses the
published crate's resolved dependency versions and disables publishing.
The runtime remains the unmodified crates.io hotpath 0.24.0 crate.

The only source change pins the generated async function body before passing
it to the four measurement helpers (timing, result logging, future tracking,
and their combination). The helpers borrow the pinned body instead of owning
another inline copy of its future state. No heap allocation is introduced;
feature-off expansion and all measurement labels remain unchanged.

With four nested measured functions retaining a 1024-byte leaf, the original
macro produced frames of 3216, 9816, 29616 and 89016 bytes. The patch yields
1192, 1368, 1544 and 1720 bytes; feature-off sizes are 1025 through 1028 bytes.
The regression lives with TraceDecay's existing future-size probes.

Upstream 0.25.1 and main at 8dd9b7079b65dd4a8e96dcb590f249d7908f28b0
still forward the body by value. Remove this directory and the Cargo patch
when a maintained release provides equivalent pinning and passes the probes.
