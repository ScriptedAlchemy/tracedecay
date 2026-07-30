# TraceDecay Rust SDK

Blocking local and remote clients for the canonical 64-operation HTTP
inventory, cancellation, paging, and resumable SSE streams.

```rust
use serde_json::json;
use tracedecay_sdk::api::HttpApplicationOperation;
use tracedecay_sdk::client::{Client, ConnectionMode, RequestOptions};

let client = Client::builder(ConnectionMode::local(
    "http://127.0.0.1:4317",
    "project.example",
    "daemon-token",
))
.build()?;
let status = client.call(
    HttpApplicationOperation::StorageStatus,
    &json!({}),
    RequestOptions::default(),
)?;
# Ok::<(), tracedecay_sdk::client::ClientError>(())
```

Work operations are not advertised as available until their production routes
are mounted.
