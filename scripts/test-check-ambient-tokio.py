#!/usr/bin/env python3
"""Behavioral tests for the ambient Tokio source check."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT_PATH = Path(__file__).resolve().with_name("check-ambient-tokio.py")


def load_script():
    spec = importlib.util.spec_from_file_location("check_ambient_tokio", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class AmbientTokioTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.check = load_script()

    def findings(self, files: dict[str, str]) -> list[str]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for relative, text in {"crates/demo/src/lib.rs": "pub fn ok() {}\n", **files}.items():
                (root / relative).parent.mkdir(parents=True, exist_ok=True)
                (root / relative).write_text(text, encoding="utf-8")
            scanned, findings = self.check.check(root)
        self.assertGreater(scanned, 0)
        return [f"{finding.path}:{finding.line}:{finding.target}" for finding in findings]

    def one(self, source: str) -> list[str]:
        return self.findings({"crates/demo/src/work.rs": source})

    def test_ambient_calls_are_reported_through_imports(self) -> None:
        cases = {
            "fn f() { tokio::spawn(async {}); }": "tokio::spawn",
            "fn f() { ::tokio::task::spawn_blocking(|| ()); }": "tokio::task::spawn_blocking",
            "use tokio::runtime::Handle;\nfn f() { Handle::current(); }": "tokio::runtime::Handle::current",
            "use tokio::{task::{self, JoinSet}, time};\nfn f() { task::spawn(x); time::sleep(d); }": "tokio::task::spawn",
            "use tokio::time::timeout as bounded;\nfn f() { bounded(d, x); }": "tokio::time::timeout",
            "use tokio::time::*;\nfn f() { interval_at(a, b); }": "tokio::time::interval_at",
            "use tokio::spawn;\nfn f() { spawn(async {}); }": "tokio::spawn",
        }
        for source, target in cases.items():
            with self.subTest(source=source):
                found = self.one(source + "\n")
                self.assertTrue(found, source)
                self.assertTrue(found[0].endswith(target), found)

    def test_join_set_spawn_is_reported_only_on_a_join_set(self) -> None:
        source = (
            "use tokio::task::JoinSet;\n"
            "struct Owner { tasks: tokio::task::JoinSet<()>, registry: Registry }\n"
            "fn f(owner: &mut Owner) {\n"
            "    let mut set = JoinSet::new();\n"
            "    set.spawn(async {});\n"
            "    owner.tasks.spawn_blocking(|| ());\n"
            "    owner.tasks.spawn_on(async {}, &handle);\n"
            "    owner.registry.spawn(work);\n"
            "}\n"
        )
        self.assertEqual(
            self.one(source),
            [
                "crates/demo/src/work.rs:5:tokio::task::JoinSet::spawn",
                "crates/demo/src/work.rs:6:tokio::task::JoinSet::spawn_blocking",
            ],
        )

    def test_explicit_handles_and_other_crates_pass(self) -> None:
        for source in (
            "fn f(h: tokio::runtime::Handle) { h.spawn(async {}); h.spawn_blocking(|| ()); }",
            "fn f() { native_runtime::spawn_blocking(|| ()); native_runtime::sleep_until(t); }",
            "use asupersync::time::sleep;\nfn f() { sleep(now, d); }",
            "fn f() { let _ = tokio::runtime::Handle::try_current(); tokio::time::Instant::now(); }",
            "fn f() { std::thread::spawn(|| ()); }",
        ):
            with self.subTest(source=source):
                self.assertEqual(self.one(source + "\n"), [])

    def test_exemption_needs_a_reason_near_the_call(self) -> None:
        call = "    tokio::spawn(async {});"
        self.assertEqual(self.one(f"fn f() {{\n    // ambient-tokio: runs on a Tokio worker\n{call}\n}}\n"), [])
        self.assertEqual(len(self.one(f"fn f() {{\n    // ambient-tokio:\n{call}\n}}\n")), 1)
        self.assertEqual(len(self.one(f"fn f() {{\n    // ambient-tokio: far away\n\n\n\n{call}\n}}\n")), 1)

    def test_a_module_declaration_exemption_covers_its_file(self) -> None:
        files = {
            "crates/demo/src/lib.rs": "// ambient-tokio: its timers run on Tokio tasks\nmod lifecycle;\nmod other;\n",
            "crates/demo/src/lifecycle.rs": "fn f() { tokio::time::sleep(d); }\n",
            "crates/demo/src/other.rs": "fn f() { tokio::time::sleep(d); }\n",
        }
        self.assertEqual(self.findings(files), ["crates/demo/src/other.rs:1:tokio::time::sleep"])

    def test_test_code_comments_and_strings_are_ignored(self) -> None:
        call = "fn t() { tokio::spawn(async {}); }\n"
        self.assertEqual(self.findings({"crates/demo/src/store_tests.rs": call}), [])
        self.assertEqual(self.findings({"crates/demo/src/run_test.rs": call}), [])
        self.assertEqual(self.findings({"crates/demo/tests/run.rs": call}), [])
        self.assertEqual(self.findings({"crates/demo/src/http_tests/remote.rs": call}), [])
        inline = f"fn f() {{}}\n#[cfg(test)]\n#[allow(dead_code)]\nmod tests {{\n    {call}}}\nfn g() {{}}\n"
        self.assertEqual(self.one(inline), [])
        for cfg in ("all(test, unix)", 'any(test, feature = "test-helpers")'):
            with self.subTest(cfg=cfg):
                self.assertEqual(self.one(f"#[cfg({cfg})]\n{call}"), [])
        self.assertEqual(len(self.one(f'#[cfg(any(test, feature = "serve"))]\n{call}')), 1)
        external = {
            "crates/demo/src/lib.rs": "#[cfg(test)]\nmod harness;\n",
            "crates/demo/src/harness.rs": call,
            "crates/demo/src/harness/journey.rs": call,
        }
        self.assertEqual(self.findings(external), [])
        commented = '// tokio::spawn(x);\n/* tokio::spawn(x); */\nfn f() { log("tokio::spawn(x)"); }\n'
        self.assertEqual(self.one(commented), [])
        after_test_module = f"#[cfg(test)]\nmod tests {{ fn t() {{}} }}\n{call}"
        self.assertEqual(self.one(after_test_module), ["crates/demo/src/work.rs:3:tokio::spawn"])


if __name__ == "__main__":
    unittest.main()
