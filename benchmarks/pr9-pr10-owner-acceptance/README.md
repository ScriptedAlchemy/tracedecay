# PR9/PR10 owner acceptance

This packet is an owner-controlled, deterministic evaluation surface for Plans
15, 25, and 31. It uses checked-in source fixtures and real Git history. It
does not use signatures, trust roots, attestations, reveal capabilities, or
custom anti-forgery.

The public packet contains the corpus, train and validation labels, holdout
queries without labels, partition seed, profile matrix, protected classes,
resource budgets, and decision policy. Owner holdout labels live outside the
benchmark directory at:

```text
.owner-evidence/pr9-pr10-holdout-labels-v1.json
```

`tune` has no owner-label argument and rejects any candidate output containing
a holdout query ID. `judge` requires the frozen chosen profile and creates
`owner-judgment-v1.json` with exclusive creation; a second judgment attempt
fails. This is a workflow guard against accidental reuse, not an anti-forgery
mechanism.

Candidate outputs must come from `TraceDecay::search` or
`CompositionKernel::retrieve`, bind the exact source commit and frozen packet
digest, and identify the declared profile. A Python reimplementation, fixture
lookalike, synthetic candidate list, or candidate-authored label is invalid.

Commands:

```sh
python3 benchmarks/pr9-pr10-owner-acceptance/owner_acceptance.py validate

python3 benchmarks/pr9-pr10-owner-acceptance/owner_acceptance.py freeze \
  --owner-labels .owner-evidence/pr9-pr10-holdout-labels-v1.json

python3 benchmarks/pr9-pr10-owner-acceptance/owner_acceptance.py tune \
  --candidate-outputs /path/to/train-validation-production-outputs.jsonl

python3 benchmarks/pr9-pr10-owner-acceptance/owner_acceptance.py judge \
  --candidate-output /path/to/holdout-production-output.json \
  --owner-labels .owner-evidence/pr9-pr10-holdout-labels-v1.json
```

The expected tuning input contains exactly one `train` and one `validation`
record for every evaluated profile. The holdout candidate output is generated
only after `chosen-profile-v1.json` exists. Every query result contains:

```json
{
  "query_id": "train-001",
  "ranked": [
    {
      "anchor": "time::UtcMicros",
      "scope": "research",
      "document_id": "time",
      "tier": "exact"
    }
  ],
  "confidence_ppm": 1000000,
  "abstained": false
}
```

Top-level candidate output also records `packet_digest`, `profile_id`,
`partition`, `production_boundary`, `source_commit`, `toolchain`, `hardware`,
`fallback_digest`, `pr9_fallback_digest`, cancellation/offline outcomes, and
current/10x resource samples.

The judge reports integer fixed-point Precision@1/3/5, Recall@5/10, MRR,
nDCG@10, no-answer precision, wrong-scope error, AURC, exact retention,
per-stratum support, current/10x resources, cancellation, and offline behavior.
Missing production candidates or required resource/platform evidence is
`blocked`; it is never replaced with invented output.

`execution-status-v1.json` records the current pre-tuning blocker and executed
receipts. It is not an owner quality verdict. No `owner_decision_v1` exists
until a frozen profile has a real one-time holdout judgment.
