# A bound LIMIT or OFFSET is not reloaded when a subquery reruns (Turso 0.8.0)

When a nested query runs more than once in one statement, Turso keeps
counting its LIMIT and OFFSET down from where the previous run stopped,
provided the clause is any expression other than a numeric literal (`?1`,
`4 + 0 * ?1`). A literal LIMIT or OFFSET restarts on every run. SQLite
restarts both forms. Once a bound LIMIT counter reaches zero, later runs
return no rows. Once a bound OFFSET counter reaches zero, later runs skip
nothing.

## Repro

```sql
CREATE TABLE facts (obs TEXT NOT NULL, ord INTEGER NOT NULL);
INSERT INTO facts VALUES ('a', 1), ('a', 2), ('b', 3);

-- ?1 = 4
WITH m AS (
  SELECT obs, MIN(ord) AS first FROM facts GROUP BY obs ORDER BY first LIMIT ?1
)
SELECT f.ord, m.obs FROM facts f CROSS JOIN m
WHERE m.obs || '' = f.obs ORDER BY f.ord;

-- ?1 = 1, ?2 = 1
SELECT f.ord, COALESCE((SELECT g.obs FROM facts g WHERE g.ord >= f.ord
                        ORDER BY g.ord LIMIT ?1 OFFSET ?2), '-')
FROM facts f ORDER BY f.ord;
```

| Statement | SQLite | Turso 0.8.0 |
|---|---|---|
| grouped CTE, `LIMIT ?1` = 4 | (1,a) (2,a) (3,b) | (1,a) (2,a) |
| grouped CTE, `LIMIT ?1 OFFSET ?2` = 1, 1 | (3,b) | none |
| `UNION ALL` CTE, `LIMIT ?1 OFFSET ?2` = 1, 1 | (3,b) | none |
| `UNION ALL ... ORDER BY 2` CTE, `LIMIT ?1 OFFSET ?2` = 1, 1 | (3,b) | none |
| correlated scalar, `LIMIT ?1 OFFSET ?2` = 1, 1 | (1,a) (2,b) (3,-) | (1,a) (2,a) (3,b) |

The CTE runs once per outer row because the CROSS JOIN keeps it on the
inner side. Its join key is wrapped (`m.obs || ''`) because an indexable key
makes Turso materialize the CTE once into an ephemeral seek index, which
hides the defect. That is why a small table with a plain equality join did
not reproduce it. The conformance case
`//crates/tracedecay-sql:conformance`
`bound_limit_and_offset_restart_on_every_subquery_rescan` runs all five
statements on both engines.

`search_workflow_facts`
(`crates/tracedecay-sessions/src/runtime/store_access/sessions.rs`) shows the
same signature. Its grouped `matching_observations` CTE yields two
observations per run. If it runs once per outer fact row, a bound limit of 4
falls 4, 2, 0, so the third row, the only fact of the second observation,
finds no match. A limit of 8 leaves count for every run, and a narrower outer
column list changes the plan. Those are the three conditions under which
only the bound form at limit 4 with the full column list dropped a group.
The patch below makes the suite repro pass with the limit bound again.

## Root cause

`translate_expr` (`core/translate/expr/translator.rs:100`) opens a constant
span for every constant expression. A bound parameter is constant, so its
load is hoisted into the program's one-time init block. These counter loads
went through it:

- `init_limit`, `core/translate/emitter/mod.rs:1886` (LIMIT) and `:1921`
  (OFFSET), used by every simple and recursive SELECT.
- `emit_program_for_compound_select`, `core/translate/compound_select.rs:80`
  (LIMIT) and `:112` (OFFSET).
- `emit_compound_order_by`, `core/translate/compound_select.rs:1034` (LIMIT)
  and `:1061` (OFFSET).

The registers they fill are counters that `DecrJumpZero` and `IfPos`
decrement in place. A coroutine subquery on the inner side of a join is
restarted with `InitCoroutine` for every outer row
(`core/translate/main_loop/open.rs:195`), and a correlated subquery reruns
per row, but neither path reaches the hoisted load again. A literal takes
the separate `Insn::Integer` branch, which is never hoisted, so it reloads
on every run.

## Patch

`scripts/bazel/patches/turso-core-rescanned-limit-counters.patch`, applied
to `turso_core` through its `crate.annotation` in `MODULE.bazel`, translates
all six counter expressions with `translate_expr_no_constant_opt(...,
NoConstantOptReason::RegisterReuse)`. That reason already exists for
registers the program overwrites after loading them. The load now runs at
the start of each run, as the literal form does.

`search_workflow_facts` binds its limit again. The literal-limit workaround
is gone, and `//crates/tracedecay:session_suite`
`observation_workflow_projection::search_returns_one_row_per_observation_before_applying_limit`
guards the statement.

## Shapes that run once

An audit of every owned-store statement that binds LIMIT or OFFSET and
closes a nested query with it found these shapes besides the rescanned CTE
and correlated subquery above. Their nested query runs once per statement,
so the defect cannot reach them:

- `x IN (SELECT ... LIMIT ?)`, including `DELETE ... WHERE rowid IN (SELECT
  ... LIMIT ?)` (retention slices, ledger prune, preview-input purge, stack
  delivery promotion). A SELECT and a DELETE of this shape returned SQLite's
  rows on unpatched Turso, because Turso builds the IN set once.
- An uncorrelated `EXISTS (SELECT 1 FROM source LIMIT 1 OFFSET ?)`, in the
  session-temporal doctor probes.
- A subquery or CTE that is the only FROM item or the left side of a LEFT
  JOIN: `MAX(rowid) FROM (... LIMIT ?)`, the `page` and `bounded` window
  pipelines in lcm and session-temporal, and the project registry's
  `recent_projects`.
- The LIMIT of a recursive CTE, which `init_limit` loads once before the
  recursion starts.

The dashboard's `SESSION_PAGE_SQL` and `PAGE_EVENTS_SQL` join a bounded CTE
to a table by rowid or primary key, so the planner keeps the CTE outer. The
patch covers them if a plan ever puts the CTE inner.
