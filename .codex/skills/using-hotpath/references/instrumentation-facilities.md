# Hotpath Instrumentation Facilities

- Synchronous function or bounded phase: `#[hotpath::measure]` or `hotpath::measure_block!("static.label", expression)`.
- Bulk instrumentation of a suspect area: `#[hotpath::measure_all]` on an inline `mod` or `impl` block applies `measure` to every function inside; exclude trivial or noisy functions with `#[hotpath::skip]`. It cannot be a file-level inner attribute, and trait-impl methods get timing/allocation but not CPU-sample attribution. Use it to blanket one investigation target, not the codebase; trim it back per the instrumentation rules before merge.
- Async task lifetime, suspension, polling, or cancellation: `#[hotpath::measure(future = true)]` or `hotpath::future!(future, label = "static.label")`; use one, not both.
- Stream production/consumption: `hotpath::stream!`.
- Queue depth and send-to-receive latency: `hotpath::channel!`; default wrap mode changes endpoint types, while `proxy = true` preserves them but loses exact depth/latency.
- Actual read/write operations and bytes: `hotpath::io!` around the one canonical handle, not both a file and its buffer.
- Lock wait and hold: `hotpath::mutex!` / `hotpath::rw_lock!`, using feature-dependent type aliases when the wrapped type changes.
- HTTP server: one Axum layer after the complete router is assembled.
- HTTP client: one supported Hotpath middleware per client. Header completion excludes body download/decode, so measure decoding separately when material.
- Tokio: register the already-built runtime once with `hotpath::tokio_runtime!(runtime.handle())`.
- Counts/current state: static `hotpath::gauge!` keys; use additive lifecycle guards for shared state and clean them up in `Drop`.
- Debug values: avoid in production unless values are bounded and non-sensitive.
- Direct rusqlite: manual phase/queue/transaction instrumentation; Hotpath 0.24 has no rusqlite adapter. Its `sql` report is fed only by third-party front-ends — `sqlx_tracing_layer()` / `toasty_tracing_layer()` are `tracing_subscriber` layers that harvest those ORMs' completed-query tracing events (emitter-measured elapsed, statements normalized into parameter-insensitive buckets, attributed to the innermost measured frame), and diesel hooks its own instrumentation trait. Use a tracing bridge only for a third-party emitter that already pays tracing's cost; first-party code keeps compile-out macros. Each bridge needs its cargo feature, and a global `EnvFilter` can suppress `sqlx::query` events for the whole stack — attach filters per layer.

