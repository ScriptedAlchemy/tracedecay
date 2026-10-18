#!/usr/bin/env python3
"""Keep scripts/measure-ranked-tail-trim.py's replay of the trim rule equal to the Rust rule.

The rule cases live next to the Rust rule and drive its unit tests too, so a
rule change that updates them fails here until the replay follows.
"""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("measure-ranked-tail-trim.py")
CASES = Path(__file__).parents[1] / "crates/tracedecay-mcp/src/tools/render/ranked_tail_cases.json"
spec = importlib.util.spec_from_file_location("measure_ranked_tail_trim", SCRIPT)
trim = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = trim
spec.loader.exec_module(trim)

SEARCH_PAGE = "".join(
    [
        "## Search Results\n",
        "- **strong** (function, exact_message), rank 1 · utility 2000000\n",
        "  anchor_id: `code-symbol:strong`\n",
        "- **also_strong** (function, exact_message), rank 2 · utility 2000000\n",
        "  anchor_id: `code-symbol:also_strong`\n",
        "- **third** (function, exact_message), rank 3 · utility 2000000\n",
        "  anchor_id: `code-symbol:third`\n",
        "- **anchored** (function, exact_message), rank 4 · utility 1000000 · via query, anchor:anchored\n",
        "  anchor_id: `code-symbol:anchored`\n",
        "- **weak_a** (function, exact_message), rank 5 · utility 1000000 · via query\n",
        "  anchor_id: `code-symbol:weak_a`\n",
        "  Read source: `tracedecay_source_body` with `node_id: symbol.weak_a`\n",
        "- **code-symbol:weak_b** (approximate), rank 6 · utility 1000000\n",
        "  Read source: `tracedecay_source_body` with `node_id: symbol.weak_b`\n",
        "\n",
        "next_cursor: none\n",
    ]
)
CONTEXT_ROWS = "".join(
    f"- **m{rank}** (function), `src/retrieval/ranked_context_matches.rs` · rank {rank} · utility {2000000 if rank <= 3 else 1000000}\n"
    for rank in range(1, 7)
)


class SharedRuleCases(unittest.TestCase):
    cases = json.loads(CASES.read_text())

    def test_tail_start_matches_the_rust_cases(self) -> None:
        for case in self.cases["tail_start"]:
            rows = [
                trim.Row(row.get("name", f"r{index}"), index + 1, row["utility"], row.get("anchored", False), "")
                for index, row in enumerate(case["rows"])
            ]
            with self.subTest(case["case"]):
                self.assertEqual(trim.tail_start(rows, case["query"]), case["start"])

    def test_query_word_naming_matches_the_rust_cases(self) -> None:
        for case in self.cases["names_every_query_word"]:
            with self.subTest(name=case["name"], query=case["query"]):
                self.assertEqual(trim.names_every_query_word(case["name"], case["query"]), case["answers"])


class TrimMarkers(unittest.TestCase):
    MARKER = (
        "- _Trimmed 3 lower-ranked results (ranks 4-6, 120 bytes): each scored at most half of "
        "this page's best utility 2000000. To restore them, call `tracedecay_retrieve` with "
        "handle `rh_0123456789abcdef01234567` (pass the same `project_selector` if this call used one)._\n"
    )
    KEPT = "### Code\n- **kept** (function), src/lib.rs:1\n"
    TAIL = "- **a** (field), src/a.rs:4\n- **b** (field), src/b.rs:5\n- **c** (field), src/c.rs:6\n"
    PAGE = KEPT + MARKER + "\n### Related Symbols\n- **c** (method), src/c.rs:9\n"

    def stored(self, handle: str) -> str | None:
        return self.TAIL if handle == "rh_0123456789abcdef01234567" else None

    def test_binary_cuts_remove_each_marker_and_keep_its_rows(self) -> None:
        rest, cuts = trim.binary_cuts(self.PAGE, self.stored)
        self.assertEqual(rest, self.PAGE.replace(self.MARKER, ""))
        self.assertEqual(cuts, [trim.Cut(3, 120 - len(self.MARKER.encode()), self.TAIL)])

    def test_a_dead_handle_keeps_the_markers_own_counts(self) -> None:
        _, cuts = trim.binary_cuts(self.PAGE, lambda handle: None)
        self.assertEqual(cuts, [trim.Cut(3, 120 - len(self.MARKER.encode()), None)])

    def test_page_trim_counts_code_rows_only_the_binary_could_cut(self) -> None:
        page = trim.page_trim(self.PAGE, "unrelated", self.stored)
        self.assertEqual(page.untrimmed_bytes, len(self.PAGE.encode()) + 120 - len(self.MARKER.encode()))
        self.assertEqual((page.rows, page.unresolved), (3, 0))
        self.assertEqual(page.cut_rows, trim.listed_rows(self.TAIL))
        self.assertEqual(trim.LISTED.findall(page.shown), ["kept", "c"])

    def test_page_trim_replays_rows_the_page_still_ranks(self) -> None:
        page = trim.page_trim(SEARCH_PAGE, "unrelated", self.stored)
        self.assertEqual([trim.LISTED.findall(row) for row in page.cut_rows], [["weak_a"], ["code-symbol:weak_b"]])
        self.assertNotIn("weak_a", page.shown)


class RenderedPages(unittest.TestCase):
    def test_short_tail_saves_no_bytes_and_keeps_every_row(self) -> None:
        text = "".join(
            f"- **m{rank}** (fn) · rank {rank} · utility {2000000 if rank <= 3 else 1000000}\n"
            for rank in range(1, 6)
        )
        ((kept, cut, saved),) = trim.trimmed(text, "unrelated")
        self.assertEqual(([row.rank for row in kept], cut, saved), ([1, 2, 3, 4, 5], [], 0))

    def test_search_rows_keep_continuation_lines_and_anchor_routes(self) -> None:
        ((kept, cut, saved),) = trim.trimmed(SEARCH_PAGE, "unrelated")
        self.assertEqual([row.name for row in kept], ["strong", "also_strong", "third", "anchored"])
        self.assertEqual([row.name for row in cut], ["weak_a", "code-symbol:weak_b"])
        self.assertIn("node_id: symbol.weak_a", cut[0].text)
        cut_bytes = sum(len(row.text.encode()) for row in cut)
        self.assertEqual(saved, cut_bytes - len(trim.marker(cut, 2000000).encode()))

    def test_context_rows_trim_after_the_last_anchored_row(self) -> None:
        ((kept, cut, _),) = trim.trimmed(CONTEXT_ROWS, "unrelated")
        self.assertEqual(([row.rank for row in kept], [row.rank for row in cut]), ([1, 2, 3], [4, 5, 6]))
        anchored = CONTEXT_ROWS.replace("rank 6 · utility 1000000\n", "rank 6 · utility 1000000 · via anchor:m6\n")
        ((kept, cut, saved),) = trim.trimmed(anchored, "unrelated")
        self.assertEqual((len(kept), cut, saved), (6, [], 0))


if __name__ == "__main__":
    unittest.main()
