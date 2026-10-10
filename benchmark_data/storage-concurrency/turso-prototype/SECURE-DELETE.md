# Native secure-delete compatibility

Turso 0.8.0 at `2829ee1662bd01d79f60c2c170af684a2b50af86` does not
implement `PRAGMA secure_delete`. Its pinned `COMPAT.md` marks the setting
unsupported. The executed [privacy probe](native-search-privacy-results.json)
sets the pragma successfully but reads no value before or after setting it.
That success is an ignored setting, not evidence of physical deletion.

## Existing behavior that uses the setting

TraceDecay's SQLite memory schema enables the setting before installing payload
tables. It reads back `1` before admission, so an unknown
pragma cannot fabricate the guarantee. Native policy still returns `Unsupported` for the raw pragma. After explicit
approval of logical deletion, native schema installation skips that setting.
The [Rust test](rust-verification.txt) verifies fresh installation, reopen, and
honest refusal of the unsupported pragma.

Two existing production paths enable the setting before modifying ordinary
memory tables:

- `tracedecay-session-memory/src/fact_store/privacy_purge.rs::purge_candidates`
  sanitizes superseded payloads, records detector-flagged purge receipts, and
  deletes the matching `memory_v2_assertion_payloads` rows.
- `tracedecay-session-memory/src/fact_store/crud/lineage.rs` deletes assertion
  payloads after a terminal payload-access transition. It also clears feedback
  source and note text and marks available details redacted.

Source paths above are under `crates/`. Receipt, owner, and access invariants
remain in force. The native admission failure concerns storage behavior in
addition to those logical SQL effects.

## Exposure and limits

SQLite's enabled setting overwrites deleted content from ordinary tables with
zeros. Without equivalent behavior, a deleted row can disappear from normal
queries while its old bytes remain recoverable from database pages. A person
with raw file access could potentially recover those bytes. This is an exposure
inference from the missing capability; the probe did not perform a forensic
extraction of sensitive content.

The existing setting is not a complete erasure guarantee. SQLite documents
that FTS shadow tables can retain traces despite the setting. Historical WAL
versions, filesystem snapshots, backups, and storage-device copies also require
separate lifecycle guarantees. No result here proves their physical erasure.
See the [SQLite secure-delete documentation](https://www.sqlite.org/pragma.html#pragma_secure_delete).
This migration must not claim either backend erases every historical copy.

## Approved native deletion contract

The user explicitly approved logical deletion without SQLite's deleted-page
scrubbing guarantee. Native memory installation and both purge callers now
skip the unsupported setting. They retain logical deletion, immutable receipts,
owner checks, and feedback redaction. SQLite retains its setting and readback.
The raw native pragma still returns `Unsupported`; it never pretends to scrub.

Fresh native memory schema installation and reopen pass. Actual native purge,
receipt, and feedback-redaction journeys through registered production callers
remain unverified because that backend is not yet registered. The SQLite
memory caller suite verifies the retained behavior on the default backend.

Fresh files require no old-content conversion. Registered native final-shape
admission remains unavailable because native inventory verification is
unfinished, not because approval is pending. Complete operation coverage,
registered attachment, writer ordering, and recovery still require work.
