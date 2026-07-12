# ADR-004: One Query Algebra and Thin Public Transports

## Status
Accepted for V2 Phase 0.

## Context
Session, LCM, code, memory, graph, dashboard, CLI, and MCP paths currently risk distinct filtering, ordering, pagination, and error meanings.

## Decision
`TraceQueryV1` is the canonical typed query AST. Query owns validation, canonicalization, shard planning, budgets, exact/phrase/BM25 foundations, measured optional ranking channels, graph/time/as-of operators, explanations, stable anchors, and coverage. Application owns use cases, authorization, idempotency, effects, status, and layered errors. CLI, MCP, HTTP/SSE, SDKs, hooks, and dashboard only bind, stream, and render generated contracts.

Pages and cursors pin normalized query, resolved scope, access decision, watermarks/snapshot, representation and ranker versions, privacy digest, and expiry. A cursor mismatch is explicit; no restart at page one. SSE subscriptions are created by POST, then emit snapshot followed by ordered typed deltas with authenticated resumable event IDs, bounded coalescing/backpressure, heartbeat comments, gap detection, and required resync. Sensitive query text never enters URLs. Stable retrieval anchors outlive cursor/event/response-handle expiry.

Transport isolation forbids SQL, store routing, ranking, policy, migration, or business decisions in adapters. Public errors carry stable code, retryability, safe context, remediation capability, trace ID, and transport mapping. Partial/unknown/capped coverage is successful typed data, never zero or generic failure.

## Rejected alternatives
- GraphQL as the primary API, dashboard-private SQL endpoints, or MCP/CLI-specific query DSLs.
- WebSockets as a requirement; resumable SSE matches one-way live read models.
- Offset pagination or unsigned cursors across mutable federated shards.
- In-process production clients opening stores when the daemon is unavailable.

## Compatibility, rollback, and removal gates
A semantic fixture must normalize identically through direct test oracle, CLI, MCP, HTTP, SDK, export, and SSE snapshot. V1 routes and response handles remain migration-only until action/error/coverage parity passes. Cutover rejects stale names/protocols explicitly before store access; rollback restores the prior bounded route set, not mixed semantics.