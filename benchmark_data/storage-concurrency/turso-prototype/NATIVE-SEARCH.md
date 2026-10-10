# Native search verification

Run the manual `//:native_search_probe` target in this standalone Bazel module
with the same interpreter and repository flags as `turso_probe`. It uses the
same SHA-pinned official `pyturso` 0.8.0 wheel and actual Rust/Tantivy engine.
The earlier six-scenario search command and emitted facts are saved in
[native-search-verification.txt](native-search-verification.txt) and
[native-search-results.json](native-search-results.json).
The historical strict secure-delete aggregate failed, as recorded in
[native-search-privacy-results.json](native-search-privacy-results.json). After
explicit approval of logical deletion, the current probe asserts that this
pinned engine does not implement the setting. No scrubbing guarantee is claimed.

The test enables `index_method,generated_columns`, installs all four native
search indexes from `SearchIndex::create_sql`, and uses full canonical indexed
base-table declarations: memory payloads, LCM raw messages, session occurrences,
and summary nodes. `native_search_schema.sql` preserves their columns, checks,
uniqueness, foreign-key declarations, and generated expressions. Referenced
identity tables are minimal fixtures. This is neither a full-schema admission
test nor a Rust storage adapter test. All files are temporary isolated data.

Thirty-two literal-term, phrase, boolean, quote/backslash escape, Unicode, and
literal-operator checks verify result identities and `QUERY INDEX METHOD fts`
plans. LCM body-only search supplies all five indexed columns and confines the
bound query to `index_text:(...)`; a separately asserted one-column call scans
the table instead. Positive native BM25 scores rank higher values first, and
the same content corpus verifies the LCM content weight of 10 against the
single-column index. Results are materialized before ranking and joining.

The test checks native indexing of inserts, updates on mutable tables, deletes,
read-your-writes, full rollback, stable MVCC snapshots across update/delete,
overlapping same-index writers on independent rows, and persistence through a
fresh process reopen with `integrity_check=ok`. The canonical memory payload
immutability trigger remains effective. Native query deadlines and cross-thread
interrupt stop long SQL; an interrupted indexed write is rolled back without
remaining rows or hits, and later indexed reads still succeed.

The pinned engine has material limitations, captured as strict negative probes:

- `alp*` returns no matches for `alpha`; `"alp"*` raises
  `PhrasePrefixRequiresAtLeastTwoTerms`. A phrase prefix such as `"alpha b"*`
  works. The approved native caller implementation uses quoted exact terms;
  the historical SQLite single-token prefix discovery behavior is lost.
- Quoted whitespace, punctuation, emoji-only text, and an empty quoted phrase
  return no hits while retaining an FTS index plan.
- Direct `ORDER BY fts_score DESC`, its alias/ordinal variants, and direct
  top-K can produce the wrong order. A `MATERIALIZED` hits CTE computes scores
  before sorting and returns the correct top-K in this fixture.
- A direct joined query can use the FTS index yet return zero scores.
  Materializing standalone native hits and scores before the join preserves
  scores. This must also be verified against final production caller SQL.
- Bounded primary-key, member join, and correlated `EXISTS` fixtures preserve
  indexed boolean filtering, including exclusion by `NOT`. They do not cover
  every final production join; standalone materialized hit rowids keep index
  grammar separate from canonical scope and keyset joins.
- A trigger that writes the same indexed table as its firing statement is
  rejected with `statement already has an open writer on this FTS index; a
  trigger cannot write the FTS-indexed table its firing statement is writing`.
  The failing statement leaves no rows or hits. Ordinary invariant triggers
  must be retained; this limitation does not authorize their deletion.

Actual Python calls to `set_authorizer`, `set_progress_handler`, and
`set_update_hook` raise `AttributeError`. This establishes the Python binding
surface only: the Rust core exposes progress callbacks used by the separate
native adapter. The Python binding's native timeout and interrupt APIs are
successfully exercised here.

Native fact/session ranking may select different candidates, including the
memory duplicate check's existing eight-candidate cap. Materialized hits must
enumerate indexed matches before authorization, ranking, and the result limit;
there is no unauthorized-hit pretruncate. This can consume more work and memory,
and final Rust callers and their execution limits need separate verification.

The required `PRAGMA secure_delete=ON` succeeds but both pre- and post-setting
readbacks return no rows. The pinned engine's unknown-pragma path silently
ignores unsupported names ([source](https://github.com/tursodatabase/turso/blob/2829ee1662bd01d79f60c2c170af684a2b50af86/core/translate/pragma.rs#L224)).
The historical strict aggregate therefore failed. The user subsequently
approved logical deletion without page scrubbing. The current negative probe
asserts empty readbacks, and native memory installation skips this setting.
This probe establishes no secure deletion guarantee for canonical pages or
Tantivy postings and does not authorize default engine replacement.

The search scenarios verify bounded behavior, including explicitly unsupported
cases. They do not establish FTS5 equivalence, completed caller migration,
throughput improvement, posting membership health, or crash/power-loss durability.
