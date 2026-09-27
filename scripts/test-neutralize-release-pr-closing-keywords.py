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
# #N, GH-N, owner/repo#N, an issues URL, and a markdown link whose text is one
# of those. Lowercasing inside a code span still closes the issue.
CLOSING_KEYWORD = re.compile(
    r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\b"
    r"\s*:?\s*\[?"
    r"(?:#\d+|GH-\d+|[\w.-]+/[\w.-]+#\d+|https://github\.com/[\w.-]+/[\w.-]+/issues/\d+)"
)

LITERAL_BODY = "Fixes #1\ncloses #2\nResolves #3\n"

# The 1.0.0-beta.57 release PR body exactly as release-please wrote it.
RELEASE_PLEASE_BODY = """\
:robot: I have created a release *beep* *boop*
---


## [1.0.0-beta.57](https://github.com/ScriptedAlchemy/tracedecay/compare/v1.0.0-beta.56...v1.0.0-beta.57) (2026-09-27)


### Bug Fixes

* **automation:** retire unreferenced refused effect journals ([#2316](https://github.com/ScriptedAlchemy/tracedecay/issues/2316)) ([f0b18b4](https://github.com/ScriptedAlchemy/tracedecay/commit/f0b18b4525b65760999adebf624415fba4d393f4))
* **cli:** keep the installed version when codesign fails ([87d275d](https://github.com/ScriptedAlchemy/tracedecay/commit/87d275d2481093da94cc8fea71d9a2305b4a7891)), closes [#2231](https://github.com/ScriptedAlchemy/tracedecay/issues/2231)
* **cli:** type readiness waits and projectless refusals end to end ([#2314](https://github.com/ScriptedAlchemy/tracedecay/issues/2314)) ([95a63d4](https://github.com/ScriptedAlchemy/tracedecay/commit/95a63d4c6c119c1c8db91549d62d97c56d72cd24))
* **code-index:** wait for a lapsed source proof before refusing reads ([#2318](https://github.com/ScriptedAlchemy/tracedecay/issues/2318)) ([b80c426](https://github.com/ScriptedAlchemy/tracedecay/commit/b80c426c03f5a76673aa65ab5b75276fe863cffa))
* **config:** key pinned configuration by owning profile ([#2304](https://github.com/ScriptedAlchemy/tracedecay/issues/2304)) ([0477c09](https://github.com/ScriptedAlchemy/tracedecay/commit/0477c097eceb8ea80fb0f5ac1d5dd9c87392616e))
* **domain:** box project route detail so master clippy passes ([#2315](https://github.com/ScriptedAlchemy/tracedecay/issues/2315)) ([1a0287b](https://github.com/ScriptedAlchemy/tracedecay/commit/1a0287bcfd19b58fc98c706f99f73661297b5e7b)), closes [#2308](https://github.com/ScriptedAlchemy/tracedecay/issues/2308)
* **hosts:** remove install-created directories on uninstall ([#2309](https://github.com/ScriptedAlchemy/tracedecay/issues/2309)) ([453cd45](https://github.com/ScriptedAlchemy/tracedecay/commit/453cd459fbe27f735b725ce34aa51d5de1fac154))
* **release:** stop release PRs closing referenced issues ([5ad8106](https://github.com/ScriptedAlchemy/tracedecay/commit/5ad810677f3d5e0126a7160465783e1924b6adc5))

---
This PR was generated with [Release Please](https://github.com/googleapis/release-please). See [documentation](https://github.com/googleapis/release-please#release-please).
"""

# The same body after the release agent rewrote both references by hand.
RELEASE_PLEASE_BODY_NEUTRALIZED = """\
:robot: I have created a release *beep* *boop*
---


## [1.0.0-beta.57](https://github.com/ScriptedAlchemy/tracedecay/compare/v1.0.0-beta.56...v1.0.0-beta.57) (2026-09-27)


### Bug Fixes

* **automation:** retire unreferenced refused effect journals ([#2316](https://github.com/ScriptedAlchemy/tracedecay/issues/2316)) ([f0b18b4](https://github.com/ScriptedAlchemy/tracedecay/commit/f0b18b4525b65760999adebf624415fba4d393f4))
* **cli:** keep the installed version when codesign fails ([87d275d](https://github.com/ScriptedAlchemy/tracedecay/commit/87d275d2481093da94cc8fea71d9a2305b4a7891)), Refs [#2231](https://github.com/ScriptedAlchemy/tracedecay/issues/2231)
* **cli:** type readiness waits and projectless refusals end to end ([#2314](https://github.com/ScriptedAlchemy/tracedecay/issues/2314)) ([95a63d4](https://github.com/ScriptedAlchemy/tracedecay/commit/95a63d4c6c119c1c8db91549d62d97c56d72cd24))
* **code-index:** wait for a lapsed source proof before refusing reads ([#2318](https://github.com/ScriptedAlchemy/tracedecay/issues/2318)) ([b80c426](https://github.com/ScriptedAlchemy/tracedecay/commit/b80c426c03f5a76673aa65ab5b75276fe863cffa))
* **config:** key pinned configuration by owning profile ([#2304](https://github.com/ScriptedAlchemy/tracedecay/issues/2304)) ([0477c09](https://github.com/ScriptedAlchemy/tracedecay/commit/0477c097eceb8ea80fb0f5ac1d5dd9c87392616e))
* **domain:** box project route detail so master clippy passes ([#2315](https://github.com/ScriptedAlchemy/tracedecay/issues/2315)) ([1a0287b](https://github.com/ScriptedAlchemy/tracedecay/commit/1a0287bcfd19b58fc98c706f99f73661297b5e7b)), Refs [#2308](https://github.com/ScriptedAlchemy/tracedecay/issues/2308)
* **hosts:** remove install-created directories on uninstall ([#2309](https://github.com/ScriptedAlchemy/tracedecay/issues/2309)) ([453cd45](https://github.com/ScriptedAlchemy/tracedecay/commit/453cd459fbe27f735b725ce34aa51d5de1fac154))
* **release:** stop release PRs closing referenced issues ([5ad8106](https://github.com/ScriptedAlchemy/tracedecay/commit/5ad810677f3d5e0126a7160465783e1924b6adc5))

---
This PR was generated with [Release Please](https://github.com/googleapis/release-please). See [documentation](https://github.com/googleapis/release-please#release-please).
"""


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

    def test_release_please_markdown_link_body_is_neutralized(self) -> None:
        self.assertIsNotNone(CLOSING_KEYWORD.search(RELEASE_PLEASE_BODY))
        rewritten = self.rewriter.neutralize_closing_keywords(RELEASE_PLEASE_BODY)
        self.assertEqual(rewritten, RELEASE_PLEASE_BODY_NEUTRALIZED)
        self.assertIsNone(CLOSING_KEYWORD.search(rewritten))

    def test_every_keyword_and_reference_form_is_neutralized(self) -> None:
        keywords = (
            "close", "closes", "closed",
            "fix", "fixes", "fixed",
            "resolve", "resolves", "resolved",
        )
        references = {
            "{} [#7](https://github.com/o/r/issues/7)": (
                "Refs [#7](https://github.com/o/r/issues/7)"
            ),
            "{} o/r#7": "Refs o/r#7",
            "{}: #7": "Refs: #7",
        }
        for keyword in keywords:
            for spelling in (keyword, keyword.upper(), keyword.capitalize()):
                for form, expected in references.items():
                    raw = form.format(spelling)
                    with self.subTest(raw=raw):
                        self.assertIsNotNone(CLOSING_KEYWORD.search(raw))
                        self.assertEqual(
                            self.rewriter.neutralize_closing_keywords(raw), expected
                        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
