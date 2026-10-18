# TraceDecay Rust SDK

Blocking local and remote clients for typed Work and Workflow operations,
cancellation, paging, and resumable SSE streams.

## Prerequisites

The client connects to a running daemon and addresses a project that daemon
already serves. Set both up before the first call, in this order:

```bash
tracedecay daemon install-service   # or: tracedecay daemon start
tracedecay init /path/to/project
```

The daemon comes first. `tracedecay init` is brokered through the
daemon-owned code-index scheduler, so with no daemon reachable it refuses
before writing anything:

```
Error: project route error (code_index_scheduler_unavailable): project initialization requires the daemon-owned code-index scheduler; start the daemon and retry
```

```rust
use serde_json::json;
use tracedecay_sdk::client::{Client, ConnectionMode, OperationRequestOptions};
use tracedecay_sdk::operations::{TypedOperation, WorkSnapshot};

let client = Client::builder(ConnectionMode::local(
    "http://127.0.0.1:4317",
    "project.example",
    "daemon-token",
))
.build()?;
let request: <WorkSnapshot as TypedOperation>::Request =
    serde_json::from_value(json!({"page_size": 25}))?;
let snapshot = client.execute_with_options::<WorkSnapshot>(
    &request,
    OperationRequestOptions {
        deadline_micros: Some(1_800_000_000_000_000),
    },
)?;
# Ok::<(), tracedecay_sdk::client::ClientError>(())
```

Every generated callable comes from a canonical executable binding, including
its request/result schemas, route, effect, idempotency, cancellation, and
deadline contract. `operations::UNAVAILABLE_OPERATIONS` is generated from the
same registry for cataloged operations that still lack a canonical schema or
SDK transport; those entries do not acquire callable methods.
