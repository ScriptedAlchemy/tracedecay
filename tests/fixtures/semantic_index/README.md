# Isolated semantic embed/index fixture

A small demo storefront codebase (Rust + TypeScript + Python) used by
`src/daemon/production_harness/semantic_index_fixture_check_test.rs` to prove
in-process FastEmbed embedding and vector indexing work from SHA-256-verified
local model bytes — without touching a live profile, the model hub, or
semantic activation (activation stays the Plan 20 compare-and-swap after a
passing Plan 15 evaluation).

This tree is data, not a compiled target. The check copies it into a
throwaway git checkout under a temporary directory and indexes that copy. Do
not add `.git`, `.tracedecay/`, or build output here.

## Running the check

```sh
cargo nextest run --lib \
  -E 'test(~semantic_index_fixture_check_test::)' \
  --no-tests=fail
```

Two truthful outcomes:

- **pass** — every catalog member under the model cache matched its SHA-256
  and length pin, the fixture embedded and indexed inside an isolated
  `TRACEDECAY_DATA_DIR`, a complete vector generation published, and semantic
  retrieval stayed unactivated (strict-semantic fails closed as typed
  `calibration_unavailable`; exact/lexical/graph answer normally).
- **pending** — a model member is absent or fails its pin. The check prints a
  `pending` line and passes without downloading; the model hub stays off.

## Model cache

The check reads model bytes from `TRACEDECAY_FASTEMBED_MODEL_CACHE`, or
`target/fastembed-model-cache` at the repository root when unset (gitignored;
the ~641 MB model is too large to check in). Only bytes matching the catalog
pins in `crates/tracedecay-semantic/src/model_catalog.rs` (identical to
`tests/distribution/fastembed/fixture.json`) are reused; the production
install path re-verifies every member before its atomic install.

Warm the cache once in a setup phase where network use is deliberate (the
check itself never invokes this):

```sh
python3 tests/distribution/fastembed/prepare_fixture.py \
  tests/distribution/fastembed \
  target/fastembed-model-cache
```

In CI, key the cache on the pinned digests so it rolls exactly when the
pinned bytes change; a cold cache still passes as `pending`:

```yaml
- uses: actions/cache@v6
  with:
    path: target/fastembed-model-cache
    key: fastembed-model-cache-${{ hashFiles('tests/distribution/fastembed/fixture.json') }}
```
