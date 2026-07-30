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

The 17 mounted Work operations expose generated `TypedDict` request/result
models and runtime schema decoders. The 64 older production routes remain in
`SERVER_OPERATIONS` for discovery and are explicitly `schema_unavailable` in
`UNAVAILABLE_OPERATIONS`.
