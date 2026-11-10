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

The `--timeout-ms` CLI flag must match frozen `arm.timeout_ms` for qualified
experiments. The frozen value (1 to 1,800,000 milliseconds) is enforced on
each network request, not just checked as a p95 scoring tolerance afterward.
This is a transport deadline, not proof of the production Scout pass's
end-to-end cancellation or resource costs. Historical unqualified runs
retain their original 30-minute default.

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
For the captured Scout arm, the endpoint must terminate in the literal
`/v1/systemone` path, with no query or fragment. The production
`DecisionEngineClientV1` appends that path to its configured base URL.
A refrozen endpoint digest alone cannot establish transport parity if a
different HTTP route accepts the same request bytes. The evaluator checks
the URL before creating evidence or making any request; a historical
(non-Scout) evaluation can still use its own endpoint. Scoring only sees
the URL's hash, not its original text, so this admission check does **not**
attest the remote server or prove real Scout provenance.

An HTTP response counts as a decision only at status **200**, matching the
production Scout client. Other successful-looking 2xx statuses (201/202/206)
refuse before reading or publishing a prediction; an interrupted exact-identity
run may resume after a genuine 200 without accepting the refused response.

Frozen inputs and HTTP responses must be unambiguous JSON: repeated object keys
(including identical duplicate values) and nonfinite constants such as `NaN`
are refused. A last-key-wins parse could otherwise make a reviewer see a
different model, split, threshold or measurement than the evaluator used.
Malformed `.partial` rows are refused without append, and qualified scoring
rejects ambiguous original data or result JSON before publishing a report.

The JSON declaration must contain nonempty `checkpoint`, `server_revision`,
`backend`, and `dtype` strings, and the actual finite, nonnegative server
`temperature`. Include other settings that affect inference as additional keys.
Record immutable checkpoint/server revisions, not only a moving model alias.
Do not put credentials in this file. The declaration is operator-supplied
provenance, **not remote attestation**: update it whenever the deployment changes.
The optional `--noul-temperature` is a separate client-side calibration step;
it must be finite and strictly positive.
For captured Scout parity, the frozen `noul_temperature` must be `null` and
`--noul-temperature` must be omitted. Production Scout forwards the server's
raw Noul probabilities to the intent policy without client calibration.
Applying evaluator-only calibration could turn a below-threshold raw answer
into a passing held-out result that Scout would never deliver. Historical
non-Scout evaluation arms may still use frozen calibration; server-side
`server_config.temperature` is a separate declaration.

New raw-response samples, resumable `.partial` logs and completed result
files are created owner-readable/writable only (mode 0600 on POSIX), even
under a permissive process umask. A safely locked, older permissive partial
has its group/other permissions removed on resume without changing evidence
bytes. Samples can contain the full model input, including private session
content: do not put them on a shared filesystem with weaker access semantics.

The existing `.partial` log starts with the experiment identity. This binds the
whole dataset hash, selected case IDs and order, the original labels and
context-needed annotations, the exact rendered HTTP request bytes for each
case, endpoint hash, requested model, client calibration, and the
serving declaration. A completed result retains the same identity and each row's
request fingerprint. The endpoint itself is not copied into the identity.

For qualified runs, an optional first-response sample is now bound to the
exact experiment hash, case ID, and request digest (and original captured wire
bytes when present). A new qualified run refuses an already occupied sample
path before inference; an interrupted qualified run accepts its existing sample
only when the identity and original request agree. The sample must be from
the **first** frozen held-out call; its complete raw response digest must
match the first completed result row, and the sample commits to that complete
row. Changed response probabilities, telemetry or row values, missing samples
after a completed first row, and a sample with no completed first row all refuse
without further inference or modifying the interrupted log. If the process
crashes between first-sample publication and its row append, start a new tag;
the original incomplete evidence is not silently reconstructed. A concurrent
sample publication is refused instead of being silently adopted. Qualified
sample admission opens a nonblocking, no-follow regular-file inode and checks
that the pathname still identifies it after reading. Symlink, FIFO and concurrent
pathname substitution refuse qualification. These cross-checks prevent accidental
stale or corrupted sample attribution, but
operator-authored digests are **not remote response attestation**. Historical
nonqualified sample behavior is unchanged. Old unbound samples are not
silently migrated: use a new sample path. When a qualified result contains the
first-row `sample_response_sha256` commitment, independent scoring requires
`--sample-file /path/to/first-response.json`. The scorer reopens the original
regular-file inode, reconstructs the frozen request and verifies experiment,
response and complete first-row commitments before publishing a report. It
rejects a sample with no matching first-row commitment, a missing required
sample, or a forged commitment on a later row. Runs without a raw sample
report `first_response_sample_checked: false`, not raw-response attestation.

Interrupted .partial logs are untrusted evidence. Resume checks a
case-count-derived total size ceiling before parsing, and bounds reads of
each identity header and result row. The bounds include the original 1-MiB
HTTP response cap, frozen questions and case IDs, with space for JSON
escaping. Oversized sparse files and overlong or unterminated rows refuse
before inference, without overwriting recoverable legitimate prefixes.
This is a memory-admission check, not independent source attestation.

An interrupted run resumes only with matching identity. Changed questions,
question order, renderer output, labels, subset contents, or serving settings
refuse before any HTTP request or append. Headerless old logs, incomplete rows,
duplicate/foreign cases, and malformed probability maps also refuse. A resumable
log must be a contiguous prefix of the frozen case order: a row for a later case
cannot be used to skip or reorder earlier inference and distort cold/warm timing.
No migration or silent salvage is performed. Choose a new tag for another experiment or an
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
Impossible cross-counter evidence is refused before a fresh row is appended,
before an interrupted log is resumed, and by the independent scorer: dropped
state tokens may not exceed the original state count, and the number of state
tokens actually consumed may not exceed the server's total input-token count.
When no explicit consumed-state count exists, an original-minus-dropped count
is checked where both measurements are present. These checks catch
contradictory self-reports; they do not authenticate the server or replace
independently verified token accounting.

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
(`dataset_sha256`), and `questions_sha256` equal to
`qualification.sha256_ordered_questions(common.noul_questions())` for the
runner's exact question dictionary. This digest freezes **question key order
as emitted in the HTTP body**, as well as question names/instructions; a
sorted-object digest does not prevent trying reordered held-out arms. Include
`holdout_requests_sha256`, a precommitted SHA-256 of the ordered
`[[case_id, request_sha256], ...]` pairs for the held-out cases. Construct each
request SHA-256 from the exact serialized JSON body (the selected renderer
state, ordered questions and optional model selector) **before** evaluating
any holdout. The runner refuses a changed renderer output under an unchanged
manifest before model calls or output creation. Qualified scoring independently
checks the result experiment's request digest against this precommitment.
Generate a new frozen manifest for a changed renderer; a renderer config name
alone is not a code or output identity. This remains declared benchmark
evidence, **not** production Scout transcript rehydration or runtime parity.
Include an `arm` object predeclaring exactly one `config` renderer, a nullable
`requested_model`, `timeout_ms` (the enforced per-request timeout),
`endpoint_sha256` (SHA-256 of the exact URL bytes), and
`server_config_sha256` (the sorted-JSON digest of the complete serving
configuration, via `qualification.sha256_json(server_config)`). The runner
refuses additional renderers or changed serving/model/endpoint declarations
**before inference**; scoring rechecks these identities. This prevents choosing
the best evaluation arm only after examining held-out predictions. These are
operator declarations, not remote checkpoint attestation. Include
`noul_temperature` as JSON `null` for no client calibration or a finite,
positive number matching the runner's `--noul-temperature` argument. This
client-side calibration must be frozen alongside the thresholds: changing it
after seeing holdout predictions would select a new effective decision boundary
without changing the threshold manifest. Include
a required `max_request_bytes` positive integer upper bound on the exact UTF-8
JSON request body. This precommitted byte limit is **not** the model's
validated 8,192-token context limit: byte counts and token counts are different.
Also freeze a positive integer `max_input_tokens` as an independent **reported**
input-token ceiling. For the published Kev-4B checkpoint, do not set this above
its 8,192-token validated context range. Unlike the JSON byte cap, token usage
is known only after the server replies; missing or zero `usage.input_tokens`,
or any reported count above this precommitted limit, fails qualification rather
than being counted as within budget. Reported token counts are unverified server
telemetry, not an independently reproduced tokenization or attestation.
The runner refuses oversized requests before any result file or model call;
qualification independently reconstructs and verifies the byte count of every
held-out request. Every HTTP result also records the model name *reported by
the server*, including explicit null when the endpoint omits it. A frozen-arm
qualification fails if this name is missing, differs among held-out responses,
or contradicts a non-null requested model selector. A frozen arm now requires a nonblank server-reported model on **each** response,
even with no explicit model selector. Missing identities are rejected before
appending the response; missing or blank identities in an existing partial are
rejected before sending more requests. For an explicit selector, a contradictory
response is likewise rejected before appending. A change in the server-reported
name between cases is refused before the second append; valid interrupted runs
still resume with their recorded model identity. The reported name is **not** cryptographic server
identity or checkpoint attestation; a
remote service can misreport it.

Include `thresholds` with one frozen probability threshold per requested
question and `error_tolerances` with all four numbers:
`max_none_false_hint_rate` (0–1), `max_unwanted_hint_rate` (0–1),
`min_positive_case_recall` (0–1), and `max_p95_ms` (nonnegative milliseconds).
The unwanted-hint budget counts an incorrect extra suggestion on a positive
case, even if another correct suggestion passed its threshold. The frozen
positive-case recall check uses the **single highest-probability qualifying
model-only pick** (ties follow frozen question order), not any-hit recall.
The report retains any-hit recall and all-firing metrics separately. The
qualification gates for negative-case false hints and selected positive-case
recall emulate Scout's integer-permille threshold rounding and u16 relevance
rounding (ties follow the frozen question vocabulary order, **not** the
threshold JSON object's key order), rather than treating the
historical benchmark's floating-point threshold comparison as production.
Thus a probability of 0.5996 meets Scout's 600-permille threshold, even though
it is below the historical floating-point threshold of 0.6. The historical
fixed-threshold metrics remain diagnostic, not acceptance evidence. This is
only a necessary Noul selection check: captured Scout arms additionally use
frozen `margin_permille` values and compare against **all** other answered
Noul questions, including those below their own threshold, matching Rust's
saturating integer-permille margin calculation. Historical arms have no margin
policy. The model-only comparison still does **not** replay Scout's deterministic
action facts, duplicate suppression or delivery state and cannot establish
runtime parity.
These tolerances must be frozen before examining the held-out results.

The `annotations` array must cover every case in the original dataset, in
that dataset's order. Each annotation has `id`, `split` (`tune` or
`holdout`), `project_id`, `session_id`, `author`, a distinct independent
`reviewer`, `review_ref`, `verified_labels` and Boolean
`verified_context_needed`. The independently reviewed labels must exactly
match the inference dataset. Tune and held-out projects cannot overlap; session
groups cannot overlap. The original frozen dataset must also carry each case's
`project_id` and `session_id`, identical to its reviewed annotation. Inventing
independent groups solely inside the manifest is refused even if the manifest
and result hashes are recomputed. The held-out group must contain both a
positive and a negative example. Reviewer identifiers and references are **declarations**,
not cryptographic verification of independent label quality. Do not forge them
from model predictions or adoption events.

Run with the pinned remote serving declaration as before, adding
`--dataset /path/to/original-cases.json` and
`--qualification-manifest /path/to/frozen.json`. The dataset file's bytes
are bound to the run and must be supplied again when scoring. The runner also
refuses a missing or mismatched frozen client-calibration setting before any
HTTP request and the scorer rechecks it against the experiment identity. The runner refuses an
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
abstention, positive-case recall, reconstructed JSON request sizes
(min/p50/max/total and the frozen byte ceiling), input-token measurement
coverage, observed peak and over-budget cases, p50/p95 measured end-to-end HTTP
latency and the predeclared tolerances. A passing report requires every held-out case to
report measured zero dropped state tokens **backed by original numeric server
telemetry on every held-out row**, and a consistent self-reported model name.
An edited/legacy result with `dropped: 0` but no original numeric source
counter cannot qualify merely because its aggregate looks untruncated.
Native Kev's optional truncation receipts use a top-level Boolean
`truncated` and numeric `usage.state_tokens` / `usage.state_tokens_used`,
not `usage.state_tokens_dropped`. The runner reconstructs their difference
and rejects inconsistent counts or flags before recording a row. The native
server's original and used token counts and explicit truncation flags are
retained in the output; an independent scorer and partial-resume reader reject
a rewritten normalized drop count. Direct `state_tokens_dropped` counts remain
supported for servers that expose them.
Kev emits those paired counters only when its `KEV_TRUNCATE_STATES=1` serving
setting is enabled; a bare `truncated: false` without numeric counters is
**unknown, not measured zero**, and cannot pass qualification. Freeze the
serving configuration; no infrastructure change is implied. Missing truncation telemetry is unknown, and a response measured after truncated input cannot prove the full
frozen request was evaluated; the measurements remain in the failing report. Scoring reads the original frozen case bytes,
checks all reviewed labels/splits against them and recreates each HTTP
request fingerprint with the runner's own renderer. Self-consistent result
hashes alone do not prove the submitted inputs. It deliberately does not publish optimized
in-sample thresholds as qualification. Neither correct answers nor those metrics
alone demonstrate task success, ignored/acted outcomes, cold/warm cost, native
runtime renderer/settings parity or a real Kev checkpoint. All those require
canonical transcript export, independent review, real host inference and
application-specific trials before any replacement or merge decision.

## Declared question vocabulary (qualification only)

The historical benchmark uses nine fixed categories. A frozen held-out run
can instead use `--questions-file /path/to/questions.json` alongside
`--qualification-manifest`. The JSON object maps each configured key to
`{"type":"noul","instructions":"..."}`; the manifest's `questions_sha256`
must pin the ordered question contract via
`qualification.sha256_ordered_questions(questions)`. A changed insertion
order of question keys is a different arm and is refused before inference.
Score with
`score.py --qualified MANIFEST DATASET RESULT --questions-file FILE`.
Both commands reject changed or non-Noul instructions; historical scoring
retains its original nine-category behavior.

**Not runtime parity:** An operator-supplied file does not establish that the
actual configured Scout questions, settings digest, window renderer or
rehydrated transcript were used. Obtain that evidence from the production
settings and owning transcript authority before making any application
replacement decision. The runner itself does not export canonical Scout data.

## Captured Scout request arm (qualified only)
The live intent pass also records a content-free `intent_decision` analytics
event for **every completed assessment**, including below-threshold and
no-signal results that produce no delivered hint. The event keeps the owning
session/project/provider and boundary identity, original store-ID bounds and
window digest, settings digest, typed model outcome, Noul probabilities,
observed action facts, prompt inferences, and the policy selection separately.
An unavailable model has **null answers**, not fabricated negative labels.
The transcript body stays in the owning session store. These events are
provenance pointers, not independent correctness labels or proof that the
session text still exists: analytics can also be unavailable, and the missing
bounded exporter must rehydrate the original owning window and independently
verify its digest before claiming a canonical Scout capture. No such
rehydration or real Kev experiment is claimed by this event alone.


The historical `A/B/B2` renderers do **not** reproduce production Scout
requests. A separately captured, byte-exact request file can now be evaluated
without rewriting those requests in Python. This is a new arm,
`S_scout_captured`, not a historical benchmark result.

Use a JSON array containing **only the ordered held-out cases**, each with
`{"id":"case-id","body":"<complete JSON HTTP request as a string>"}`.
The inner `body` string must be the literal UTF-8 JSON that the runtime sent:
`{"state": "...", "questions": {"intent": {"type": "noul",
"instructions": "..."}}}` with optional `"model"` if the frozen model
selector is non-null. Keep its exact byte encoding, escapes, whitespace and
field order: the evaluator sends those bytes directly instead of generating
new ones with the historical renderer. In the pinned Bazel Rust runtime,
`serde_json::Map` serializes question object keys in lexical order even when
the effective settings' `questions` vector is configured in a different order.
For this arm, `questions_sha256` binds the declared settings vocabulary and
its order, while `captured_requests_sha256` and `holdout_requests_sha256`
bind the actual ordered **wire bytes**. The capture validator requires the
canonical sorted question-map wire order, not a match to the settings vector's
iteration order. It also checks the full pretty Rust `serde_json::Value` wire
encoding (lexically ordered object keys, two-space indentation and UTF-8 text
rather than alternative Unicode escapes). The client uses one
`serde_json::to_vec_pretty` byte buffer for both transport and request
commitments. Semantically equivalent compact JSON or different indentation
is not a legitimate capture of this pinned runtime serializer, even if every
evidence hash has been recomputed. Only the inner request must be canonical; the outer evidence
file preserves its original hashed bytes. This wire-shape check does **not**
attest that the bytes actually originated from Scout.

The manifest must set `arm.config` to `S_scout_captured` and freeze
`margin_permille` as a complete key-to-value map in configured question order:
each value is either `null` (no margin) or an integer from 0 through 1000
matching the effective production `IntentQuestionSettingsV1.margin_permille`.
The HTTP request body does **not** contain these local policy settings; they
must be obtained from the same settings authority and independently verified,
not inferred from the model predictions. Missing, incomplete, malformed or
historical-arm margin declarations fail before HTTP inference. The arm also
freezes a production-range integer `max_state_bytes` (1..1048576) from the
actual `DecisionEngineConfigV1` / effective Scout settings. Captured thresholds
must be exact integer-permille values, not fractional probabilities the Rust
settings cannot represent. Frozen question instructions must be control-free,
and a selected model must be an unpadded, control-free, at-most-256-byte name:
the Rust client trims configured model names before sending them. This limits the UTF-8
bytes in the decoded `state` field separately from the whole HTTP JSON byte
ceiling. The Rust client keeps only the newest state tail (on a character
boundary) if the input is larger; therefore a claimed capture exceeding this
state cap cannot have been emitted by that configured client. The validator
refuses it before inference, even if all capture and manifest digests are
recomputed. This is a **declared** setting until verified against canonical
settings provenance, not proof of production runtime identity. Set
`captured_requests_sha256` to the SHA-256 of the entire **original capture
file**, and `holdout_requests_sha256` to the existing ordered
`[[case_id, sha256(inner_body_bytes)], ...]` commitment. Other manifest
identity, split, serving, calibration, token-budget, and tolerance constraints
are unchanged. For example:

```sh
python evals/intent_engine/run_http.py \
  --url https://private-model-host.example/v1/systemone \
  --tag scout-capture-1 \
  --server-config /path/to/serving.json \
  --qualification-manifest /path/to/manifest.json \
  --questions-file /path/to/frozen-questions.json \
  --captured-requests /path/to/scout-requests.json \
  S_scout_captured

python evals/intent_engine/score.py --qualified \
  /path/to/manifest.json /path/to/original-dataset.json \
  evals/intent_engine/results/scout-capture-1_S_scout_captured.json \
  --questions-file /path/to/frozen-questions.json \
  --captured-requests /path/to/scout-requests.json
```

Both the runner and independent scorer admit capture files through the same
bounded original-byte reader: it refuses non-regular or substituted paths,
source growth during reading, and sources larger than a cohort-derived JSON
escape allowance or 64 MiB, whichever is smaller. Neither entry point reads
an unbounded capture into memory before checking the frozen digest. Keep
larger experiments in separately frozen bounded cohorts; this protects the
qualification process, not the remote model's resource usage.

Both the runner and independent scorer refuse altered capture bytes, missing
or reordered held-out cases, invalid Noul questions, noncanonical Scout wire
question key ordering (even when map values match), differing model selectors,
extra request keys, request-size violations, or a capture attached to an
historical renderer. A capture is an **operator-supplied input**, not proof that
its bytes came from Scout. The owning transcript's original store-id bounds,
canonical window digest, production settings, real question instructions, and
actual request serialization must still be independently verified by the
missing Scout exporter. Do not claim production parity, real Kev inference,
reviewed labels, or a replacement result merely because a captured-arm
fixture passes tests.

Original frozen question, manifest, server, dataset, subset and qualified result
inputs use the same maximum 64 MiB no-follow, held-inode reader as Scout
captures. Oversized sparse inputs, symlinks, FIFOs and detectably changed
inodes refuse before model traffic or scored publication. This is a resource
and local evidence-integrity bound, not source/model attestation.
