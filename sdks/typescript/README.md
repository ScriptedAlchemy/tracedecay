# TraceDecay TypeScript SDK

`@tracedecay/sdk` provides generated TypeScript contracts and a strict client
for TraceDecay's public application API.

The generated callable surface includes the 18 mounted Work operations and the
cancellation/resumable-stream lifecycle. The 64 older production routes remain
in `SERVER_OPERATIONS` for discovery with `schema_unavailable`; they are not
exposed as partially typed methods.

## Requirements

- Node.js 20 or newer
- A reachable TraceDecay daemon application endpoint
- An application token issued for that daemon

## Usage

```ts
import { createClient } from "@tracedecay/sdk";

const client = createClient({
  baseUrl: "http://127.0.0.1:4317",
  projectId: "project.example",
  token: process.env.TRACEDECAY_APPLICATION_TOKEN!,
});

const snapshot = await client.operations.work_snapshot(
  { page_size: 25 },
  { page: { size: 25 } },
);
```

Request and result types, decoders, and operation methods are generated from
the Rust contracts that own the wire format. Malformed or unavailable
contracts fail closed.
