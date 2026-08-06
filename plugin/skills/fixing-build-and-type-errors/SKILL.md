---
name: fixing-build-and-type-errors
description: 'Use when compiler or type errors are in play: pasted "cargo check"/clippy/tsc/pyright output, a failed build, or a planned type check. Trigger before running those in the shell — tracedecay maps diagnostics to the enclosing symbol with callers. Do NOT use for test failures.'
---

# Fixing build & type errors

```
NO RAW `cargo check`/`tsc` IN THE SHELL WHEN A STRUCTURED PATH EXISTS,
AND NO FIX WITHOUT THE ENCLOSING SYMBOL AND ITS CALLERS IN VIEW.
```

Announce: "Using tracedecay:fixing-build-and-type-errors."

## Choose the entry — this fork matters

| Situation | Call | Cost |
|---|---|---|
| Compiler output already on hand (pasted or captured) | `tracedecay_diagnose` (`cargo_output`, `include_callers?`) — parses text, maps each error to the smallest containing node with up to 5 callers. Rust/cargo only. | Free — no toolchain run |
| Need retained diagnostics | `tracedecay_diagnostics` / `tracedecay_diagnostics_read` (`scope`: `workspace` \| `file`+`path`) — canonical clean-generation diagnostics, structured and node-mapped. | Read-only; configured producers publish new diagnostics through their owned lifecycle. |

Always prefer the free row. Respect the host's approval/run-mode before
running fresh toolchain checks.

## LSP server discovery

When an editor-language-server availability question is material to the
diagnosis, run `tracedecay lsp servers --json`. This is a CLI-only,
informational inventory of detected servers and install hints; it neither
starts nor installs a server. It is not an MCP tool and must not be added to a
host MCP allowlist.

## Fix loop

1. Map each error class to its cheapest anchoring tool with
   [references/error-class-to-tool.md](references/error-class-to-tool.md)
   (undefined symbol → search+signature; arity mismatch → callers; missing
   field → constructors; trait bound → implementations; etc.).
2. Understand the failing code via the `tracedecay:exploring-code` ladder;
   widen with `tracedecay_impact` when the fix touches a hub.
3. Apply the fix via `tracedecay:editing-safely`.
4. Re-check with the cheapest applicable path (paste new output into
   `diagnose`, or `diagnostics` with `scope: "file"`), then verify behavior via
   `tracedecay:assessing-impact`.
5. The same error twice after a fix, or 3+ failed fixes → stop patching;
   re-derive the root cause from callers and types before another attempt.

## If tools are deferred or MCP fails

- Deferred: one ToolSearch call —
  `select:tracedecay_diagnose,tracedecay_diagnostics,tracedecay_search,tracedecay_signature,tracedecay_callers`.
- MCP error: `tracedecay tool diagnose --cargo-output @/tmp/err.txt` (the `@`
  reads a file) / `tracedecay tool diagnostics --scope file --path …` (see
  `tracedecay:using-the-cli`). Only if the CLI is also unavailable, run the
  raw toolchain and paste its output back through `diagnose` when possible.

## Deliverable

Do not end while errors remain unexplained: deliver the grouped diagnostics
with enclosing symbols and callers, the applied fix per error class, and a
clean re-check (or a precise statement of what still fails and why). Report
any `tracedecay_metrics:` line.
