#!/usr/bin/env python3
"""Mutation checks against the actual final handoff, without editing its assets."""
import importlib.util
from pathlib import Path
import shutil
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('pack_check', Path(__file__).with_name('check-concept-pack.py'))
pack_check = importlib.util.module_from_spec(spec)
spec.loader.exec_module(pack_check)


class PackTests(unittest.TestCase):
    def test_final_pack_and_rejected_mutations(self):
        _, errors = pack_check.validate(pack_check.CANONICAL)
        self.assertEqual(errors, [])
        with tempfile.TemporaryDirectory() as temp:
            pack = Path(temp) / 'pack'
            shutil.copytree(pack_check.CANONICAL, pack)
            brief = next(pack.glob('*/final/[0-9]*.md'))
            original = brief.read_text()
            brief.unlink()
            # Links elsewhere in the real handoff that named the brief also break;
            # only the plate/brief pairing error is this mutation's own contract.
            self.assertEqual(
                pack_check.validate(pack)[1][0],
                f'{brief.with_suffix(".png")}: missing same-stem brief',
            )
            brief.write_text(original + '\n[Broken](missing-authority.md)\n')
            self.assertEqual(
                pack_check.validate(pack)[1],
                [f'{brief}: broken or escaping local link missing-authority.md'],
            )
            brief.write_text(original)
            manifest = brief.parent / 'README.md'
            manifest.write_text(manifest.read_text() + f'\n[Duplicate]({brief.name})\n')
            self.assertEqual(
                pack_check.validate(pack)[1],
                [f'{manifest}: {brief.name} has 2 mappings; expected one'],
            )


if __name__ == '__main__':
    unittest.main()
