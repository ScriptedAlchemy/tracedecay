# TraceDecay TypeScript SDK

`@tracedecay/sdk` provides generated TypeScript contracts and a strict client
for TraceDecay's public application API.

The generated client includes every Work and Workflow operation
admitted by the canonical executable binding registry plus the
cancellation/resumable-stream lifecycle. Only the canonical executable binding
registry determines whether an operation is available to the SDK.

## Requirements

- Node.js 20 or newer on a supported platform: macOS arm64, Linux x64 or
  arm64 (glibc), or Windows x64
- A reachable TraceDecay daemon application endpoint
- An application token issued for that daemon

## Architecture

The HTTP lifecycle runs in Rust. The package is a generated TypeScript facade
over a Node-API binding of the TraceDecay Rust SDK, so Node and Rust clients
admit exactly the same envelopes, problems, stream frames, terminal states and
reconnects. Network waits run on the binding's own threads and never block
the Node event loop. The daemon remains the authority for storage, scheduling
and admission.

The binding ships as an optional platform package
(`@tracedecay/sdk-darwin-arm64`, `-linux-x64-gnu`, `-linux-arm64-gnu`,
`-win32-x64-msvc`) that npm installs for the host automatically. Hosts that
deliver it some other way, such as an offline plugin install that places
`tracedecay_sdk_node.node` beside the daemon binary, pass its absolute path as
`addon`. MCP-backed operations never load the binding.

### Migrating from 0.15 to 0.16

- `ClientOptions.fetch` is removed. Requests no longer pass through a
  replaceable `fetch`; test against a real loopback server instead of a fetch
  stub, and route traffic through `baseUrl`. Ambient proxy variables are not
  used, as before.
- Error classes and their `instanceof` relationships are unchanged. Messages
  are now the Rust client's messages; branch on the error class, `status`, or
  `problem`, not on message text.
- Event streams ignore the SSE `retry:` field (the daemon never sends it) and
  reconnect immediately within `maxReconnects`. Lines end with LF or CRLF;
  a lone CR, a leading byte-order mark, and malformed UTF-8 are protocol
  failures rather than decoded leniently. A body that ends inside a frame is
  a protocol failure, not a retried disconnect.
- Responses are admitted only in their canonical shape: event names outside
  the canonical set, and problem records with unknown fields or identities
  inconsistent with their kind, are `TraceDecayMalformedResponseError`.
  Unknown fields inside success payloads and canonical events still pass
  through.
- Integer values from daemon responses must fit JavaScript's safe integer
  range, including receipts, events, problems, and unknown integer fields.
  Otherwise the binding raises `TraceDecayMalformedResponseError` before
  JavaScript can round them. Unsafe values in integer request fields and
  non-finite request numbers raise `TraceDecayProtocolError` before network
  I/O. Fields declared as `f64` retain floating-point semantics and may exceed
  2^53.
- `resume.nextSequence` accepts a safe integer `number`, an unsigned decimal
  string, or a `bigint` within the `u64` range. Decimal strings and bigints
  preserve the exact resume query value. Numeric fields received in stream
  frames still have to fit JavaScript's safe integer range.

## Usage

```ts
import { createClient } from "@tracedecay/sdk";

const client = createClient({
  baseUrl: "http://127.0.0.1:4317",
  projectId: "project.example",
  token: process.env.TRACEDECAY_APPLICATION_TOKEN!,
});

const graph = await client.operations.work_views(
  {
    selection: { selection: "profile_owned_no_git" },
    mode: { mode: "current" },
    continuation: null,
    observed_at: 1_800_000_000_000_000,
  },
  {
    deadlineMicros: 1_800_000_000_000_000,
  },
);
```

Request and result types, decoders, and operation methods are generated from
the Rust contracts that own the wire format. Deadline metadata is retained on
each operation descriptor and invocation deadlines are forwarded to daemon
admission. `UNAVAILABLE_OPERATIONS` reports cataloged operations that still
lack a canonical schema or SDK transport without creating callable methods.
Malformed or unavailable contracts fail closed.

## HTTP cancellation and failures

Pass an `AbortSignal` in the operation options or in
`cancelOperation(operationId, { signal })`. Caller cancellation raises
`TraceDecayAbortError` with the original signal reason as its `cause`, including
when headers have arrived and the SDK is still reading the body. Aborting
drops the in-flight native request, which closes its connection.

Body I/O failures raise `TraceDecayDisconnectedError`. Completely received but
invalid JSON or application envelopes raise `TraceDecayMalformedResponseError`.
Authentication and origin refusals retain their distinct 401/403 statuses on
`TraceDecayAuthenticationError`; they are refused from the response head
without reading the body.

Unary HTTP operations are not automatically retried. A disconnected or aborted
response does not prove that an effect was rolled back or never committed.
Use `cancelOperation` to explicitly request cancellation of a running operation.

## Resuming an operation stream

An `open` event's `frontier.next_sequence` is the server's publication watermark.
It does not mean that the subscriber has consumed those events. The client keeps
its delivered cursor unchanged on `open` and uses the frontier's `resume_token`
for reconnection. A delivered event advances the cursor to its sequence plus one;
a `resume_gap` advances it to `gap.last_missing_sequence + 1` and explicitly
reports the lost range.

For manual resume, supply the next sequence after the last event or gap your
application consumed. Use zero if you consumed only `open`. Resuming exactly one
past a completed operation delivers its retained terminal receipt once and ends.
Automatic reconnect remains opt-in through `maxReconnects`.

## Publishing

`pnpm test` runs the fast, mock-server unit suite only; it does not by itself
demonstrate that the built package works against a real daemon. `npm publish`
runs `prepublishOnly`, which also requires `pnpm run test:installed`,
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

Build the binding through `bazel build //crates/tracedecay-sdk-node:addon`
and set `TRACEDECAY_SDK_ADDON` to the resulting
`bazel-bin/crates/tracedecay-sdk-node/tracedecay_sdk_node.node` for local tests.

Actual releases run through `.github/workflows/release.yml` on the same
GitHub Release trigger that ships the binaries. The native build matrix uses
the same release targets as the daemon, loads each addon on its target runner,
and packs one platform package per target. An unprivileged
`build-typescript` job verifies the complete platform set, links those exact
versions as optional dependencies, packs the facade exactly once, and runs
typecheck/unit/conformance against that same tarball, and stages both the
artifact and digest-pinned npm tooling. A separate protected
`publish-typescript` job, gated on the release verification job, re-verifies
those exact bytes and publishes the platform packages before the facade through
tokenless npm trusted publishing
(OIDC; provenance attached automatically; no `NPM_TOKEN` exists), without
installing code. Prerelease package versions publish under the `beta`
dist-tag; stable versions take `latest`. Setting `TRACEDECAY_SDK_TARBALL` to
a prebuilt tarball path makes `test:installed` exercise that tarball instead
of packing its own.
`TRACEDECAY_SDK_NATIVE_TARBALL` selects the exact platform tarball for that
installed-consumer test. Native archives also carry the same addon beside the
daemon for offline plugin installations.

## Event-stream framing limits

Node and Rust share one framing implementation with a 64 KiB allowance for
each raw SSE line, including its line ending, and for the UTF-8 event, ID and
data bytes retained for a frame. Data-line separators count toward the frame
allowance. A frame ID carried across an empty frame stays charged to the next
frame. Canonical accepted-operation metadata, test-result names (at most
1 KiB), progress, frontiers, gaps and terminal receipts fit within this policy.

The binding reads the next body chunk only when the iterator asks for the next
event, so a slow consumer applies TCP backpressure to the daemon instead of
growing a buffer. Overflow throws `TraceDecayMalformedResponseError`, closes
the connection, and never reconnects or delivers the oversized frame. Leaving
a `for await` loop early, calling `return()` or `throw()`, or aborting its
signal releases the connection without cancelling the operation. Explicit
iterator disposal closes the transport even while `next()` is pending: that
pending read rejects with `TraceDecayAbortError`, while `return()` resolves
normally. Parsing a terminal event also closes the response immediately,
before the consumer requests another event.
