#!/usr/bin/env python3
"""Score the session-recall hint policy against a running Laya server.

Sends each prompt in a case file to `laya-serve` with the same question the
Codex prompt hook sends (`recall_model.rs`), then reports how many recall
requests the keyword predicate, the model, and the shipped keyword-then-model
policy catch, and how many non-recall prompts each wrongly hints.

    laya-serve &                      # pip install 'laya[serve]'
    python3 evals/hint_recall_model/run.py --url http://127.0.0.1:8000
"""

import argparse
import json
import statistics
import sys
import time
import urllib.request
from pathlib import Path

# Keep in sync with RECALL_QUESTION and RECALL_PROBABILITY_THRESHOLD in
# crates/tracedecay-agent-hosts/src/hooks/tool_hints/recall_model.rs.
RECALL_QUESTION = (
    "Is the user asking to look up or reuse something from an earlier conversation, "
    "a previous session, or another agent's transcript?"
)
DEFAULT_THRESHOLD = 0.6

# Baseline only: the phrase lists of asks_for_session_recall
# (tool_hints/classifiers.rs) as of this eval. The Rust eval matrix, not this
# copy, is the authority on keyword behavior.
PRIOR_CONTEXT = [
    "where did we", "what did we", "when did we", "did we talk", "remind me",
    "remember when", "last time", "earlier we", "talk about", "discuss before",
    "mentioned before", "previously discussed", "last run", "prior run",
    "previous run", "automation run", "self-improvement run", "past session",
    "prior conversation", "previous conversation", "earlier conversation",
    "context length exceeded", "cannot compress further", "compacted context",
    "compaction", "context was compacted", "session search", "session recall",
    "conversation history",
]
RAW_TRANSCRIPT = [
    "raw codex jsonl transcript", "raw codex jsonl transcripts", "hook input",
    "hook usage", "hint displayed", "hints displayed", "model gets", "user submitted",
]


def keyword_recall(prompt):
    text = prompt.lower()
    has = lambda needles: any(needle in text for needle in needles)  # noqa: E731
    return has(PRIOR_CONTEXT) or has(RAW_TRANSCRIPT) or (
        has(["lcm sessions", "past sessions", "prior sessions"])
        and has(["check", "search", "find", "look", "review"])
    )


def recall_probability(url, prompt):
    body = json.dumps({
        "state": prompt,
        "questions": {"recall": {"type": "noul", "instructions": RECALL_QUESTION}},
    }).encode()
    request = urllib.request.Request(
        f"{url.rstrip('/')}/v1/systemone", data=body,
        headers={"Content-Type": "application/json"},
    )
    started = time.perf_counter()
    with urllib.request.urlopen(request, timeout=30) as response:
        answer = json.load(response)
    return answer["answers"]["recall"]["noul"], (time.perf_counter() - started) * 1000


def main():
    here = Path(__file__).resolve().parent
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--url", default="http://127.0.0.1:8000")
    parser.add_argument("--cases", type=Path, default=here / "heldout.json")
    parser.add_argument("--threshold", type=float, default=DEFAULT_THRESHOLD)
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()

    cases = json.loads(args.cases.read_text())
    tally = {name: {"caught": 0, "false_hints": 0} for name in ("keyword", "model", "policy")}
    recall_total = sum(case["expected"] == "SessionRecall" for case in cases)
    latencies = []
    for case in cases:
        probability, ms = recall_probability(args.url, case["prompt"])
        latencies.append(ms)
        keyword = keyword_recall(case["prompt"])
        model = probability >= args.threshold
        verdicts = {"keyword": keyword, "model": model, "policy": keyword or model}
        is_recall = case["expected"] == "SessionRecall"
        for name, hinted in verdicts.items():
            if hinted and is_recall:
                tally[name]["caught"] += 1
            elif hinted:
                tally[name]["false_hints"] += 1
        if args.verbose and verdicts["policy"] != is_recall:
            print(f"  miss p={probability:.2f} expected={case['expected']}: {case['prompt']}")

    print(f"{len(cases)} prompts, {recall_total} recall requests, threshold {args.threshold}")
    for name, counts in tally.items():
        print(f"  {name:8} caught {counts['caught']}/{recall_total}, "
              f"false hints {counts['false_hints']}/{len(cases) - recall_total}")
    print(f"  model latency median {statistics.median(latencies):.0f} ms "
          f"(includes HTTP; hardware-dependent)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
