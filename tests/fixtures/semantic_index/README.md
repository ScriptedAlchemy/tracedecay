# Isolated semantic embed/index fixture

A small demo storefront codebase (Rust + TypeScript + Python) used by the
isolated semantic fixture check
(`src/daemon/production_harness/semantic_index_fixture_check_test.rs`) to
prove that in-process FastEmbed embedding and vector indexing work end to end
from SHA-256-verified local model bytes — without ever touching a live
profile, the model hub, or semantic activation.

This tree is data, not a compiled target. The check copies it into a
throwaway git checkout under a temporary directory and indexes that copy; the
checked-in tree is only read. Do not add `.git`, `.tracedecay/`, or build
output here.

## Running the check

```sh
cargo nextest run --lib \
  -E 'test(~semantic_index_fixture_check_test::)' \
  --no-tests=fail
```

The check is hermetic and has exactly two truthful outcomes:

- **pass** — every catalog member under the model cache matched its pinned
  SHA-256 and byte length, the fixture was embedded and indexed inside an
  isolated `TRACEDECAY_DATA_DIR`, a complete vector generation published, and
  semantic retrieval stayed **unactivated** (strict-semantic requests report
  typed unavailability; exact/lexical/graph answer normally).
- **pending** — one or more model members are absent or fail their SHA-256
  pin. The check prints a `pending` line naming the cache path and returns
  without failing. It never downloads: the model hub stays disabled, and
  mismatched bytes are never "repaired" from the network.

## Model cache contract

The check reads FastEmbed model bytes from a dedicated cache directory:

- `TRACEDECAY_FASTEMBED_MODEL_CACHE`, when set; otherwise
- `target/fastembed-model-cache` at the repository root (gitignored with the
  rest of `target/`; the ~641 MB model is far too large to check in).

Only bytes whose SHA-256 and length match the production catalog pins
(`crates/tracedecay-semantic/src/model_catalog.rs`, identical to
`tests/distribution/fastembed/fixture.json`) are reused. The verified bytes
are seeded into the isolated profile's lifecycle cache, where the production
install path re-verifies every member before the atomic install; later runs
reuse the same warm cache directory, so only the first run pays the copy.

Warm the cache once, in a setup phase where network use is deliberate (this
is the same acquisition script the distribution gate uses; the check itself
never invokes it):

```sh
python3 tests/distribution/fastembed/prepare_fixture.py \
  tests/distribution/fastembed \
  target/fastembed-model-cache
```

## CI cache key

Keep the fixture warm in CI with a cache keyed by the pinned digests, so the
key rolls exactly when the pinned bytes change:

```yaml
- uses: actions/cache@v6
  with:
    path: target/fastembed-model-cache
    key: fastembed-model-cache-${{ hashFiles('tests/distribution/fastembed/fixture.json') }}
```

On a cold CI cache the check reports `pending` and passes; restore or warm
the cache in a dedicated setup step (never from the test) to get the full
embed/index proof.

## Callable is not activated

A passing check proves the embed/index machinery works. It grants no semantic
activation: activation remains the Plan 20 compare-and-swap that follows a
passing Plan 15 evaluation, and the check asserts that the semantic runtime
is not `ready`, that strict-semantic search returns typed unavailability, and
that exact, lexical, and graph retrieval serve the current generation
throughout.
