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
