#!/usr/bin/env python3
"""Every file under scripts/bazel/patches is referenced by MODULE.bazel.

An unreferenced patch is dead weight that looks like a live fix.
"""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).absolute().parent.parent
PATCHES = ROOT / "scripts" / "bazel" / "patches"
LABEL = re.compile(r'"//scripts:bazel/patches/([^"]+\.patch)"')


class DependencyPatchesTest(unittest.TestCase):
    def test_no_patch_file_is_unreferenced(self):
        referenced = set(LABEL.findall((ROOT / "MODULE.bazel").read_text()))
        on_disk = {p.name for p in PATCHES.glob("*.patch")}
        self.assertTrue(on_disk)
        self.assertEqual(on_disk - referenced, set())

    def test_every_referenced_patch_exists(self):
        referenced = set(LABEL.findall((ROOT / "MODULE.bazel").read_text()))
        self.assertEqual(referenced - {p.name for p in PATCHES.glob("*.patch")}, set())


if __name__ == "__main__":
    unittest.main()
