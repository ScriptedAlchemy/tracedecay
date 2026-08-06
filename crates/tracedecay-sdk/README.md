# TraceDecay Rust SDK

Blocking local and remote clients for typed Work and Workflow operations,
cancellation, paging, and resumable SSE streams.

```rust
use serde_json::json;
use tracedecay_sdk::client::{Client, ConnectionMode};
use tracedecay_sdk::operations::{TypedOperation, WorkSnapshot};

let client = Client::builder(ConnectionMode::local(
    "http://127.0.0.1:4317",
    "project.example",
    "daemon-token",
))
.build()?;
let request: <WorkSnapshot as TypedOperation>::Request =
    serde_json::from_value(json!({"page_size": 25}))?;
let snapshot = client.execute::<WorkSnapshot>(&request)?;
# Ok::<(), tracedecay_sdk::client::ClientError>(())
```

Every generated SDK method comes from a canonical executable binding, including
its request/result schemas, route, effect, idempotency, and cancellation
contract. The SDK does not publish a separate discovery inventory that can
invent unavailable operations.
