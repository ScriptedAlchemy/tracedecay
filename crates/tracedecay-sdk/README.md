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

All 18 mounted Work routes and 5 mounted Workflow routes use generated
request/result wire models. The 64 older production routes remain
discovery-only `SchemaUnavailable` capabilities until their canonical catalog
exports schema bodies.
