#!/usr/bin/env python3
"""Behavioral tests for the SQLite plan-stability source check."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_PATH = Path(__file__).resolve().with_name("check-sqlite-plan-stability.py")

SCHEMA = '''
pub const SCHEMA: &str = "CREATE TABLE jobs (id INTEGER, state TEXT, owner TEXT, meta TEXT);
    CREATE TABLE runs (id INTEGER, state TEXT);
    CREATE INDEX IF NOT EXISTS idx_jobs_pending ON jobs(owner) WHERE state = 'pending';
    CREATE INDEX idx_jobs_owned ON jobs(id) WHERE owner IS NOT NULL;
    CREATE INDEX idx_jobs_flagged ON jobs(id)
        WHERE json_extract(meta, '$.flag') = 1;";
'''


def load_script():
    spec = importlib.util.spec_from_file_location("check_sqlite_plan_stability", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class PlanStabilityTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.check = load_script()

    def findings(self, source: str, *, path: str = "crates/demo/src/store.rs") -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, text in (("crates/demo/src/schema.rs", SCHEMA), (path, source)):
                (root / relative).parent.mkdir(parents=True, exist_ok=True)
                (root / relative).write_text(text, encoding="utf-8")
            indexes, findings = self.check.check(root)
        self.assertEqual(
            [(index.name, sorted(index.terms)) for index in indexes],
            [
                ("idx_jobs_flagged", ["json_extract(meta,'$.flag')"]),
                ("idx_jobs_pending", ["state"]),
            ],
        )
        return [finding.message for finding in findings]

    def test_bound_partial_index_predicate_is_reported(self) -> None:
        for sql in (
            "SELECT id FROM jobs WHERE owner = ?1 AND state = ?2",
            "SELECT id FROM jobs WHERE ?1 = state",
            "SELECT id FROM jobs WHERE state IN (?1, ?2)",
            "SELECT COUNT(*) FROM main.jobs AS j WHERE j.state = :state",
            "DELETE FROM jobs WHERE state IS ?",
            "SELECT id FROM jobs WHERE json_extract(meta, '$.flag') = ?1",
        ):
            with self.subTest(sql=sql):
                found = self.findings(f'fn read() {{ query("{sql}"); }}\n')
                self.assertTrue(found)
                for message in found:
                    self.assertIn("partial index idx_jobs_", message)

    def test_literal_predicates_and_other_tables_pass(self) -> None:
        for sql in (
            "SELECT id FROM jobs WHERE owner = ?1 AND state = 'pending'",
            "UPDATE jobs SET state = ?1, owner = ?2 WHERE state = 'pending'",
            "SELECT id FROM runs WHERE state = ?1",
            "UPDATE runs SET state = ?1 WHERE state = ?2 AND id IN "
            "(SELECT id FROM jobs WHERE state = 'pending')",
            "SELECT id FROM jobs WHERE owner = ?1",
            "SELECT id FROM jobs WHERE owner LIKE '%' || ?1 || '%'",
            "SELECT id FROM jobs WHERE owner NOT LIKE ?1",
            "SELECT id FROM jobs WHERE owner LIKE ?1 || '%'",
        ):
            with self.subTest(sql=sql):
                self.assertEqual(self.findings(f'fn read() {{ query("{sql}"); }}\n'), [])

    def test_bound_like_or_glob_pattern_is_reported(self) -> None:
        for sql in (
            "SELECT id FROM runs WHERE state LIKE ?1",
            "SELECT id FROM runs WHERE state GLOB ? ORDER BY id",
        ):
            with self.subTest(sql=sql):
                found = self.findings(f'fn read() {{ query("{sql}"); }}\n')
                self.assertEqual(len(found), 1, found)
                self.assertIn("pattern", found[0])

    def test_format_and_raw_literals_are_scanned(self) -> None:
        source = (
            'fn read() {\n'
            '    query(&format!("SELECT {COLUMNS} FROM jobs WHERE state = ?1"));\n'
            '    query(r#"SELECT id FROM jobs\n         WHERE state = \'pending\'"#);\n'
            '    query("SELECT id \\\n          FROM jobs WHERE state = ?2");\n'
            '}\n'
        )
        found = self.findings(source)
        self.assertEqual(len(found), 2, found)

    def test_exemption_needs_a_reason_near_the_statement(self) -> None:
        bound = 'query("SELECT id FROM jobs WHERE state = ?1");'
        exempt = f"fn read() {{\n    // qpsg-exempt: the table holds one row\n    {bound}\n}}\n"
        self.assertEqual(self.findings(exempt), [])
        no_reason = f"fn read() {{\n    // qpsg-exempt:\n    {bound}\n}}\n"
        self.assertEqual(len(self.findings(no_reason)), 1)
        far = f"fn read() {{\n    // qpsg-exempt: too far away\n\n\n\n    {bound}\n}}\n"
        self.assertEqual(len(self.findings(far)), 1)

    def test_test_sources_and_comments_are_ignored(self) -> None:
        bound = 'query("SELECT id FROM jobs WHERE state = ?1");'
        self.assertEqual(self.findings(f"fn t() {{ {bound} }}\n", path="crates/demo/src/store_tests.rs"), [])
        self.assertEqual(self.findings(f"fn t() {{ {bound} }}\n", path="crates/demo/tests/plan.rs"), [])
        inline = f"fn read() {{}}\n#[cfg(test)]\nmod tests {{\n    fn t() {{ {bound} }}\n}}\n"
        self.assertEqual(self.findings(inline), [])
        commented = f"// {bound}\n/* {bound} */\nfn read() {{}}\n"
        self.assertEqual(self.findings(commented), [])


if __name__ == "__main__":
    unittest.main()
