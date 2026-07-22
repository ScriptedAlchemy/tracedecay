# PR12 daemon LSP gateway measurement packet

This packet defines the warm, in-process measurement contract for the
single-root daemon LSP session actor. It measures typed JSON-RPC dispatch,
strict `Content-Length` framing, overlay edit/debounce handling, and bounded
diagnostic publication. It does not start an analyzer, open a store, or use a
host plugin; those owners have separate cold-start and conformance budgets.

The workload also pins the callable architecture:

- `DaemonInvocationService` owns authenticated sessions;
- `DaemonLspProtocolSession` owns JSON-RPC lifecycle and framing;
- `FeedbackCyclePort` sends save and diagnostic-pull triggers to the existing
  feedback-cycle authority;
- `DiagnosticSnapshotPort` reads the committed canonical generation for
  bounded UTF-16 diagnostics; and
- `SemanticProviderPort` handles negotiated definition, references, hover,
  and related navigation without moving analyzer authority into the gateway.

The fixture pins these paths, the request mix, payload limits, and p95/p99
budgets. Static validation does not run Cargo:

```text
python3 benchmarks/pr12-lsp-gateway/validate_packet.py
```

A measured runner must report wall-clock samples (`Instant`), process CPU,
peak RSS, queued bytes/messages, overlay bytes, publication bytes, dropped or
superseded publications, and reconnect/expiry counts. It must keep all samples
and distinguish warm actor work from analyzer/indexing time. A budget miss must
be recorded as partial/unavailable behavior, never hidden by extending a
deadline or publishing a stale diagnostic.

The checked-in measurement is `pending_execution`: no timing or resource claim
is made. The declared semantic gate remains executable for an authorized run:

```text
python3 benchmarks/pr12-lsp-gateway/validate_packet.py --protocol-gate
```

The protocol gate is separate from static packet validation and does not
manufacture measurement samples.
