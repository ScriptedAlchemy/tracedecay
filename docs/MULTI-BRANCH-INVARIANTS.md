# Repository and worktree invariants

1. A registered repository has one project identity and one project graph
   authority across all linked worktrees.
2. Branch, ref, worktree, commit, PR, session, and agent identity are
   provenance/query selectors only.
3. Project facts and project sessions are project-wide and survive branch,
   worktree, ref, and agent deletion.
4. Every code/graph result names the exact generation and repository/worktree
   snapshot it represents.
5. No read falls back to an active checkout, default branch, base worktree, or
   another registered project.
6. A published graph generation is complete and validated; failure leaves the
   prior complete generation readable.
7. Content-addressed reuse requires complete parser/model/privacy/configuration
   identity and never reuses snapshot provenance.
8. One datum has one durable authority: Grafeo for graph/vector state, SQLite
   for relational/content/journal state, and holographic memory for fact
   content.
9. All writers are daemon-owned and fenced. Clients, hooks, tests, and
   transports do not open business stores directly.
10. Read operations do not synchronize, repair, retain, compact, or mutate
    access counters. They report stale/partial/unavailable/reset states.
11. Worktree/ref deletion may collect only unreachable derived generations; it
    cannot delete project facts, source evidence, receipts, or live snapshots.
12. Incompatible TraceDecay stores are reset/recreated, never migrated,
    backfilled, dual-written, copied, or adopted.
13. Historical host/repository data may be admitted as new sanitized V2
    capture; that is not database migration.
14. Doctor is read-only. Maintenance effects are separate authorized daemon
    operations with typed receipts and rollback/replay behavior.
15. Linked-worktree, multi-root, restart, cancellation, stale-authority,
    isolation, and deletion tests must exercise the production daemon route.
