# Official Public API and SDKs

## Status / Role

- Required V2 product surface.
- PR12 delivers the official daemon API.
- PR18 stabilizes that API and fully delivers supported Rust, TypeScript, and Python SDKs.
- The end state is complete across all three SDKs; no language binding is deferred or skipped.

## Outcome

Agents and applications use one supported daemon API to access TraceDecay capabilities.
CLI, MCP, HTTP, and SDK adapters expose the same operations, validation, errors, and privacy behavior.
The daemon remains the only process that reads or writes product storage.

## Owns

- The public daemon protocol, versioning policy, and compatibility rules.
- Executable request, response, pagination, streaming, and error contracts.
- Rust, TypeScript, and Python client libraries.
- Authentication and connection mechanics for local and remote daemon clients.
- Cancellation, backpressure, idempotency, and retry-safe operation metadata.
- Direct parity tests across CLI, MCP, HTTP, and all SDKs.

## Does not own

- Domain rules, query semantics, privacy policy, configuration semantics, or storage implementation.
- Direct database access from clients or language bindings.
- A generated compatibility inventory or a second model of the product.
- Markdown parsers, plan trackers, task executors, or workflow JavaScript.
- Dynamic workflow execution. PR17 stores typed workflow definitions and invokes existing daemon
  operations; it does not introduce a JavaScript SDK or runtime.

## Required behavior

1. One actual contract source
   - Public request and response types live at the daemon application boundary.
   - Routes, MCP tools, CLI commands, schemas, and SDK bindings map directly to those types.
   - An adapter may change syntax, never meaning.

2. Complete operation parity
   - Every supported public operation declares its availability across CLI, MCP, HTTP, and SDKs.
   - Unsupported transport behavior is an explicit contract decision, not an accidental omission.
   - Equivalent calls return equivalent values, stable error codes, and the same redaction outcome.

3. Daemon authority
   - Clients connect to the daemon and never open TraceDecay databases.
   - The daemon owns authorization, transaction boundaries, migrations, concurrency, and recovery.
   - Connection loss, cancellation, and retries cannot duplicate committed mutations.

4. Stable protocol
   - Additive changes preserve compatibility within a major version.
   - Breaking changes require a new major protocol version and an actionable negotiation error.
   - Unknown fields are handled consistently and documented per protocol version.

5. Usable SDKs
   - Rust, TypeScript, and Python expose typed sync or async APIs idiomatic to each ecosystem.
   - Pagination, streaming, cancellation, timeouts, and structured errors are first-class.
   - SDKs provide connection setup and operation calls, not independent business logic.

6. Safe output
   - Privacy enforcement runs before every public response, stream item, log, and diagnostic payload.
   - Credential material remains opaque and is never returned by read APIs.

## Acceptance

- PR12 ships a versioned daemon API backed by the real application contracts.
- CLI, MCP, and HTTP parity tests cover every public operation and stable error code.
- PR18 ships usable, documented, tested Rust, TypeScript, and Python SDKs.
- The three SDK suites pass the same contract fixtures against one daemon build.
- Cancellation, reconnect, idempotent retry, pagination, and streaming tests pass.
- A client cannot open product storage or bypass daemon authorization and privacy enforcement.
- Contract drift is detected by executable adapter and SDK tests, not generated inventory files.
