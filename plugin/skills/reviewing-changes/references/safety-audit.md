# Interpreting safety and dead-code evidence

Scope panic/unsafe, unfinished-work, redundancy, and test-risk evidence to the
requested review. Production panic reachability matters; test assertions and an
unsafe block's mere existence are not defects. Verify the actual failure mode
and callers before assigning severity.

Unmounted-file results mean no indexed build root reaches the file, not proof
that no runtime loader or external build can use it. Investigate that boundary
before deleting a whole file. Zero indexed callers similarly does not establish
that a public API, macro entry point, or external consumer is unused.

Body-similarity findings need semantic comparison; name-only and vector-only
signals cannot justify consolidation alone. Structural test links identify
candidate verification, not executed coverage. Verify changed behavior with the
real build or host journey that owns the relevant boundary.
