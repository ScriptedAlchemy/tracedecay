#!/usr/bin/env python3
"""Adoption scorecard: turn a hermetic run's ``results.jsonl`` into a
fact-store adoption metric.

The hermetic harness (``evals/hermetic/run.sh`` + ``score.py``) emits one scored
JSON object per scenario into ``results.jsonl``. Each row carries at least:

    id, category, rep, pass (bool), expected_tools_missing, verify_pass, ...

This tool answers a single question the raw pass count cannot: **how often does
an agent actually reach for the fact store when it has the opportunity to?**

The corpus tags fact-store scenarios with a category of ``factstore-<bucket>``:

    factstore-write     -> the agent should call ``tracedecay_fact_store_add``
    factstore-recall    -> the agent should call ``tracedecay_fact_store_search``
    factstore-feedback  -> the agent should call ``tracedecay_fact_feedback``

For each bucket we report:

    opportunities  count of scenarios that *could* have triggered the behavior
    triggered      count of those scenarios that passed (behavior observed)
    adoption%      triggered / opportunities * 100

Definitions:

* A row "counts as an opportunity" for a bucket if its ``category`` is
  ``factstore-<bucket>``. With ``--corpus`` the denominator comes from the
  corpus (so scenarios that errored / were skipped / never produced a results
  row still count as a *missed* opportunity); without it the denominator is the
  rows actually present in ``results.jsonl``.
* A row "triggered" when it passed. Passing is read from the ``pass`` field
  (the boolean score.py emits); ``passed`` is accepted as a fallback alias.

Usage:

    python3 evals/hermetic/scorecard.py <results.jsonl> [--corpus <corpus.jsonl>]

Emits a human-readable table on stdout followed by a machine JSON block
(delimited so a baseline diff can extract it), exit 0 even on an empty or
missing results file (prints 0/0).
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

# Canonical fact-store buckets, in report order.
FACTSTORE_PREFIX = "factstore-"
BUCKETS = ("write", "recall", "feedback")

# Marker lines that fence the machine-readable JSON block in stdout so a
# baseline differ can slice it out without parsing the human table.
JSON_BEGIN = "----- SCORECARD-JSON BEGIN -----"
JSON_END = "----- SCORECARD-JSON END -----"


def bucket_of(category: str) -> str | None:
    """Return the fact-store bucket for a category, or None if not fact-store.

    ``factstore-write`` -> ``write``. An unknown ``factstore-*`` suffix maps to
    ``other`` so it still counts toward the overall fact-store denominator
    rather than being silently dropped.
    """
    if not isinstance(category, str):
        return None
    if not category.startswith(FACTSTORE_PREFIX):
        return None
    suffix = category[len(FACTSTORE_PREFIX):]
    return suffix if suffix in BUCKETS or suffix == "precision" else "other"


def row_triggered(row: dict) -> bool:
    """Did the scenario trigger the memory behavior, path-agnostically?

    tracedecay is reachable via BOTH the MCP tools (``tracedecay_fact_*``) and
    the ``tracedecay tool fact_*`` CLI, so neither counts as a miss. Ground
    truth is the ``verify_cmd`` store-state check when the scenario has one
    (fact added / helpful_count / unhelpful_count / fact_count unchanged);
    otherwise credit any MCP fact tool OR any CLI fact-command attempt.
    """
    if not isinstance(row, dict):
        return False
    if row.get("agent_completed") is False or row.get("anti_tools_used"):
        return False
    verify = row.get("verify_pass")
    if verify is not None:
        return bool(verify)
    mcp = row.get("tracedecay_tools") or []
    used_mcp = any("fact" in str(t).lower() for t in mcp)
    cli_attempts = row.get("tool_cmd_attempts") or 0
    if used_mcp or (isinstance(cli_attempts, int) and cli_attempts > 0):
        return True
    for key in ("pass", "passed"):
        if key in row:
            return bool(row.get(key))
    return False


# Backwards-compatible alias.
row_passed = row_triggered


def aggregate(results: list[dict], corpus: list[dict] | None = None, reps: int | None = None) -> dict:
    """Count each expected scenario/repetition once, including missing runs."""
    if reps is not None and (type(reps) is not int or reps < 1):
        raise ValueError("reps must be a positive integer")
    if any(type(row.get("rep", 1)) is not int or row.get("rep", 1) < 1 for row in results):
        raise ValueError("repetition must be a positive integer")
    indexed = any(row.get("scenario_index") is not None for row in results)
    if indexed and any(type(row.get("scenario_index")) is not int or row["scenario_index"] < 1 for row in results):
        raise ValueError("indexed results require a positive scenario_index on every row")
    identities = set()
    for row in results:
        identity = (row["scenario_index"] if indexed else row.get("id"), row.get("rep", 1))
        if identity in identities:
            raise ValueError(f"duplicate scenario/repetition: {identity}")
        identities.add(identity)
    observed_reps = max((row.get("rep", 1) for row in results), default=1)
    reps = observed_reps if reps is None else reps
    if reps < 1 or observed_reps > reps:
        raise ValueError("reps must cover every observed positive repetition")
    if corpus is not None and indexed:
        for row in results:
            index = row["scenario_index"]
            if index > len(corpus):
                raise ValueError("result scenario_index is outside the corpus")
            scenario = corpus[index - 1]
            if row.get("id") != scenario.get("id") or row.get("category", "") != scenario.get("category", ""):
                raise ValueError("result scenario/category does not match its corpus index")
    elif corpus is not None:
        categories = {scenario["id"]: scenario.get("category", "") for scenario in corpus}
        if len(categories) != len(corpus):
            raise ValueError("duplicate corpus scenario")
        if any(row.get("id") not in categories or row.get("category", "") != categories[row["id"]] for row in results):
            raise ValueError("result scenario/category does not match the corpus")
    order = list(BUCKETS)

    def blank() -> dict:
        return {"opportunities": 0, "triggered": 0}

    buckets: dict[str, dict] = {name: blank() for name in order}

    if corpus is not None:
        for scenario in corpus:
            b = bucket_of(scenario.get("category", "") if isinstance(scenario, dict) else "")
            if b is None:
                continue
            if b not in buckets:
                buckets[b] = blank()
                order.append(b)
            buckets[b]["opportunities"] += reps
    else:
        for row in results:
            b = bucket_of(row.get("category", "") if isinstance(row, dict) else "")
            if b is None:
                continue
            if b not in buckets:
                buckets[b] = blank()
                order.append(b)
            buckets[b]["opportunities"] += 1

    for row in results:
        b = bucket_of(row.get("category", "") if isinstance(row, dict) else "")
        if b is None:
            continue
        if b not in buckets:
            buckets[b] = blank()
            order.append(b)
        if row_passed(row):
            buckets[b]["triggered"] += 1

    for name in order:
        stats = buckets[name]
        stats["adoption_pct"] = adoption_pct(stats["triggered"], stats["opportunities"])

    # OVERALL adoption sums only the adoption buckets. `precision` measures
    # restraint (don't-store-ephemeral) and `other` is uncategorized, counting
    # correct abstention as adoption would bias the headline upward.
    total_opp = sum(buckets[n]["opportunities"] for n in order if n in BUCKETS)
    total_trig = sum(buckets[n]["triggered"] for n in order if n in BUCKETS)

    overall = {
        "opportunities": total_opp,
        "triggered": total_trig,
        "adoption_pct": adoption_pct(total_trig, total_opp),
    }

    feedback_pct = buckets.get("feedback", blank()).get("adoption_pct", 0.0)

    return {
        "buckets": {name: buckets[name] for name in order},
        "bucket_order": order,
        "overall": overall,
        "factstore_adoption_pct": overall["adoption_pct"],
        "feedback_adoption_pct": feedback_pct,
        "denominator_source": "corpus" if corpus is not None else "results",
    }


def adoption_pct(triggered: int, opportunities: int) -> float:
    """triggered / opportunities as a percentage, rounded to 2 dp; 0 when no
    opportunities (avoids divide-by-zero and keeps an empty run at 0.0)."""
    if opportunities <= 0:
        return 0.0
    return round(triggered / opportunities * 100.0, 2)


def read_jsonl(path: Path) -> list[dict]:
    """Read a JSONL file into a list of dicts, skipping blank/garbage lines.

    Missing or unreadable file -> empty list (graceful degrade)."""
    rows: list[dict] = []
    try:
        text = path.read_text()
    except OSError:
        return rows
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            rows.append(obj)
    return rows


def render_table(summary: dict) -> str:
    """Human-readable adoption table."""
    lines: list[str] = []
    lines.append("Fact-store adoption scorecard")
    lines.append(f"(denominator source: {summary['denominator_source']})")
    lines.append("")
    header = f"{'bucket':<12} {'opportunities':>13} {'triggered':>10} {'adoption%':>10}"
    lines.append(header)
    lines.append("-" * len(header))
    for name in summary["bucket_order"]:
        stats = summary["buckets"][name]
        lines.append(
            f"{name:<12} {stats['opportunities']:>13} {stats['triggered']:>10} "
            f"{stats['adoption_pct']:>9.2f}%"
        )
    lines.append("-" * len(header))
    ov = summary["overall"]
    lines.append(
        f"{'OVERALL':<12} {ov['opportunities']:>13} {ov['triggered']:>10} "
        f"{ov['adoption_pct']:>9.2f}%"
    )
    lines.append("")
    lines.append(f"Fact-store adoption %: {summary['factstore_adoption_pct']:.2f}%")
    lines.append(f"Feedback-loop adoption %: {summary['feedback_adoption_pct']:.2f}%")
    return "\n".join(lines)


def render_json_block(summary: dict) -> str:
    """Machine-readable JSON block, fenced so a baseline diff can extract it."""
    return "\n".join(
        [JSON_BEGIN, json.dumps(summary, indent=2, sort_keys=True), JSON_END]
    )


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Fact-store adoption scorecard from a hermetic results.jsonl"
    )
    ap.add_argument("results", help="path to results.jsonl from a hermetic run")
    ap.add_argument(
        "--corpus",
        help="optional corpus.jsonl; used as the opportunity denominator so "
        "errored/skipped scenarios still count as missed opportunities",
    )
    ap.add_argument("--reps", type=int, help="expected repetitions, including runs with no result rows")
    args = ap.parse_args(argv)

    results = read_jsonl(Path(args.results))
    corpus = read_jsonl(Path(args.corpus)) if args.corpus else None

    summary = aggregate(results, corpus, args.reps)

    print(render_table(summary))
    print()
    print(render_json_block(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
