# #801 checkpoint

Lane: `agent/issue-801-c4e8a91f`
Worktree: `/home/zack/.cursor/worktrees/issue-801-c4e8a91f/tracedecay-3b667bf84334`
Base: `101072320287b9621050fa7699da972cff9f80bc`

## Landed

graph-db ID-only relation fan-out, per-epoch quarantine approval,
label-key cache, adjacency identity cache, cursor pages.

## RED → GREEN

- RED: `property_decodes=2048` on ID fan-out; second-call `label_universe_scans=1`; page-1 still hydrated 2048 properties.
- GREEN: 7/7 `paged_relation_ids` including 100k-star page 10/10; 46/46 `runtime_contract`.

## Remaining (out of this slice)

- `code-index-runtime` `relation_records` / `callers` still builds the full
  closure then paginates.
- Excluded by brief: eager Grafeo engine opens, lazy historical generation
  handles, LPG replay open cost.
