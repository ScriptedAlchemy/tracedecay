# TraceDecay TypeScript SDK

`@tracedecay/sdk` provides generated TypeScript contracts and a strict client
for TraceDecay's public application API.

The generated callable surface includes every Work and Workflow operation
admitted by the canonical executable binding registry plus the
cancellation/resumable-stream lifecycle. Only the canonical executable binding
registry determines whether an operation is available to the SDK.

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
the fast unit suite alone as publish-ready conformance. `prepublishOnly` is a
local safety net for anyone running `npm publish` by hand from this
directory; `npm publish <tarball>.tgz` (what the CI publish job does) skips
package lifecycle scripts entirely, so CI's own build/conformance job is the
authoritative gate, not this hook.

Actual releases run through `.github/workflows/sdk-publish.yml`, manually
dispatched from `master` without a package selector. An unprivileged
`build-typescript` job packs the tarball exactly once, runs
typecheck/unit/conformance against that same tarball, and stages both the
artifact and digest-pinned npm tooling. A separate protected
`publish-typescript` job re-verifies those exact bytes and publishes through
npm OIDC without installing code or receiving an `NPM_TOKEN`. Setting
`TRACEDECAY_SDK_TARBALL` to a prebuilt tarball path makes `test:installed`
exercise that tarball instead of packing its own.
