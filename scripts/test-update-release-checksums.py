#!/usr/bin/env python3
"""Behavioral checks for immutable checksum recovery."""

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("release_checksums", Path(__file__).with_name("update-release-checksums.py"))
assert SPEC is not None and SPEC.loader is not None
CHECKSUMS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKSUMS)


class ChecksumRecoveryTests(unittest.TestCase):
    def test_unchanged_release_is_a_noop(self):
        manifest = b"abc  archive.tar.gz\ndef  bundle.mcpb\n"
        self.assertFalse(CHECKSUMS.requires_update(manifest, manifest))

    def test_recovery_can_add_assets_and_reorder_lines(self):
        self.assertTrue(CHECKSUMS.requires_update(
            b"abc  archive.tar.gz\n\n", b"def  windows.zip\nabc  archive.tar.gz\n"))

    def test_existing_asset_hash_or_name_cannot_change(self):
        published = b"abc  archive.tar.gz\ndef  bundle.mcpb\n"
        for current in (b"changed  archive.tar.gz\ndef  bundle.mcpb\n",
                        b"abc  renamed.tar.gz\ndef  bundle.mcpb\n",
                        b"abc  archive.tar.gz\n"):
            with self.subTest(current=current), self.assertRaises(ValueError):
                CHECKSUMS.requires_update(published, current)

    def test_final_line_without_newline_is_still_immutable(self):
        with self.assertRaises(ValueError):
            CHECKSUMS.requires_update(b"abc  archive.tar.gz", b"def  windows.zip\n")


if __name__ == "__main__":
    unittest.main()
