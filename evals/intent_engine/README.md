# Intent engine eval

Which local decision model should judge user intent for the daemon-owned
suggestion pass (plan 41), and at what thresholds.

## Data

- `dataset.json`: 121 multi-turn cases written by hand before any model
  ran (sha256 in `dataset.sha256`; labels are frozen). Each case has
  2–5 prior turns (user, assistant, tool calls), a current prompt, zero or
  more labels, and `context_needed` (48 cases are ambiguous without the
  prior turns). 37 cases are `none`, including hard negatives that mention
  memory, sessions, history or refactoring without that intent.
- `heldout_prompts.json`: the earlier 78 single-prompt set used for the
  session-recall-only experiment.
- Categories judged: session_recall, memory_store, call_graph, impact,
  refactor_edit, review_changes, type_orientation, project_context,
  unexpected_changes. Tool-shape hints (the agent ran `rg`, read a whole
  file) stay deterministic and are out of scope.

Every model answers the same nine yes/no (`noul`) questions in one batched
request over the same rendered state: the prior turns as a plain-text
transcript, then the current prompt.

## Results (prompt + window, per-category yes/no)

| Model | Macro-F1 at fixed 0.6 | Macro-F1, leave-one-out thresholds | Top-1 accuracy at per-model best thresholds | False hints on `none` cases | Context-dependent subset, macro-F1 at 0.6 |
|---|---|---|---|---|---|
| Laya (421M, English) | 0.42 | 0.46 | 0.55 | 46% | 0.23 |
| Kev-4B | 0.69 | 0.73 | 0.83 | 0% | 0.59 |
| **Intern-Decision-4B** | **0.80** | **0.85** | **0.91** | 5% | **0.78** |

Per-category AUC is at or above 0.97 for Kev-4B and 0.95 for
Intern-Decision-4B on every category; Laya ranges 0.76–0.98.
Intern-Decision-4B's one soft spot is session_recall (best F1 0.62);
Kev-4B reaches 0.85 there. Intern-Decision-4B was run with its model
card's calibration temperature (1.99) applied to `noul` logits.

Laya configurations compared on the same set, for the record: prompt-only
yes/no (macro AUC 0.86), prompt + window yes/no (0.89), prompt + window
single 10-way choice (0.93), hybrid (0.94); adding the window lifted AUC on
the context-dependent subset from 0.80 to 0.86–0.91, but no Laya
configuration reached macro-F1 0.45 at a fixed threshold.

Latency on this 4-core CPU box, Kev-4B, nine questions batched over ~375
input tokens: 8.2 s median per case; one question over ~100 tokens: 3.4 s.
Not representative: Kev reports ~0.7 s for Kev-4B on an Apple M5 and
~0.14 s when the text repeats (our window repeats between turns);
Intern-Decision reports 44 ms on an RTX 4090. CPU-only machines should run
a 0.8B model.

## Recommendation

Default to Intern-Decision-4B with per-intent thresholds near 0.6; keep
Kev-4B as the alternative (same `/v1/systemone` server shape; fine-tunes
for about $1 of H100 time from labelled JSONL). Re-sweep thresholds on
this set whenever the engine model changes: decision models rank well but
overstate the top probability, so thresholds do not transfer.

## Caveats

Synthetic cases written by one author; small per-category counts (6–15
positives); vendor latency figures. Treat the ranking as robust and the
absolute numbers as indicative.

## Files still to land

The runner scripts and the per-model result JSON live in the experiment
scratch directory until the comparison finishes; see the pull request's
remaining-work list.
