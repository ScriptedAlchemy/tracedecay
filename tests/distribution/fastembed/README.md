# FastEmbed distribution acceptance fixture

This directory pins the real production/default code-search model,
FastEmbed `EmbeddingModel::JinaEmbeddingsV2BaseCode`, to immutable upstream
bytes. The same pins are embedded in
`src/semantic_code/model_catalog.rs` for daemon-owned background acquisition.
The 641 MB model is intentionally not checked into Git.

`fixture.json` records the exact Hugging Face commit, Apache-2.0 provenance,
member lengths, and SHA-256 digests for:

- `onnx/model.onnx`
- `tokenizer.json`
- `config.json`
- `special_tokens_map.json`
- `tokenizer_config.json`

`prepare_fixture.py` uses `curl` only during the acceptance setup phase to
obtain that immutable package into a staging directory. It verifies every
declared length and digest before an atomic directory rename. Interrupted
members can resume in staging. The packaged FastEmbed/ORT invocation then runs
with both Cargo and Hugging Face offline flags and receives only verified local
bytes.

This is CI fixture acquisition, not a query-time model path. Synthetic ONNX
bytes, lookalike models, hash-derived vectors, mocked inference, symlinks,
mutable revisions, and query-time downloads do not satisfy the gate.

Validate the checked-in pins without downloading:

```sh
python3 tests/distribution/fastembed/prepare_fixture.py \
  --check tests/distribution/fastembed
```

Run the heavyweight acceptance command from the repository root:

```sh
scripts/check-distribution-acceptance.sh
```

This release-builds and packages every feature, installs the packaged CLI,
verifies the packaged `semantic-fastembed`/bundled-ORT feature path, performs
one bounded local-byte inference, rejects invalid model bytes, and checks the
typed fallback and strict-unavailable semantic policy.
