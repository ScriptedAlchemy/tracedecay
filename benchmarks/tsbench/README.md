# tsbench — tracedecay run

Adapts [`Mibayy/tsbench`](https://github.com/Mibayy/tsbench) (the 96-task
agent benchmark token-savior uses to publish its 97.9% score) to drive
tracedecay instead of token-savior, then reports the result.

Headline: **184 / 192 (95.8%)** on the first untuned run. Full breakdown in
[`SUMMARY.md`](SUMMARY.md). See also
[`docs/TRACEDECAY-VS-TOKENSAVIOR.md`](../../docs/TRACEDECAY-VS-TOKENSAVIOR.md) §4.

## Reproduce

```bash
# 1. Clone tsbench
git clone --depth=1 https://github.com/Mibayy/tsbench /tmp/tsbench
cd /tmp/tsbench

# 2. Apply the tracedecay fork patch
patch -p0 < /path/to/tracedecay/benchmarks/tsbench/bench_tracedecay.patch
#    -> produces bench_tracedecay.py alongside the original bench.py

# 3. Index the synthetic project with tracedecay
tracedecay init .

# 4. Run all 96 tasks
TRACEDECAY_BIN=$(which tracedecay) TSBENCH_BARE=0 \
  python3 bench_tracedecay.py --tasks all --run B

# 5. Per-task JSON appears in ./results-tracedecay/raw/
#    Aggregate stats with:
python3 - <<'PY'
import json, pathlib
files = sorted(pathlib.Path("results-tracedecay/raw").glob("TASK-*-run-B.json"))
score = sum(json.loads(f.read_text())["score"] for f in files)
print(f"{score}/{2*len(files)} = {score/(2*len(files))*100:.1f}%")
PY
```

## What the patch changes (vs. upstream `bench.py`)

- **MCP config** — launches `tracedecay serve -p <root> --timings` instead of
  `token_savior.server` over Python stdio.
- **System prompt** — rewrites `SYSTEM_PROMPT_TS` to map each token-savior
  tool to its tracedecay equivalent (`find_symbol` →
  `tracedecay_find_exact_symbol`, `get_function_source` → `tracedecay_body`,
  `get_full_context` → `tracedecay_context`, etc.). Where no tracedecay
  equivalent exists (`add_field_to_model`, `move_symbol`,
  `analyze_config`, `analyze_docker`), the prompt explicitly allows
  `Read` / `Edit` fallback.
- **`--disallowedTools`** — relaxed from
  `["Read","Grep","Glob","Agent"]` to `["Agent"]` only, since the four
  fallback task categories need text-level tools.
- **Tool-prefix matcher** — `ts_prefixes = ("mcp__tracedecay__",)`.
- **Results path** — `results-tracedecay/raw/` (so a tracedecay run doesn't
  overwrite token-savior's `results/raw/`).
- **Seed-session filename** — `.bench-tracedecay-session-id`.
- **`CLAUDE_PROJECT_ROOT`** env var — set to `ROOT` (the local repo) instead
  of the hard-coded `/root/projects/tsbench`.

## Environment

- `TRACEDECAY_BIN` — path to the tracedecay binary. Defaults to the release
  build in the canonical checkout location.
- `TSBENCH_BARE` — set to `0` on macOS / Max OAuth (default is `1`, but
  `--bare` mode broke OAuth in our environment). On Linux + API key, leave
  default.
- `TSBENCH_MODEL` — defaults to `claude-opus-4-7`.

## License

The original `bench.py` is MIT (`Mibayy/tsbench`). The patch in this
directory is contributed back under the same terms.
