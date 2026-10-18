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

| Model | Macro AUC | Macro-F1 at fixed 0.6 | Macro-F1, leave-one-out thresholds | False hints on `none` cases at 0.6 / at best thresholds | Context-dependent top-1 at 0.6 / best | session_recall F1 (best) |
|---|---|---|---|---|---|---|
| Laya (421M, English) | 0.89 | 0.42 | 0.46 | 22% / 46% | 0.38 / 0.44 | 0.67 |
| **Kev-4B** | 0.99 | 0.69 | 0.73 | **11% / 0%** | 0.69 / 0.75 | **0.85** |
| Intern-Decision-4B | 0.99 | **0.80** | **0.85** | 35% / 5% | 0.67 / **0.85** | 0.62 |

Per-category AUC is 0.97 or higher for both 4B models on every category;
Laya ranges 0.76–0.98. Intern-Decision-4B wins the code-structure
categories (impact, refactor, type orientation, project context,
unexpected changes) but cannot separate session_recall in windowed states
(its positives and negatives both sit around 0.85–0.92), and it only
reaches its headline numbers with per-category thresholds of 0.8–0.9,
which is fragile with 6–15 positives per category. Kev-4B works at
mid-range thresholds (0.4–0.8). Intern-Decision-4B was run text-only with
its released calibration temperature (1.99) applied to `noul` logits.

Recommended per-category thresholds, in the order session_recall,
memory_store, call_graph, impact, refactor_edit, review_changes,
type_orientation, project_context, unexpected_changes:
Kev-4B .7 .7 .4 .4 .5 .8 .4 .4 .5; Intern-Decision-4B .9 .9 .8 .8 .8 .7
.6 .6 .8 (on temperature-scaled probabilities).

On a 65-case stratified subset (`subset.json`), prompt + window beat
prompt-first and prompt-only for both 4B models (leave-one-out macro-F1:
Kev .69/.65/.60, Intern .83/.74/.69).

Laya configurations compared on the same set, for the record: prompt-only
yes/no (macro AUC 0.86), prompt + window yes/no (0.89), prompt + window
single 10-way choice (0.93), hybrid (0.94); adding the window lifted AUC on
the context-dependent subset from 0.80 to 0.86–0.91, but no Laya
configuration reached macro-F1 0.45 at a fixed threshold.

Cost on this 4-core CPU box, with other builds running (relative scaling
only): one nine-question call, median: Laya 1.9 s, Kev-4B 12.9 s (8.2 s
on a quiet box), Intern-Decision-4B 20.3 s. Kev-4B uses ~350 input tokens
per call against Intern's ~880. Peak RSS was 9.7 GB for Kev (2.6 GB
non-reclaimable) and 10.2 GB for Intern (1.9 GB), with weights
memory-mapped. Downloads: Kev 0.16 GB adapter + 9.3 GB Qwen3.5-4B base;
Intern 8.4 GB text-only. Vendor figures: Kev-4B ~0.7 s on an Apple M5 and
~0.14 s when the text repeats (our window repeats between turns);
Intern-Decision-4B 44 ms on an RTX 4090. CPU-only machines should run a
0.8B model.

## Server shapes

- Kev `POST /v1/systemone`: `model` optional (defaults to `kev-latest`);
  yes/no answers are `{"type":"noul","noul":p}`; choice adds `choice`,
  `confidence`, `probabilities`; score adds `score`, `legend`,
  `probabilities`; plus `usage` and `latency_ms`. A state over 65,536
  tokens is a 422 unless `KEV_TRUNCATE_STATES=1`.
- Intern-Decision `POST /v1/decisions` (`/v1/systemone` is a 404):
  `model` ignored, at most 16 questions; yes/no answers carry
  `probabilities{no,yes}`, `confidence`, `noul`, `decision`; probabilities
  are uncalibrated unless a calibration file is configured.
- Examples: `logs/*_sample_response.json`.

## Recommendation

Default to Kev-4B with the thresholds above; keep Intern-Decision-4B as
the alternative for a re-test on a larger, independently labelled set
(it needs a `/v1/decisions` adapter). Kev also fine-tunes for about $1 of
H100 time from labelled JSONL, which is the realistic route to higher
accuracy on this narrow task. Re-sweep thresholds on this set whenever
the engine model changes: decision models rank well but overstate the
top probability, so thresholds do not transfer.

## Caveats

Synthetic cases written by the same agent family that evaluated them;
small per-category counts (one case moves a category's F1 by 0.05–0.15);
best-threshold numbers are in-sample; Kev ran with its LoRA adapter
unmerged over memory-mapped weights (probabilities differ by at most
0.014 from a merged load on four spot checks). Labels worth a second
look, left unedited: c043 and c069 (call_graph plausibly missing), c042
(impact debatable), c025 (refactor_edit arguable), c078 (review_changes
plausibly a second label).

## Files

`REPORT.md` is the agent's full write-up. `run.py` drives Laya
in-process; `run_http.py` drives any `/v1/systemone`-style server
(`kev_serve.py`, `intern_serve.py` wrap the two 4B models;
`mmap_loader.py` memory-maps their weights). `score.py` rescores
`results/` into `summary.json`; `compare.py` prints the head-to-head.
`results/` and `summary.json` are run outputs (about 59k lines for the
runs behind the tables above) and are not committed; rerun the scripts
against the model servers to regenerate them.
