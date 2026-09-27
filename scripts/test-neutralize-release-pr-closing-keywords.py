#!/usr/bin/env python3
"""The release PR body rewrite must leave no GitHub closing keyword."""

from __future__ import annotations

import importlib.util
import re
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT_PATH = ROOT / "scripts/neutralize-release-pr-closing-keywords.py"

# GitHub matches these case-insensitively, with an optional colon, against
# #N, GH-N, owner/repo#N, and an issues URL. Lowercasing inside a code span
# still closes the issue.
CLOSING_KEYWORD = re.compile(
    r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\b"
    r"\s*:?\s*"
    r"(?:#\d+|GH-\d+|[\w.-]+/[\w.-]+#\d+|https://github\.com/[\w.-]+/[\w.-]+/issues/\d+)"
)

LITERAL_BODY = "Fixes #1\ncloses #2\nResolves #3\n"


def load_script():
    spec = importlib.util.spec_from_file_location(
        "neutralize_release_pr_closing_keywords",
        SCRIPT_PATH,
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {SCRIPT_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class NeutralizeReleasePrClosingKeywordsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.rewriter = load_script()

    def test_literal_body_keeps_no_closing_keyword(self) -> None:
        self.assertIsNotNone(CLOSING_KEYWORD.search(LITERAL_BODY))
        rewritten = self.rewriter.neutralize_closing_keywords(LITERAL_BODY)
        self.assertIsNone(CLOSING_KEYWORD.search(rewritten))
        self.assertEqual(rewritten, "Refs #1\nRefs #2\nRefs #3\n")

    def test_other_references_are_neutralized_and_prose_is_kept(self) -> None:
        samples = {
            "Fixed: #9": "Refs: #9",
            "Closes ScriptedAlchemy/tracedecay#1226": "Refs ScriptedAlchemy/tracedecay#1226",
            "Resolve https://github.com/ScriptedAlchemy/tracedecay/issues/1226": (
                "Refs https://github.com/ScriptedAlchemy/tracedecay/issues/1226"
            ),
            "Fixes GH-8": "Refs GH-8",
            "This fixes the parser.": "This fixes the parser.",
            "See #4.": "See #4.",
        }
        for raw, expected in samples.items():
            rewritten = self.rewriter.neutralize_closing_keywords(raw)
            self.assertEqual(rewritten, expected)
            if CLOSING_KEYWORD.search(raw):
                self.assertIsNone(CLOSING_KEYWORD.search(rewritten))


if __name__ == "__main__":
    unittest.main(verbosity=2)
