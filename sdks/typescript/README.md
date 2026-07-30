# TraceDecay TypeScript SDK

`@tracedecay/sdk` provides generated TypeScript contracts and a strict client
for TraceDecay's public application API.

The generated operation surface comes only from canonical executable bindings.
Operations without a reviewed route or schema body are omitted rather than
exposed as partially functional methods.

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
```

Request and result types, decoders, and operation methods are generated from
the Rust contracts that own the wire format. Malformed or unavailable
contracts fail closed.
