# Laya vs Kev-4B vs Intern-Decision-4B: TraceDecay hint routing

Frozen `dataset.json` (sha256 unchanged): 121 cases, 37 `none`, 48 `context_needed`. Same 9 per-category `noul` questions, **one batched request per case**. Config **B (prompt + window)** ran on all 121 cases for every model. B2 and A ran on a stratified 65-case subset (`subset.json`), the same for all models. No fallback was needed.

## Head-to-head, config B, full set (n=121)

| | Laya | Kev-4B | Intern-4B (T=1.99) |
|---|---|---|---|
| macro AUC | 0.89 | **0.99** | **0.99** |
| macro F1, best t (in-sample) | 0.53 | 0.80 | **0.89** |
| macro F1, leave-one-out-chosen t | 0.46 | 0.73 | **0.85** |
| macro F1 @ 0.6 | 0.42 | 0.69 | **0.80** |
| `none` false-hint, best t | 0.46 | **0.00** | 0.05 |
| `none` false-hint @ 0.6 | 0.22 | **0.11** | 0.35 |
| ctx-subset top-1 acc, best t / @0.6 | 0.44 / 0.38 | 0.75 / **0.69** | **0.85** / 0.67 |
| ctx-subset exact-set acc @0.6 | 0.38 | **0.60** | 0.25 |

Per-category F1 at best t (t) / F1 @0.6 / AUC (n positives):

| category | Laya | Kev-4B | Intern-4B |
|---|---|---|---|
| session_recall (15) | 0.67 (.5) / .62 / .88 | **0.85 (.7) / .80 / .99** | 0.62 (.9) / .36 / .95 |
| memory_store (14) | 0.67 (.25) / .62 / .94 | 0.86 (.7) / .85 / .99 | **1.00 (.9)** / .67 / 1.00 |
| call_graph (10) | 0.48 (.3) / .29 / .92 | 0.87 (.4) / .82 / 1.00 | 0.87 (.8) / .87 / .99 |
| impact (11) | 0.53 (.25) / .40 / .90 | 0.80 (.4) / .53 / .97 | **0.95 (.8) / .91** / .98 |
| refactor_edit (10) | 0.44 (.6) / .44 / .77 | 0.78 (.5) / .76 / .98 | **0.91 (.8)** / .74 / 1.00 |
| review_changes (8) | 0.54 (.5) / .50 / .93 | 0.86 (.8) / .71 / .99 | 0.84 (.7) / .80 / .99 |
| type_orientation (8) | 0.71 (.25) / .22 / .98 | 0.84 (.4) / .55 / .99 | **1.00 (.6) / 1.00** / 1.00 |
| project_context (7) | 0.25 (.9) / .25 / .76 | 0.71 (.4) / .67 / .98 | **1.00 (.6) / 1.00** / 1.00 |
| unexpected_changes (6) | 0.44 (.7) / .44 / .92 | 0.67 (.5) / .50 / .99 | **0.83 (.8) / .83** / .99 |

Subset macro LOO F1 for B / B2 / A: Laya .39/.41/.49, Kev .69/.65/.60, Intern .83/.74/.69. B is best for both 4B models; A loses on context-needed cases.

**Recommended thresholds** are the per-category best t in the table. Kev's sit mid-range (.4–.8). Intern's are .6–.9 on T=1.99 probabilities, which is .94–.987 on its raw output. Intern session_recall is pinned at the grid top: windowed positives and negatives both sit around .85–.92.

## Cost (CPU, 4 cores, contended by concurrent cargo builds; use for relative scaling only)

| | Laya | Kev-4B | Intern-4B |
|---|---|---|---|
| 9-question call, median | 1.9 s | 12.9 s (8.2 s quiet) | 20.3 s |
| input tokens (B) | ~42 | ~350 | ~880 |
| peak RSS | not re-measured | 8.6 GB merged load; 9.7 GB mmap (2.6 GB anon) | 10.2 GB mmap (1.9 GB anon) |
| download | 0.84 GB | 0.16 GB adapter + 9.32 GB Qwen3.5-4B-Base | 9.11 GB (8.4 GB language-only) |

**Deviations:**
- Both 4B models ran with weights memory-mapped from safetensors (`mmap_loader.py`), because RAM was shared. Kev's adapter stayed unmerged; against Kev's merged load, max |Δp| ≤ 0.014.
- Intern ran text-only (no vision shards, stub processor), with a flush-to-zero patch on the DeltaNet fallback (denormals: 48–70 s → 15 s, identical outputs). Its preset T was applied client-side.

## Server response shapes

**Kev, `POST /v1/systemone`.** `model` is optional (default `kev-latest`, echoed back).
```
{"model":"kev-latest","answers":{"recall":{"type":"noul","noul":0.8278},
 "dept":{"type":"choice","choice":"returns","confidence":0.2601,"probabilities":{"returns":0.5067,…}},
 "mood":{"type":"score","score":1.292,"legend":{"0":"Calm",…},"probabilities":{"0":0.0424,…},"confidence":0.4348}},
 "usage":{"input_tokens":394,"output_tokens":207},"latency_ms":5862.0}
```
Compared with laya-serve, Kev drops noul `confidence`, `x_jev_confidence` and the `usage` truncation fields. It adds a top-level `latency_ms`, and returns **422** for a state over 65,536 tokens instead of truncating.

**Intern, `POST /v1/decisions`** (`/v1/systemone` returns 404). It ignores `model` and takes at most 16 questions.
```
{"answers":{"recall":{"type":"noul","probabilities":{"no":0.0017,"yes":0.9983},"confidence":0.9983,"noul":0.9983,"source":"local","decision":"yes"},
 "dept":{"type":"choice","probabilities":{…},"confidence":0.893,"choice":"returns","source":"local","decision":"returns"},
 "mood":{"type":"score","probabilities":{"0":…},"confidence":0.946,"score":1.052,"legend":{…},"source":"local","decision":"1"}},
 "usage":{"input_tokens":926,"output_tokens":9,"decision_count":9},"timing":{"inference_ms":…,"queue_ms":…,"server_ms":…},
 "model":"intern-decision","request_id":"…","thinking":{"enabled":false,"confidence_threshold":0.7,"tasks":[]}}
```
Intern's `confidence` is max(p). Output is uncalibrated unless `CALIBRATION_PATH` is bound.

## Recommendation

**Ship Kev-4B as the default.** Both 4B models are far ahead of Laya. Intern scores higher on macro F1 (LOO .85 vs .73), but Kev wins on four points:
1. It speaks the `/v1/systemone` contract TraceDecay already integrates.
2. It is better on session_recall (.85 vs .62), the highest-traffic category.
3. It is safe at default thresholds: 11% `none` false hints @0.6 vs Intern's 35%. Intern needs per-category thresholds of .8–.9, fragile at 6–15 positives.
4. It is about 1.6× cheaper per call.

Intern is stronger on the code-structure categories and is worth re-testing on a larger, independently labelled set.

## Caveats
- **Small n:** one case moves per-category F1 by .05–.15, and best-t numbers are in-sample (use LOO). The subset has 2–8 positives per category.
- **Author bias:** the same agent family wrote the dataset, questions and windows.

## Labels worth a second look (dataset not edited)
- **c043, c069:** both 4B models fire call_graph; it is plausibly a missing label.
- **c042:** the `impact` label is debatable.
- **c025:** `refactor_edit` is arguable alongside memory_store.
- **c078:** `review_changes` is plausibly a second label.
