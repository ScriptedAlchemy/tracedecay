# MCP Response Truncation & Local Response Handles — Failure-Mode Audit

Scope: the path that keeps MCP tool responses under the transport size budget and
the "reversible truncation" handle cache that lets a client recover the original.
This is `src/mcp/response_handles.rs` plus the truncation helpers in
`src/mcp/tools/handlers/mod.rs` and the dispatch boundary in `src/mcp/server.rs`.
The irreversible plain-text truncation used by most tools is included where it
shares the budget constant.

All line references are against the current `master` checkout.

---

## 1. The budget and the two truncation paths

- Budget: `MAX_RESPONSE_CHARS = 15_000` (`src/mcp/tools/mod.rs:20`). Despite the
  name, this is a **byte** limit, not a character count — the helpers compare
  `str::len()` (bytes) against it. The same is true of the `*_chars`/`preview_chars`
  field names reported to clients: they are byte counts.

There are two truncation strategies, and they are **not applied uniformly**:

| Helper | File:line | Stores a handle? | Used by |
|---|---|---|---|
| `truncate_response(&str)` | `handlers/mod.rs:99` | **No — irreversible** | `graph`, `info`, `health`, `analysis`, `git`, `redundancy`, `workflow` handlers (the majority of the tool surface) |
| `truncated_json_envelope_with_handle(project_root, &str)` | `handlers/mod.rs:123` | **Yes — reversible** | `dashboard` (`handlers/dashboard.rs:66,97,138`), `memory` (`handlers/memory.rs:20`), `session` / LCM (`handlers/session.rs:37,70,75`) |

So when a `tracedecay_search` / `tracedecay_callers` / `tracedecay_read` /
`tracedecay_context` response exceeds 15 KB, the tail is dropped with a
`[... truncated at N chars]` marker and there is **no way for the client to
recover the omitted bytes**. Reversible truncation is currently limited to the
dashboard, memory (fact store), and LCM session handlers.

`truncate_response` walks back to a UTF-8 char boundary before cutting
(`handlers/mod.rs:104-108`), as does the envelope loop
(`handlers/mod.rs:134-136`) — neither panics on multi-byte content.

---

## 2. Handle lifecycle — create / store / retrieve / expire

All storage is in `src/mcp/response_handles.rs`.

- **Create/store** — `store_response_handle(project_root, content, now)`
  (`response_handles.rs:44`). The handle is content-addressed:
  `sha256(content)[0..12 bytes]` rendered as 24 lowercase hex chars prefixed with
  `rh_` (`response_handles.rs:119-123`, `HANDLE_HEX_CHARS=24`, `HANDLE_PREFIX`).
  Identical content always maps to the same handle, so stores are de-duplicating
  and idempotent. Written atomically: temp file `*.json.tmp.<pid>` then
  `fs::rename` (`response_handles.rs:59-67`). Stored on disk as
  `StoredResponseHandleRecord { created_at, expires_at, content }`
  (`response_handles.rs:37-42`) — note the on-disk record carries **no** `handle`
  or `original_chars` field (the filename is the handle).
- **Retrieve** — `retrieve_response_handle(project_root, handle, now)`
  (`response_handles.rs:71`). Returns `Ok(None)` when (a) the file is absent or
  (b) `expires_at <= now`; in the expired case it also deletes the file
  (`response_handles.rs:82-85`). Any other read/parse failure returns `Err`.
- **Expire** — `RESPONSE_HANDLE_TTL_SECS = 86_400` (24 h, `response_handles.rs:16`).
  `expires_at = now.saturating_add(TTL)`.
- **Sweep** — `cleanup_expired_response_handles(project_root, now)`
  (`response_handles.rs:94`) exists but is **dead code**. A repo-wide search finds
  only its definition; nothing in the server startup, the request loop, or a
  background task ever calls it. Expired handle files are therefore removed only
  *lazily* — when that exact content-addressed handle is retrieved again after
  expiry. Because handles are content-addressed, a truncated response that is
  never re-produced with byte-identical content is **never swept**; its file sits
  in `.tracedecay/response-handles/` until the disk fills or a human intervenes.

The `tracedecay_retrieve` tool (`handle_retrieve`, `handlers/mod.rs:172`) wraps
`retrieve_response_handle` and emits two payload shapes:

- Found: `{ handle, expired:false, original_chars, created_at, expires_at, content }`
  (`original_chars` is `content.len()`, i.e. bytes — `response_handles.rs:32-34`).
- Not found / expired: `{ handle, expired:true, content:null }`.

These two shapes are test-locked (`tests/mcp_handler_test.rs:532-583`,
`retrieve_tool_returns_full_stored_response`).

---

## 3. Observed failure modes

### 3.1 Handle-store failure is swallowed → silent, unrecoverable truncation
`truncated_json_envelope_with_handle` stores best-effort
(`handlers/mod.rs:130-131`):

```rust
let stored = project_root
    .and_then(|root| store_response_handle(root, formatted, current_timestamp()).ok());
```

The `.ok()` discards any `Err` (disk full, permission denied, read-only FS,
broken path). When `stored` is `None` the envelope is still emitted with
`truncated:true`, `original_chars`, `preview_chars`, `preview` — but **without**
`handle` / `retrieve_*` keys (those are only inserted when `stored` is `Some`,
`handlers/mod.rs:144-163`). The original full response existed only in memory and
is now gone; the client receives a truncated preview with no recovery handle and
no error. There is no log line and no metric. This is the primary silent-data-loss
path.

Note `stored` is also `None` whenever `project_root` is `None` — i.e. for every
**profile-scoped** LCM tool call (dispatched via
`handle_profile_scoped_lcm_tool_call`, `handlers/mod.rs:343`, which passes
`project_root=None`). Profile-scoped truncation therefore never produces a handle
even on success. This is by design (nowhere to store), but it is undocumented and
means the same LCM tool yields handles or not depending on storage scope.

### 3.2 Retrieve cannot distinguish "expired" from "never stored" from "deleted"
`retrieve_response_handle` returns `Ok(None)` for all three of: file absent,
file expired-and-just-deleted, and handle string well-formed but unknown. The tool
surfaces all of them as the identical `{expired:true, content:null}` success
payload. Combined with 3.1, a client that retrieves after a failed store gets
`expired:true` and reasonably concludes "it timed out" when in fact it was never
persisted. There is no `not_found` / `store_failed` signal.

### 3.3 Every handler error collapses to a single JSON-RPC code
At the dispatch boundary (`src/mcp/server.rs:1654-1658`):

```rust
Err(e) => JsonRpcResponse::error(
    id, ErrorCode::InternalError, format!("tool execution failed: {e}")),
```

Every `TraceDecayError` variant — `Config` (malformed handle, missing `handle`
param), `Io` (disk outage on store/retrieve/read), `Json` (corrupted handle
file), `Database`, `Libsql` — maps to the **same** `-32603 InternalError`. Clients
cannot programmatically distinguish:
- a malformed handle (`validate_handle` → `Config`, `response_handles.rs:134-146`),
- a missing `handle` argument (`handle_retrieve` → `Config`,
  `handlers/mod.rs:172-178`),
- a corrupted/unparseable handle file (`serde_json::from_str` → `Json`,
  `response_handles.rs:81`),
- a disk I/O failure reading the file (`fs::read_to_string` → `Io`,
  `response_handles.rs:80`).

All become `-32603 "tool execution failed: {e}"`. Worse, the first two are
*client input problems* that the JSON-RPC spec would class as
`InvalidParams (-32602)`, but they surface as `InternalError`. The structured
`data` field of the error object is always `None` (`transport.rs:64-75`), so there
is nowhere for a machine-readable reason code even if a client wanted one.

### 3.4 No expiry sweep → unbounded on-disk growth
`cleanup_expired_response_handles` is never called (see §2). Only retrieval of an
*identical* truncated payload after expiry reclaims space. For a long-lived
project this means `.tracedecay/response-handles/` grows without bound.

### 3.5 Clock skew / monotonicity
`now` comes from `current_timestamp()` (wall clock) at both store and retrieve.
A backward clock jump (NTP step, VM migration) can make a handle appear valid
past its nominal expiry; a forward jump expires it early. `expires_at` uses
`saturating_add` so there is no overflow panic at `i64::MAX`. Minor robustness
note, not a correctness bug under normal conditions.

### 3.6 Temp-file naming is per-process, not per-call
The atomic-write temp path is `<handle>.json.tmp.<pid>`
(`response_handles.rs:59`). Two concurrent stores of *different* content write
different handle paths, so they don't collide. Two concurrent stores of
*identical* content (same handle path) within one process share the same temp
name and clobber each other — harmless because the payloads are identical, but
worth noting if the temp name is ever derived from non-deterministic data.

### 3.7 No observability for the handle cache
There is no `tracing`/`log`/metric around handle create, retrieve hit/miss,
expiry, or store failure (confirmed: the only request-path logging is the generic
`eprintln!("[tracedecay] tool call: {tool_name}")` at `server.rs:1425`).
`tracedecay_status` (`server_stats_json`, `server.rs:1663`) reports nothing about
the response-handle cache (count, bytes, oldest age).

---

## 4. Recommendations

### 4.1 Metrics / log points
- Counters: handle `store_attempted`, `store_failed`, `retrieve_hit`,
  `retrieve_miss`, `retrieve_expired`, `retrieve_malformed`. Emit at
  `store_response_handle` / `retrieve_response_handle`.
- **Warn-log the swallowed store failure** in
  `truncated_json_envelope_with_handle` (`handlers/mod.rs:130-131`) — replace
  `.ok()` with a logged `match`, or at minimum `tracing::warn!` on `Err`.
- Surface cache stats in `tracedecay_status`: file count, total bytes, oldest
  `expires_at`, last sweep time.

### 4.2 Error semantics
- Map client-input failures to `InvalidParams (-32602)`: missing `handle`
  argument and `validate_handle` failure should not be `InternalError`. This
  requires either a typed result from the handler (so the dispatcher can pick the
  code) or special-casing `TraceDecayError::Config` originating from the
  retrieve/handle path in `handle_tools_call`.
- Give clients a way to branch on retrieve outcome. Either (a) split the not-found
  payload into `expired` vs `not_found` vs `corrupt`, or (b) populate the
  JSON-RPC error `data` field (`transport.rs:87` is already `Option<Value>`, just
  unused) with a machine-readable `{ "reason": "..." }` object so a `-32603` is
  at least introspectable. Keep the existing success/not-found payload shapes for
  compatibility (see §5).

### 4.3 Wire up the sweep
Call `cleanup_expired_response_handles` at server startup (and optionally on a
periodic timer or graceful shutdown) to bound disk growth. It is already
implemented and tested-in-spirit; it just needs a caller.

### 4.4 Reduce silent loss
- When `store_response_handle` fails, prefer downgrading the envelope to a
  *smaller* inlined payload (shrink `preview` further) rather than emitting a
  handle-less truncated envelope, **or** at minimum add an explicit
  `"handle_available": false` + `"reason"` field so clients can tell
  "truncated, no handle by design" from "truncated, handle store failed".
- Consider extending `truncated_json_envelope_with_handle` to the high-value read
  tools that currently use irreversible `truncate_response`
  (`tracedecay_read`, `tracedecay_body`, `tracedecay_outline`, graph trace tools),
  or document explicitly which tools are reversible.

---

## 5. Behavior that must not change (compatibility)

The following are exercised by tests or referenced by agent-facing docs and must
be preserved:

- **`tracedecay_retrieve` accepts only the canonical `handle` field.** Passing an
  alias (e.g. `retrieve_handle`) must error (`tests/mcp_handler_test.rs:571-582`).
  The `handle` param description must mention the required `handle` argument
  (`tests/mcp_handler_test.rs:503-506`).
- **On-disk record shape** is `{ created_at, expires_at, content }` with **no**
  `handle` / `original_chars` keys
  (`tests/mcp_handler_test.rs:552-553`).
- **Retrieve response shapes**: success
  `{handle, expired:false, original_chars, created_at, expires_at, content}` and
  not-found `{handle, expired:true, content:null}`. The `expired` boolean and
  `content` key are the contract clients/agents depend on
  (`tests/mcp_handler_test.rs:567-569`).
- **Envelope keys when a handle is present**: `truncated`, `original_chars`,
  `preview_chars`, `preview`, `handle`, `retrieve_tool`, `retrieve_ttl_seconds`,
  `retrieve_expires_at`, `retrieve_instruction`
  (`handlers/mod.rs:138-163`). Agent skill docs
  (`cursor-plugin/skills/*/SKILL.md`) instruct clients to read `handle` and call
  `tracedecay_retrieve`, so `handle` and `retrieve_tool` especially must remain.
- **Handle format** `rh_` + 24 lowercase hex chars
  (`response_handles.rs:119-146`); `validate_handle` enforces prefix + length +
  hex. Clients/skills may pattern-match this.
- **TTL constant** `86_400` is surfaced to clients as `retrieve_ttl_seconds` /
  `retrieve_expires_at`; the *field* must remain. Changing the default value is
  acceptable (clients read it dynamically), but a change invalidates in-flight
  handles on upgrade.
- **Content-addressed determinism** (same content → same handle) is relied on for
  de-dup. Changing the hash input or truncation length invalidates every existing
  handle in one shot — do this only behind an explicit cleanup.

---

## 6. Quick-reference index

| Concern | Location |
|---|---|
| Budget constant (15 000 bytes) | `src/mcp/tools/mod.rs:20` |
| Irreversible text truncation | `src/mcp/tools/handlers/mod.rs:99` (`truncate_response`) |
| Reversible envelope + handle | `src/mcp/tools/handlers/mod.rs:123` (`truncated_json_envelope_with_handle`) |
| Swallowed store error (`.ok()`) | `src/mcp/tools/handlers/mod.rs:130-131` |
| `tracedecay_retrieve` handler | `src/mcp/tools/handlers/mod.rs:172` (`handle_retrieve`) |
| Handle store / retrieve / sweep | `src/mcp/response_handles.rs:44,71,94` |
| Handle derivation + validation | `src/mcp/response_handles.rs:119,134` |
| TTL constant | `src/mcp/response_handles.rs:16` |
| Error → JSON-RPC collapse | `src/mcp/server.rs:1654-1658` |
| Error type variants | `src/errors.rs:6-37` |
| Locked retrieve/store contracts | `tests/mcp_handler_test.rs:532-583` |
| Agent-facing handle guidance | `cursor-plugin/skills/reading-code-cheaply/SKILL.md`, `searching-for-code/SKILL.md`, `tracing-functions/SKILL.md`, `reviewing-a-diff/SKILL.md` |
