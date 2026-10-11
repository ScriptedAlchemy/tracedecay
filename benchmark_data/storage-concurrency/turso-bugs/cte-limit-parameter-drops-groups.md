# A bound `LIMIT` inside a grouped CTE drops groups (Turso, tracedecay-sql native)

A CTE that groups, orders and ends in `LIMIT ?n` returns fewer groups than
its limit allows when the outer query joins back and selects many columns.
The same statement with the limit spelled as an integer literal returns every
group. SQLite returns every group in both forms.

## Repro

`session_suite`
`observation_workflow_projection::search_returns_one_row_per_observation_before_applying_limit`
under `--config=native` before the workaround. The profile store holds three
workflow facts in two observations (`a`: alpha, beta; `b`: follow-up). The
statement is `search_workflow_facts` in
`crates/tracedecay-sessions/src/runtime/store_access/sessions.rs`:

```sql
WITH matching_observations AS (
    SELECT w.provider, w.observation_id, MIN(w.item_order) AS first_item_order, ...
    FROM observation_workflow_facts w JOIN sessions s ON ...
    WHERE ... GROUP BY w.provider, w.observation_id HAVING ...
    ORDER BY item_order_missing, first_item_order, ...
    LIMIT ?8)
SELECT s.provider, s.session_id, ... (33 columns of sessions and workflow facts)
FROM matching_observations matched JOIN observation_workflow_facts w ON ...
JOIN sessions s ON ...
ORDER BY ...
```

Read through the registered profile snapshot (every other value a literal):

| LIMIT | outer columns | rows |
|---|---|---|
| `?1` bound to 4 | 33 | alpha, beta (observation `b` missing) |
| `4 + 0 * ?1` | 33 | alpha, beta |
| `4` literal | 33 | alpha, beta, follow-up |
| `?1` bound to 4 | 2 | alpha, beta, follow-up |
| `?1` bound to 8 | 33 | alpha, beta, follow-up |

Dropping the inner `ORDER BY`, the `HAVING`, or the outer `ORDER BY` does not
change the bound-limit result. A three-row in-memory table with the same CTE
shape and up to 34 outer columns did not reproduce it, so the trigger also
depends on the store's state; the suite test above is the reliable repro.

## Workaround

`search_workflow_facts` writes its fetch limit into the SQL as an integer
literal. The limit is a `usize` the function computes, never caller text.
