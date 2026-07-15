# Next delivery: sanitized observation capture

PR4's production transcript-store boundary is implemented. The next slice moves
one provider's parsed transcript records into an immutable, sanitized
observation path behind the same daemon-owned database authority.

## Scope

- Define ordinary typed capture inputs, sanitizer receipts, source identity, and
  idempotency keys; do not add a macro DSL or parallel metadata model.
- Route one existing provider path end to end through capture, persistence, and
  replay while preserving the root binary and V1 behavior.
- Reuse the open `GlobalDb` authority and `tracedecay-store` boundary. Do not
  open a second database or add local, in-memory, source-adjacent, or recovery
  fallback writers.
- Persist project observations in the canonical project-wide store shared by
  all branches and worktrees; keep account-wide user sessions in the
  user/profile store. Branch/worktree scope applies only to code-graph indexes.
- Resolve worktrees through the project registry and Git common directory, and
  fail closed when the required project or user-store authority is unavailable.
- Persist only sanitized observations; malformed, partial, duplicate, and
  restart behavior must remain explicit and retry-safe.
- Add direct behavior tests for secret rejection/redaction, idempotent replay,
  crash-before-commit, suffix resume, stale-owner rejection, and restart.

## Done when

- One real provider path produces replayable sanitized observations through the
  production daemon/store authority.
- Crash and retry tests prove no duplicate observation, skipped suffix, advanced
  offset without data, or unsanitized durable payload.
- Linux and Windows checks pass without inventory generators, plan parsers,
  workflow executors, or generated architecture views.
