# TraceDecay TypeScript SDK

`@tracedecay/sdk` provides generated TypeScript contracts and a strict client
for TraceDecay's public application API.

The generated callable surface includes every Work operation admitted by the
canonical executable binding registry plus the cancellation/resumable-stream
lifecycle. Base application routes without canonical executable schema bodies
remain in `SERVER_OPERATIONS` for discovery with `schema_unavailable`; they
are not exposed as partially typed methods.

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

## Publishing

`npm test` runs the fast, mock-server unit suite only; it does not by itself
demonstrate that the built package works against a real daemon. `npm publish`
runs `prepublishOnly`, which additionally requires `npm run test:installed` —
a check that requires a prebuilt production `tracedecay` executable at
`target/debug/tracedecay`, or at the absolute path supplied through
`TRACEDECAY_TEST_BIN`; the check does not build the daemon. It starts that
binary with an isolated profile, packs the SDK, installs it into an isolated
consumer project, and exercises it as an installed dependency. Do not treat
the fast unit suite alone as publish-ready conformance.

Actual releases run through the `SDK publish` GitHub Actions workflow
(`.github/workflows/sdk-publish.yml`, dispatched with `sdk: typescript`), which
builds the real daemon, runs this same conformance suite, and publishes to npm
via OIDC trusted publishing (no `NPM_TOKEN`). See
[`docs/RELEASE-AUTOMATION.md`](../../docs/RELEASE-AUTOMATION.md) for the
trusted-publisher bootstrap.
