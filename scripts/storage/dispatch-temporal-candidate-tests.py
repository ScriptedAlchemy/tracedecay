#!/usr/bin/env python3
"""Route the registered temporal candidate tests through the engine's query arms.

The two candidate tests in session-temporal-store retrieval/tests.rs ran the
FTS5 query constants against whatever engine the registered runtime links.
This rewrites, inside those two test functions only, each FTS-bearing query
constant to its backend dispatcher and every pinned `BackendKind::Sqlite`
search-syntax argument to the read's own backend. `EXPLAIN QUERY PLAN`
assertions run only where SQLite produced a plan. Rerunning is a no-op.
"""

import re
import sys
from pathlib import Path

TESTS = [
    "candidate_queries_return_live_rows_and_use_schema_indexes",
    "summary_and_derived_candidate_queries_enforce_live_boundaries_and_plans",
]

DISPATCH = {
    "OCCURRENCE_FTS_QUERY": "occurrence_fts_query",
    "SUMMARY_CANDIDATE_QUERY": "summary_candidate_query",
    "ROOT_OCCURRENCE_FTS_QUERY": "root_occurrence_fts_query",
    "ROOT_EXACT_CANDIDATE_QUERY": "root_exact_candidate_query",
    "ROOT_SUMMARY_CANDIDATE_QUERY": "root_summary_candidate_query",
    "DERIVED_CANDIDATE_QUERY": "derived_candidate_query",
    "ROOT_DERIVED_CANDIDATE_QUERY": "root_derived_candidate_query",
}


def function_span(text: str, name: str) -> tuple[int, int]:
    start = text.index(f"async fn {name}()")
    end = text.index("\n}\n", start) + 3
    return start, end


def assert_end(body: str, start: int) -> int:
    """Index just past the `);` closing the `assert!(` at `start`."""
    depth = 0
    index = body.index("(", start)
    while True:
        character = body[index]
        if character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
            if depth == 0:
                return body.index(";", index) + 1
        index += 1


def gate_plans(body: str) -> str:
    """Plans become `Option`s that only SQLite fills, and every assertion over
    one runs only when the plan exists."""
    plans = set(re.findall(r"let (\w+_plan) = read\s*\.explain_query_plan\(", body))
    body = re.sub(
        r"(let \w+_plan = read\s*)\.explain_query_plan\(", r"\1.sqlite_plan(", body
    )
    output, cursor = [], 0
    for match in re.finditer(r"assert!\(\s*(\w+)", body):
        name = match.group(1)
        if name not in plans or match.start() < cursor:
            continue
        end = assert_end(body, match.start())
        output.append(body[cursor : match.start()])
        output.append(f"if let Some({name}) = &{name} {{\n{body[match.start():end]}\n}}")
        cursor = end
    output.append(body[cursor:])
    return "".join(output)


def rewrite(body: str) -> str:
    body = body.replace(
        "tracedecay_runtime_core::db::engine::BackendKind::Sqlite", "backend"
    )
    constant = re.compile(r"\b(" + "|".join(DISPATCH) + r")\b")
    body = constant.sub(lambda match: f"{DISPATCH[match.group(1)]}(backend)", body)
    if "let backend = read.backend();" not in body:
        body = body.replace(
            "let read = runtime.retrieval_read_for_test().await;\n",
            "let read = runtime.retrieval_read_for_test().await;\n    let backend = read.backend();\n",
            1,
        )
    return gate_plans(body)


def main() -> None:
    path = Path(sys.argv[1])
    text = path.read_text()
    for name in TESTS:
        start, end = function_span(text, name)
        text = text[:start] + rewrite(text[start:end]) + text[end:]
    path.write_text(text)


if __name__ == "__main__":
    main()
