#!/usr/bin/env bash
# diagnose-summary.sh — turn TraceDecay diagnostics into a mapped-owner summary.
#
# Runs the graph-aware diagnostic path this skill prescribes and prints: how many
# diagnostics were recognized, which symbol/file owns each failure, the callers
# that may break after a fix, and how many diagnostics went UNMAPPED (a parse- or
# file-mapping-coverage gap, not an unimportant error).
#
# Two modes, matching the skill workflow:
#   # 1. You already have raw compiler stderr -> map it (fast, no build):
#   cargo check 2>&1 | scripts/diagnose-summary.sh
#   # 2. No stderr yet -> run the structured checker (first cold call can take
#   #    minutes; it uses an isolated target dir so it will not race your cargo):
#   scripts/diagnose-summary.sh
set -euo pipefail
PY=python3; command -v "$PY" >/dev/null 2>&1 || PY=python

# Choose the source: piped stderr -> diagnose; otherwise -> diagnostics.
if [ ! -t 0 ]; then
  STDERR="$(cat)"
  if [ -z "${STDERR//[$'\t\r\n ']/}" ]; then echo "no compiler output on stdin; nothing to map."; exit 0; fi
  RAW="$(printf '%s' "$STDERR" | tracedecay tool diagnose --cargo-output @- --format json --include-callers true 2>/dev/null)"
  SRC="diagnose (parsed from piped stderr)"
else
  echo "No stdin: running 'tracedecay tool diagnostics' (first cold call may take minutes)..." >&2
  RAW="$(tracedecay tool diagnostics --format json 2>/dev/null)"
  SRC="diagnostics (ran the project type-checker)"
fi

RAW="$RAW" SRC="$SRC" "$PY" <<'PYEOF'
import os, json, sys
src = os.environ["SRC"]
raw = os.environ["RAW"]
# Strip the trailing "tracedecay_metrics: ..." line (and blanks); keep it for savings.
metrics, body = None, []
for ln in raw.splitlines():
    if ln.startswith("tracedecay_metrics:"): metrics = ln
    elif ln.strip(): body.append(ln)
try:
    d = json.loads("\n".join(body))
except Exception as e:
    print("could not parse diagnostics JSON:", e); sys.exit(0)

diags = d.get("diagnostics") or []
parsed   = d.get("diagnostics_parsed", d.get("parsed", len(diags)))
returned = d.get("diagnostics_returned", len(diags))
mapped   = d.get("mapped_to_node")
unmapped = d.get("unmapped")
if mapped is None:   mapped   = sum(1 for x in diags if (x.get("node") or x.get("enclosing_node")))
if unmapped is None: unmapped = sum(1 for x in diags if not (x.get("node") or x.get("enclosing_node")))

def sev(x):  return x.get("severity") or x.get("level") or "?"
def node(x): return x.get("node") or x.get("enclosing_node") or x.get("symbol")

errs  = sum(1 for x in diags if sev(x) == "error")
warns = sum(1 for x in diags if sev(x) == "warning")

print("=" * 60)
print(" TraceDecay diagnostics summary")
print("=" * 60)
print(f"source        : {src}")
print(f"recognized    : {parsed} parsed, {returned} returned  ({errs} error, {warns} warning)")
print(f"mapped/unmapped: {mapped} mapped to a symbol, {unmapped} UNMAPPED")
if d.get("truncated"): print("note          : output truncated (raise --max-diagnostics for more)")
if not diags:
    print("\nclean — no diagnostics with a resolvable file:line span.")

# Group by mapped owner so shared root causes cluster.
from collections import defaultdict
by_owner = defaultdict(list)
unmapped_hits = []
for x in diags:
    n = node(x)
    tag = f"{sev(x)} {x.get('code') or ''}".strip() + f": {x.get('message','')[:70]}"
    loc = f"{x.get('file','?')}:{x.get('line','?')}"
    if n:
        key = f"{n.get('kind','?')} {n.get('qualified_name') or n.get('name','?')}"
        by_owner[key].append((loc, tag, len(x.get("callers") or [])))
    else:
        unmapped_hits.append((loc, tag))

if by_owner:
    print("\n## Mapped owners (fix these; callers may break)")
    for owner, hits in sorted(by_owner.items(), key=lambda kv: -len(kv[1])):
        callers = max((c for _, _, c in hits), default=0)
        print(f"\n  {owner}   [{len(hits)} diagnostic(s), up to {callers} caller(s)]")
        for loc, tag, _ in hits[:6]:
            print(f"    - {loc}  {tag}")

if unmapped_hits:
    print("\n## UNMAPPED (parse/file-mapping coverage gap — still real errors)")
    for loc, tag in unmapped_hits[:10]:
        print(f"    - {loc}  {tag}")
    print("  -> If these own real code, that is a TraceDecay extractor/mapping gap worth an issue.")

if metrics:
    import re
    m = re.search(r"before=(\d+)\s+after=(\d+)", metrics)
    if m:
        b, a = int(m.group(1)), int(m.group(2))
        print(f"\nTraceDecay'd ~{b-a} tokens ({b} -> {a}).")
PYEOF
