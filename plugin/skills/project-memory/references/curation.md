# Curation authority and terminal evidence

`fact_store_curate` is the public semantic launcher for an agent-managed run.
Its caller supplies review bounds, not task identity, arbitrary operations,
validation, policy, or effect authority. The daemon collects evidence, validates,
and automatically applies supported operations within that run.

Inspect the run and advertised artifacts through supported automation views;
artifact retrieval verifies its published chain. Terminal status, applied and
rejected operations, and read-only fact verification establish the outcome.
A failed run may contain committed effects: report and reconcile those effects
before considering another launch. Dashboard launch and observation use this
same authority.

Direct add/update/supersede/remove operations are independent exact
administration, not a continuation of a curator run. Supersession preserves the
old fact by id and records its successor; permanent removal has no undo. Resolve
ambiguous deletion targets, but do not add confirmation for an already exact
instruction. Inspectors provide cited candidates; they do not inherit write
authority merely from being asked to review.

For an authorized request to remember a subject, research its evidence, reject
secrets and transient progress, search existing facts, and retain provenance and
uncertainty in accepted facts. Handle near-duplicate, conflict, and secret
rejections explicitly. Age alone does not justify lowering trust.
