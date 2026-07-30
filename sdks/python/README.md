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
result = client.operations.health_read({})
```

Work operations remain listed in `UNAVAILABLE_OPERATIONS` until their
production routes are mounted.
