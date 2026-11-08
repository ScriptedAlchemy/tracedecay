# Resumable HTTP evaluations

`run_http.py` requires a serving declaration in addition to the endpoint and tag:

```sh
python evals/intent_engine/run_http.py \
  --url http://127.0.0.1:8009/v1/systemone \
  --tag pinned-run \
  --server-config /path/to/serving.json \
  --send-model kev-latest \
  B_noul_text
```

The endpoint must use HTTP or HTTPS with a hostname. File URLs, data URLs,
other non-HTTP schemes, malformed ports and URL-embedded credentials are
refused before any evaluation output is opened. Otherwise urllib can read
static local JSON instead of performing a network inference.

For an authenticated server, set a bearer-token environment variable and pass
`--bearer-token-env NAME` (for example, `SYSTEMONE_TOKEN`). The runner
reads it before creating partial output or issuing requests, sends it only in
the HTTP Authorization header, and never stores the credential or its variable
name in result evidence. An authenticated remote endpoint must use HTTPS;
plain HTTP is accepted only for direct loopback addresses. Ambient proxies
and HTTP redirects are disabled, including for authenticated requests.

The JSON declaration must contain nonempty `checkpoint`, `server_revision`,
`backend`, and `dtype` strings, and the actual finite, nonnegative server
`temperature`. Include other settings that affect inference as additional keys.
Record immutable checkpoint/server revisions, not only a moving model alias.
Do not put credentials in this file. The declaration is operator-supplied
provenance, **not remote attestation**: update it whenever the deployment changes.
The optional `--noul-temperature` is a separate client-side calibration step;
it must be finite and strictly positive.

The existing `.partial` log starts with the experiment identity. This binds the
whole dataset hash, selected case IDs and order, the original labels and
context-needed annotations, the exact rendered HTTP request bytes for each
case, endpoint hash, requested model, client calibration, and the
serving declaration. A completed result retains the same identity and each row's
request fingerprint. The endpoint itself is not copied into the identity.

An interrupted run resumes only with matching identity. Changed questions,
question order, renderer output, labels, subset contents, or serving settings
refuse before any HTTP request or append. Headerless old logs, incomplete rows,
duplicate/foreign cases, and malformed probability maps also refuse; no migration
or silent salvage is performed. Choose a new tag for another experiment or an
old unidentifiable log. Completed output files are never overwritten.

An exclusive lock on the partial log rejects concurrent writers for the same tag/configuration. Rows are flushed after each response;
this is not a crash-durable database or a concurrent-writer protocol. The runner
writes and syncs the final result to a temporary file in the results directory,
then publishes it with a hard link, which fails if the output path already
exists. If the write or link fails, no truncated JSON appears at the output path.
The `.partial` log stays in place, so a retry with the same identity does not
repeat inference. The runner does not sync the directory, so an OS crash can lose
a completed result's directory entry.

Before any HTTP request, the runner tests hard-link publication in the results
directory and, when `--sample-out` names a file that does not exist yet, in the
sample directory. On a volume without hard links, such as FAT/exFAT, the runner
refuses the run before it creates a partial log or spends inference tokens. Use
an output filesystem that supports hard links. The runner never falls back to
rename, because a rename can replace an existing result.

## Token and truncation evidence

Each result records `usage.input_tokens` as `input_tokens`,
`usage.state_tokens` as `state_tokens`, and `usage.state_tokens_dropped` as
`dropped`. These are distinct server measurements; total input tokens are never
substituted for state tokens. Omitted or null telemetry stays JSON `null`.
Reported zeroes stay zero, and reported counts must be nonnegative integers.

`score.py` computes token statistics only from measured cases and reports
`input_tokens_measured_cases`, `state_tokens_measured_cases`, and
`truncation_measured_cases` alongside `total_cases`. The full-cohort
`cases_truncated` is unknown unless every case reports a dropped-token count;
`observed_cases_truncated` counts only confirmed truncation in measured cases.
No measurements means unknown token statistics, not a zero-token result.

The experiment identity is version 2. Version-1 partial logs are refused before
inference or append: their default-zero truncation and mislabeled input counts
cannot be recovered as measurements. Use a new tag; do not relabel old logs.
Version-1 completed HTTP results are also refused by the scorer. Historical
reports are not rewritten or retrospectively qualified.

The scorer also rejects completed HTTP results whose experiment-identity digest,
per-case request fingerprints, recorded order, case labels/context annotations,
dataset digest, or configuration disagree with the output. Earlier version-2 HTTP
results without the case-annotation digest must be rerun with a new tag; they
cannot be independently scored against their original annotations. These checks
detect accidental edits and mismatched runs;
these operator-written hashes do not authenticate the remote model or verify
independent ground-truth labels.

The runner still uses the historical nine-category evaluation in `common.py`.
Runtime-renderer parity, qualified labels, held-out quality, and independent
verification of server-reported telemetry remain separate qualification work.

Run the no-inference persistence regressions with:

```sh
bazel test //evals/intent_engine:http_runner_test //evals/intent_engine:scoring_test --test_output=errors
```

The tests exercise the actual runner and filesystem with substituted HTTP
responses. They do not download models, spend inference tokens, or qualify a
model's quality or performance.

## Frozen held-out qualification (additional, not runtime parity)

The historical nine-category evaluation is **not** a qualification of the ten
current Scout intent questions. The optional qualification mode makes the
existing HTTP runner/scorer fail closed on case-group leakage, unreviewed labels,
changed question instructions, post-hoc thresholds and incomplete holdout
metrics; it cannot reconstruct a canonical Scout transcript or attest a model.

Create a separate JSON annotation manifest **before running inference** with
`version: 1`, `application`, the SHA-256 of the original dataset file
(`dataset_sha256`), and `questions_sha256` equal to the canonical
`qualification.sha256_json(common.noul_questions())` of the runner's exact
question dictionary. This last digest must change with question text. Include
`thresholds` with one frozen probability threshold per requested question and
`error_tolerances` with all four numbers:
`max_none_false_hint_rate` (0–1), `max_unwanted_hint_rate` (0–1),
`min_positive_case_recall` (0–1), and `max_p95_ms` (nonnegative milliseconds).
The unwanted-hint budget counts an incorrect extra suggestion on a positive
case, even if another correct suggestion made its recall appear successful.
These tolerances must be frozen before examining the held-out results.

The `annotations` array must cover every case in the original dataset, in
that dataset's order. Each annotation has `id`, `split` (`tune` or
`holdout`), `project_id`, `session_id`, `author`, a distinct independent
`reviewer`, `review_ref`, `verified_labels` and Boolean
`verified_context_needed`. The independently reviewed labels must exactly
match the inference dataset. Tune and held-out projects cannot overlap; session
groups cannot overlap. The held-out group must contain both a positive and a
negative example. Reviewer identifiers and references are **declarations**,
not cryptographic verification of independent label quality. Do not forge them
from model predictions or adoption events.

Run with the pinned remote serving declaration as before, adding
`--dataset /path/to/original-cases.json` and
`--qualification-manifest /path/to/frozen.json`. The dataset file's bytes
are bound to the run and must be supplied again when scoring. The runner refuses an
additional `--subset`, validates every case and its group before network
traffic, infers **only the frozen held-out cases**, and binds the raw manifest's
SHA-256 to the experiment identity and resumable partial log. A changed
manifest cannot resume the old run. The scorer accepts exactly one result and
the same manifest:

```sh
python evals/intent_engine/score.py --qualified /path/to/frozen.json \
  /path/to/original-cases.json evals/intent_engine/results/pinned-run_B_noul_text.json
bazel test //evals/intent_engine:qualification_test --test_output=errors
```

This writes `evals/intent_engine/qualification_summary.json` separately from
the historical `summary.json`, and reports only **frozen-threshold** held-out
false hints, all-case unwanted suggestions, missed positive cases, coverage,
abstention, positive-case recall, p50/p95 measured end-to-end HTTP latency and
the predeclared tolerances. A passing report requires every held-out case to
report measured zero dropped state tokens. Missing truncation telemetry is
unknown, and a response measured after truncated input cannot prove the full
frozen request was evaluated; the measurements remain in the failing report. Scoring reads the original frozen case bytes,
checks all reviewed labels/splits against them and recreates each HTTP
request fingerprint with the runner's own renderer. Self-consistent result
hashes alone do not prove the submitted inputs. It deliberately does not publish optimized
in-sample thresholds as qualification. Neither correct answers nor those metrics
alone demonstrate task success, ignored/acted outcomes, cold/warm cost, native
runtime renderer/settings parity or a real Kev checkpoint. All those require
canonical transcript export, independent review, real host inference and
application-specific trials before any replacement or merge decision.
