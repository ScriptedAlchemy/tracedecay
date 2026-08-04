# Other Git refs

Use the daemon's read-only Git/code operations to search or compare exact ref
snapshots without switching the checkout.

- `tracedecay_branch_list` reports refs/snapshots known to the canonical
  project authority.
- `tracedecay_branch_search` searches one exact snapshot.
- `tracedecay_branch_diff` compares exact base/head snapshots.

Branch/ref/worktree are provenance and selectors in one project-wide Grafeo
store. Never create a branch database, ask the user to track a branch into a
separate store, or accept an ancestor fallback. Preserve commit, worktree,
generation, freshness, coverage, and typed absent/indexing/stale state.
