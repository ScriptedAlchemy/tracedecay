# Arena rubric (judge and picker only; runners never see this)

Artifact: a whole-shape storage design package (design.md + sketch.rs) per arena/task.md.

Score each criterion 1 to 5 with one line of evidence (section or line in the candidate).

1. Incident closure. Each 2026-10-16 incident is impossible by construction, or bounded and observable: 153 GB unmeasured growth and every growth source in growth-sources-4099.md; the fail-closed projection freeze (and the #4094 process-lifetime TEMP-cache fault); the PASSIVE checkpoint ratchet and idle-WAL never checkpointing; native-worker reader waits (#4085) and sync ports parking writers; per-worktree/orphan stores never collected; the 24 h grant freeze; the daemon holding 112 GB of unlinked files unnoticed.
2. Interface depth. Small public port surface that hides the engine, encodes execution class (no caller can park a native worker), carries no SQL text, WAL budgets, wire/serde protocol enums, or engine error strings. Compared against today's census (19 store traits/143 methods, 24 sync Work ports/77 methods, closed-enum runtime protocol).
3. Partitioning and containment. Files split along failure domain, write cadence, retention class, and sharing scope with a justified table; derived data rebuildable without operator action; one bad row or file blocks only its own partition; total threads bounded independent of file count.
4. Cutover realism. Sequenced, independently verifiable PRs, riskiest unknown first; converges with no long-lived compatibility layer; honors the one-engine SQLite decision or argues against it with evidence; explicit beta upgrade path; deletes more than it adds.
5. Evidence fit. Claims about current code are correct (spot-check two per candidate); reuses existing canonical authorities where they are right (runtime-core async Connection, writer ledger, sealed generations, spool) and replaces them only with argument.
6. Simplicity. Fewest new concepts and crates; call chains traceable in three files or fewer; no shallow modules, information leakage, temporal decomposition, or pass-through layers (design-red-flags.md).
