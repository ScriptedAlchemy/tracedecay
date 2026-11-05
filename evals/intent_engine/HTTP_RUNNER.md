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

The JSON declaration must contain nonempty `checkpoint`, `server_revision`,
`backend`, and `dtype` strings, and the actual finite, nonnegative server
`temperature`. Include other settings that affect inference as additional keys.
Record immutable checkpoint/server revisions, not only a moving model alias.
Do not put credentials in this file. The declaration is operator-supplied
provenance, **not remote attestation**: update it whenever the deployment changes.
The optional `--noul-temperature` is a separate client-side calibration step;
it must be finite and strictly positive.

The existing `.partial` log starts with the experiment identity. This binds the
whole dataset hash, selected case IDs and order, the exact rendered HTTP request
bytes for each case, endpoint hash, requested model, client calibration, and the
serving declaration. A completed result retains the same identity and each row's
request fingerprint. The endpoint itself is not copied into the identity.

An interrupted run resumes only with matching identity. Changed questions,
question order, renderer output, labels, subset contents, or serving settings
refuse before any HTTP request or append. Headerless old logs, incomplete rows,
duplicate/foreign cases, and malformed probability maps also refuse; no migration
or silent salvage is performed. Choose a new tag for another experiment or an
old unidentifiable log. Completed output files are never overwritten.

Use only one writer per tag/configuration. Rows are flushed after each response;
this is not a crash-durable database or a concurrent-writer protocol. Final
results are fully rendered and synced in a same-directory temporary file, then
published exclusively using an atomic hard link. A failed write/link cannot
expose truncated completed JSON and leaves the exact-identity `.partial` log
available for a retry without repeating inference. Completion does not
guarantee directory metadata durability across an abrupt OS crash. Before
making HTTP requests, the runner probes no-clobber hard-link publication in
the results directory and (if needed) the optional sample directory. Volumes
without hard-link support, such as FAT/exFAT, refuse immediately without
creating a partial log or spending inference tokens; use a compatible output
filesystem instead. No unsafe replace-on-rename fallback is attempted.

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
