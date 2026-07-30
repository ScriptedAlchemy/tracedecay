# TraceDecay Python SDK

Typed local and remote clients for TraceDecay's canonical HTTP operations,
cancellation endpoint, and resumable SSE lifecycle.

```python
from tracedecay_sdk import TraceDecayClient

client = TraceDecayClient.local(
    "http://127.0.0.1:43123",
    project_id="project.example",
    token="daemon-token",
)
response = client.operations.work_snapshot({"page_size": 25})
snapshot = response.result
```

Every Work operation admitted by the canonical executable binding registry
exposes generated `TypedDict` request/result models and runtime schema
decoders. Base application routes without canonical executable schema bodies
remain in `SERVER_OPERATIONS` for discovery and are explicitly
`schema_unavailable` in `UNAVAILABLE_OPERATIONS`.

## Publishing

`python -m unittest discover -s tests -p "test_client.py"` runs the fast,
mock-server unit suite only; it does not demonstrate that the built wheel
works against a real daemon. Before publishing a release, also run
`python -m unittest tests/test_installed_package.py`. This requires a prebuilt
production `tracedecay` executable at `target/debug/tracedecay`, or at the
absolute path supplied through `TRACEDECAY_TEST_BIN`; the test does not build
the daemon. It starts that binary with an isolated profile, builds the wheel,
installs it into an isolated virtualenv, and exercises it as an installed
dependency. Do not treat the fast unit suite alone as publish-ready
conformance.

Actual releases run through the `SDK publish` GitHub Actions workflow
(`.github/workflows/sdk-publish.yml`, dispatched with `sdk: python`), split
into two jobs so publish credentials are never exposed to build/test code: an
unprivileged `build-python` job builds the sdist and wheel exactly once,
`twine check`s them, then installs and runs the fast unit suite plus
real-daemon conformance against that *same* wheel (not a fresh rebuild),
records their sha256 digests, and uploads both; a separate
environment-protected `publish-python` job (master-only, minimal OIDC)
downloads the artifacts, re-verifies the digests, and publishes those exact,
unchanged files to PyPI via trusted publishing (no API token). Setting
`TRACEDECAY_SDK_WHEEL` to a prebuilt wheel path makes
`test_installed_package.py` install that wheel instead of building its own.
See [`docs/RELEASE-AUTOMATION.md`](../../docs/RELEASE-AUTOMATION.md) for the
trusted-publisher bootstrap.
