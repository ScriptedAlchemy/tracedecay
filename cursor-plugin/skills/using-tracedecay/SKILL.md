---
name: using-tracedecay
description: 'Use when starting any session or task in a TraceDecay-indexed project — establishes when tracedecay tools and skills are mandatory, maps common task moments to the right tool, and rebuts every rationalization for falling back to native grep, glob, or file reads.'
---

# Using TraceDecay

This project has a live TraceDecay code graph. If there is even a 1% chance a
tracedecay tool or skill applies to what you are doing, you MUST use it. This
is not a preference or a tie-breaker: for any codebase question — finding
code, reading code, tracing calls, estimating blast radius, recalling prior
context — try the matching tracedecay tool BEFORE Grep, Glob, codebase
search, or file reads. You cannot rationalize your way out of this.

## Moment → mandatory action

| The moment you are in | Do this first |
|---|---|
| About to Grep/Glob/codebase-search for a symbol or concept | `tracedecay_search` (names) or `tracedecay_context` (concepts) — skill: `tracedecay:exploring-code` |
| About to open or Read a source file | `tracedecay_outline` → `tracedecay_body` → `tracedecay_read` slices — skill: `tracedecay:exploring-code` |
| Asked "who calls X" / "what does X call" / "trace this" | `tracedecay_callers` / `tracedecay_callees` — skill: `tracedecay:tracing-functions` |
| About to change code and wondering what breaks or which tests to run | `tracedecay_impact` / `tracedecay_diff_context` / `tracedecay_affected` — skill: `tracedecay:assessing-impact` |
| About to write a new helper, rename, or do a mechanical edit | `tracedecay:editing-safely` (duplicate probe, rename recon, anchored edits) |
| Reviewing a diff, auditing risk, or drafting commit/PR text | `tracedecay:reviewing-changes` |
| Asked about architecture, tech debt, or project/index status | `tracedecay:code-health` |
| The user references prior decisions or past conversations | `tracedecay:recalling-project-memory` / `tracedecay:recalling-session-context` |
| A compiler/type error needs context | `tracedecay:fixing-build-and-type-errors` |
| A tracedecay MCP call errors or times out | `tracedecay:using-the-cli` — never abandon tracedecay over transport |

## Red flags

These thoughts mean STOP — you are rationalizing:

| Thought | Reality |
|---|---|
| "Grep is faster for this" | `tracedecay_search` is one call and pre-ranked. |
| "I'll just read the whole file" | `tracedecay_outline` / `tracedecay_body` answer at a fraction of the tokens. |
| "This is a simple lookup" | Simple lookups are exactly what the graph is for. |
| "I already know this codebase" | The graph is fresher than your memory. Check it. |
| "The MCP call might fail" | The CLI fallback (`tracedecay tool <name>`) always works. |
| "I'll explore first, then use the skill" | The skills tell you HOW to explore. Check first. |
| "The skill is overkill here" | Simple things become complex. Use it. |

## Procedure

1. On every task (including questions), check the moment table above BEFORE
   the first tool call. If a row matches, follow it.
2. Announce which skill you are following ("Using `tracedecay:exploring-code`
   to …") so the choice is visible and deliberate.
3. Fall back to plain Grep/Glob/Read only for content the graph does not
   index (comments, string literals, prose, config bodies) or after
   tracedecay has pinpointed the exact files.
4. If a response is truncated with a `handle`, narrow the query or call
   `tracedecay_retrieve` — do not re-run broad queries or guess.
5. If any result includes a `tracedecay_metrics:` line, report the savings to
   the user.
