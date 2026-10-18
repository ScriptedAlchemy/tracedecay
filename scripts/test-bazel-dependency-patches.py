#!/usr/bin/env python3
"""The Bazel dependency patches and the extracted-crate Cargo build stay in step.

check-distribution-acceptance.sh applies every scripts/bazel/patches/asupersync-*.patch
to the asupersync source it compiles. MODULE.bazel lists the patches Bazel applies.
A patch present in only one of them means the two builds run different runtimes.
"""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).absolute().parent.parent
PATCHES = ROOT / "scripts" / "bazel" / "patches"
LABEL = re.compile(r'"//scripts:bazel/patches/([^"]+\.patch)"')


def annotation_patches(module: str, crate: str) -> list[str]:
    for block in re.findall(r"crate\.annotation\((.*?)\n\)", module, re.DOTALL):
        if f'crate = "{crate}"' in block:
            return LABEL.findall(block)
    raise AssertionError(f"MODULE.bazel has no crate.annotation for {crate}")


class DependencyPatchParityTest(unittest.TestCase):
    def setUp(self):
        self.module = (ROOT / "MODULE.bazel").read_text()

    def test_asupersync_patches_match_the_distribution_build(self):
        applied_by_distribution = sorted(p.name for p in PATCHES.glob("asupersync-*.patch"))
        self.assertTrue(applied_by_distribution)
        # crate_universe applies an annotation's patches in sorted label order.
        self.assertEqual(
            annotation_patches(self.module, "asupersync"), applied_by_distribution
        )

    def test_distribution_build_applies_that_glob(self):
        script = (ROOT / "scripts" / "check-distribution-acceptance.sh").read_text()
        self.assertIn("scripts/bazel/patches/asupersync-*.patch", script)

    def test_no_patch_file_is_unreferenced(self):
        referenced = set(LABEL.findall(self.module))
        unreferenced = {p.name for p in PATCHES.glob("*.patch")} - referenced
        self.assertEqual(unreferenced, set())


if __name__ == "__main__":
    unittest.main()
