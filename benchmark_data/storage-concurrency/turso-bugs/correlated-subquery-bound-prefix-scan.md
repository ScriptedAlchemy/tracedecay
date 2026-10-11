# A correlated subquery seeks only its bound-parameter prefix (Turso 0.8.0)

In a correlated subquery keyed by a bound parameter plus an outer column,
Turso seeks the parameter column and filters every row under it, even with
`INDEXED BY` naming the primary key. A literal in place of the parameter, or a
standalone statement with both values bound, seeks both columns. SQLite seeks
both columns in every form.

## Repro

```sql
CREATE TABLE observations (sequence INTEGER PRIMARY KEY, observation_id TEXT NOT NULL UNIQUE);
CREATE TABLE prov (projector_version TEXT NOT NULL, observation_id TEXT NOT NULL,
                   output_ordinal INTEGER NOT NULL,
                   PRIMARY KEY (projector_version, observation_id, output_ordinal));
-- 4000 observations, one prov row each, all projector_version 'v1'
SELECT o.sequence,
       (SELECT COUNT(*) FROM prov
        WHERE projector_version = ?1 AND observation_id = o.observation_id)
FROM observations AS o
WHERE o.sequence > 3984 AND o.sequence <= 4000;   -- ?1 = 'v1'
```

| spelling of the projector key | VM steps for 16 outer rows |
|---|---|
| `projector_version = ?1` | 108,291 |
| `... INDEXED BY sqlite_autoindex_prov_1 ...` | 108,291 |
| `projector_version = CAST(?1 AS TEXT)` | 108,291 |
| `projector_version = 'v1'` | about 250 |
| `projector_version = (SELECT ?1)` | 454 |

A two-column primary key, `(projector_version, observation_id)`, seeks both
columns with `?1` (323 steps), so the gap appears when the seek covers a
prefix of a longer key.

## Workaround

The resumed authority audit's page query in `tracedecay-global-db`
`schema_contract/invariants/audit.rs` reads the projector key as
`(SELECT ?1)` in its correlated counts. Before the change its work grew with
the store on native
(`resumed_authority_audit_work_follows_new_observations_not_the_store`).
