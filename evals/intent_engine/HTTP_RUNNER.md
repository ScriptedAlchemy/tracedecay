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
this is not a crash-durable database or a concurrent-writer protocol. If final
output publication itself is interrupted, the partial log remains, but an
incomplete final file requires operator inspection before retrying that tag.

The runner still uses the historical nine-category evaluation in `common.py`.
This change does not establish runtime-renderer parity, qualified labels,
held-out quality, or accurate missing token/truncation telemetry. Those remain
separate qualification work; existing score fields are not reinterpreted here.

Run the no-inference persistence regressions with:

```sh
bazel test //evals/intent_engine:http_runner_test --test_output=errors
```

The tests exercise the actual runner and filesystem with substituted HTTP
responses. They do not download models, spend inference tokens, or qualify a
model's quality or performance.
