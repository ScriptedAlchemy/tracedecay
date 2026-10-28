#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stderr
from dataclasses import replace
from pathlib import Path
from unittest import mock

from python.runfiles import runfiles


SCRIPT = Path(__file__).resolve().parent / "bazel" / "gen_builds.py"


def load_gen_builds():
    spec = importlib.util.spec_from_file_location("gen_builds", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ProductionFeatureResolutionTests(unittest.TestCase):
    ROOT = (
        "tracedecay v0.1.0 (/workspace/crates/tracedecay)|"
        "default,full,lite,production,token-counting"
    )
    ROOT_FEATURES = {"default", "full", "lite", "production", "token-counting"}

    def setUp(self) -> None:
        self.gen = load_gen_builds()

    def resolve(self, output, package="tracedecay", edges="normal,build", features=()):
        query = mock.Mock(spec=self.gen.CargoQuery)
        query.run.return_value = output
        return self.gen.feature_map(query, package, edges, features)

    def test_clean_graph_projects_members_and_unions_repeated_rows(self) -> None:
        output = "\n".join([
            self.ROOT,
            "tracedecay-code-index v0.1.0 (/workspace/crates/tracedecay-code-index)|lite",
            "serde v1.0.0|default,derive",
            "syn v2.0.0 (registry `alternate`)|full",
            "git-dep v1.2.3-beta.1+build (https://example.invalid/crates/git-dep?rev=main#01234567)|std",
            "path-dep v0.2.0 (/vendor/path-dep)|",
            "derive-dep v1.0.0 (proc-macro)|",
            "git-derive v1.0.0 (proc-macro) (file:///vendor/crates/git-derive#01234567)|derive",
            "path-derive v1.0.0 (proc-macro) (/vendor/path-derive)|derive",
            "registry-derive v1.0.0 (proc-macro) (registry `https://example.invalid/crates/index`)|derive",
            "tracedecay-macros v0.1.0 (proc-macro) (/workspace/crates/tracedecay-macros)|expand",
            "tracedecay-code-index v0.1.0 (/workspace/crates/tracedecay-code-index)|full (*)",
            "tracedecay-empty v0.1.0 (/workspace/crates/tracedecay-empty)| (*)",
            "",
        ])
        self.assertEqual(self.resolve(output), {
            "tracedecay": self.ROOT_FEATURES,
            "tracedecay-code-index": {"lite", "full"},
            "tracedecay-macros": {"expand"},
            "tracedecay-empty": set(),
        })

    def test_production_rejects_transport_including_external_repeated_rows(self) -> None:
        packages = [
            "tracedecay v0.1.0 (/workspace/crates/tracedecay)",
            "member v0.1.0 (/workspace/crates/member)",
            "registry-dep v1.0.0",
            "alt-dep v1.0.0 (registry `alternate`)",
            "git-dep v1.0.0 (https://example.invalid/dependency?branch=main#01234567)",
            "path-dep v1.0.0 (/vendor/path-dep)",
            "derive-dep v1.0.0 (proc-macro)",
            "alt-derive v1.0.0 (proc-macro) (registry `alternate`)",
            "git-derive v1.0.0 (proc-macro) (https://example.invalid/derive#01234567)",
            "path-derive v1.0.0 (proc-macro) (/vendor/path-derive)",
        ]
        for package in packages:
            with self.subTest(package=package):
                output = f"{self.ROOT}\n{package}|std\n{package}|std,test-transport (*)\n"
                with self.assertRaises(ValueError) as raised:
                    self.resolve(output)
                self.assertIn("test-transport", str(raised.exception))
                self.assertIn(package, str(raised.exception))

    def test_dev_and_explicit_test_contexts_keep_transport_features(self) -> None:
        output = "\n".join([
            self.ROOT + ",test-transport",
            "external v1.0.0|test-transport (*)",
            "",
            "member v0.1.0 (/workspace/crates/member)|test-transport",
            "",
        ])
        contexts = [
            (None, "normal,build,dev", ("tracedecay/test-transport",)),
            ("tracedecay", "normal,build,dev", ()),
            ("tracedecay", "normal,build", ("test-transport",)),
            ("member", "normal,build", ()),
        ]
        for context in contexts:
            with self.subTest(context=context):
                self.assertEqual(self.resolve(output, *context), {
                    "tracedecay": self.ROOT_FEATURES | {"test-transport"},
                    "member": {"test-transport"},
                })
        with self.assertRaises(ValueError):
            self.resolve(output)

    def test_nonrepeated_external_transport_is_rejected(self) -> None:
        with self.assertRaises(ValueError):
            self.resolve(f"{self.ROOT}\nexternal v1.0.0|test-transport\n")

    def test_lean_context_preserves_empty_features_and_locked_request(self) -> None:
        output = "tracedecay v0.1.0 (/workspace/crates/tracedecay)|\n"
        query = mock.Mock(spec=self.gen.CargoQuery)
        query.run.return_value = output
        self.assertEqual(self.gen.feature_map(
            query, "tracedecay", "normal,build", (self.gen.NO_DEFAULT_FEATURES,),
        ), {"tracedecay": set()})
        command = query.run.call_args.args[0]
        self.assertIn("--locked", command)
        self.assertIn("--no-default-features", command)
        self.assertNotIn("--features", command)

    def test_missing_production_root_is_rejected(self) -> None:
        for output in (
            "", "\n\n", "serde v1.0.0|default\n",
            "member v0.1.0 (/workspace/crates/member)|\n",
            "tracedecay v0.1.0|production\n",
            "tracedecay v0.1.0 (https://example.invalid/crates/tracedecay#01234567)|production\n",
        ):
            with self.subTest(output=output), self.assertRaises(ValueError):
                self.resolve(output)

    def test_malformed_external_rows_after_valid_root_are_rejected(self) -> None:
        for row in (
            "not a package row",
            "external v1.0.0",
            "external vgarbage|std",
            "external v1.0.0 (proc-macro) (/vendor/external|std",
            "external v1.0.0 (unknown source)|std",
            "external v1.0.0|std|test-transport",
            "external v1.0.0|std,,test-transport",
            "external v1.0.0|std,",
            "external v1.0.0|std (*) ,test-transport",
            "external v1.0.0|test-(*)transport",
            "external v1.0.0 (*)|std",
            "external v1.0.0|std (*) (*)",
        ):
            with self.subTest(row=row), self.assertRaises(ValueError):
                self.resolve(f"{self.ROOT}\n{row}\n")

    def test_native_paths_and_parentheses_remain_member_paths(self) -> None:
        for path in (
            "/workspace (local)/crates/tracedecay",
            "/workspace|local/crates/tracedecay",
            r"C:\workspace\crates\tracedecay",
            "C:/workspace/crates/tracedecay",
            r"\\server\workspace\crates\tracedecay",
        ):
            with self.subTest(path=path):
                self.assertEqual(self.resolve(
                    f"tracedecay v0.1.0 ({path})|production\n",
                ), {"tracedecay": {"production"}})


class ProductionDeclarationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gen = load_gen_builds()
        self.features = {
            "default": ["production"],
            "production": ["token-counting", "lite", "full"],
            "token-counting": [],
            "lite": [],
            "full": [],
        }

    def test_invalid_declarations_fail_before_resolution_in_both_cli_modes(self) -> None:
        invalid_tables = [
            {},
            self.features | {"default": []},
            self.features | {"default": ["production", "test-transport"]},
            self.features | {"production": []},
            self.features | {"production": ["token-counting", "lite", "full", "test-transport"]},
        ]
        invalid_tables.extend(
            self.features | {"production": [f for f in self.features["production"] if f != missing]}
            for missing in ("token-counting", "lite", "full")
        )
        packages = [[], [{"name": "tracedecay"}]] + [
            [{"name": "tracedecay", "features": table}] for table in invalid_tables
        ]
        for arguments in ([], ["--check"]):
            for declaration in packages:
                with self.subTest(arguments=arguments, packages=declaration):
                    query = mock.Mock(spec=self.gen.CargoQuery, workspace=Path("/unused"))
                    query.run.return_value = json.dumps({
                        "packages": declaration,
                    })
                    with mock.patch.object(self.gen.CargoQuery, "from_bazel", return_value=query), \
                            mock.patch.object(sys, "argv", [str(SCRIPT), *arguments]):
                        with self.assertRaises(ValueError):
                            self.gen.main()
                    self.assertEqual(query.run.call_count, 1)
                    command = query.run.call_args.args[0]
                    self.assertEqual(command[0], "metadata")
                    self.assertIn("--locked", command)

    def test_valid_declaration_reaches_graph_policy_before_any_output_write(self) -> None:
        for arguments in ([], ["--check"]):
            with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch).resolve()
                product = root / "crates/tracedecay"
                product.mkdir(parents=True)
                build = product / "BUILD.bazel"
                build.write_text("existing output\n", encoding="utf-8")
                (root / "Cargo.toml").write_text("[profile.dev]\n", encoding="utf-8")
                metadata = json.dumps({"packages": [{
                    "name": "tracedecay",
                    "features": self.features,
                    "manifest_path": str(product / "Cargo.toml"),
                    "dependencies": [],
                    "targets": [],
                }]})

                def cargo_output(command, **kwargs):
                    self.assertIn("--locked", command)
                    if command[0] == "metadata":
                        return metadata
                    self.assertEqual(command[0], "tree")
                    return (
                        f"tracedecay v0.1.0 ({product})|production\n"
                        "external v1.0.0 (proc-macro)|test-transport (*)\n"
                    )

                query = mock.Mock(spec=self.gen.CargoQuery, workspace=root)
                query.run.side_effect = cargo_output
                with mock.patch.object(self.gen.CargoQuery, "from_bazel", return_value=query), \
                        mock.patch.object(sys, "argv", [str(SCRIPT), *arguments]):
                    with self.assertRaises(ValueError) as raised:
                        self.gen.main()
                self.assertIn("test-transport", str(raised.exception))
                self.assertEqual(build.read_text(encoding="utf-8"), "existing output\n")
                self.assertEqual(set(root.rglob("BUILD.bazel")), {build})


class CargoAcquisitionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gen = load_gen_builds()
        scratch = tempfile.TemporaryDirectory(prefix="cargo query ", dir=os.environ["TEST_TMPDIR"])
        self.addCleanup(scratch.cleanup)
        root = Path(scratch.name).resolve()
        self.workspace = root / "workspace with spaces"
        self.workspace.mkdir()
        (self.workspace / "Cargo.toml").write_text("[workspace]\n", encoding="utf-8")
        files = runfiles.Create()
        self.assertIsNotNone(files)
        toolchain = files.Rlocation(os.environ["GEN_BUILDS_TOOLCHAIN"], source_repo="")
        self.assertIsNotNone(toolchain)
        (self.workspace / "rust-toolchain.toml").write_bytes(Path(toolchain).read_bytes())
        env = {
            key: value for key, value in os.environ.items()
            if not key.startswith(("CARGO", "RUSTC", "RUSTUP", "__CARGO_TEST"))
        }
        for variable, directory in (
            ("CARGO_HOME", "cargo home"),
            ("RUSTUP_HOME", "rustup home"),
            ("HOME", "home"),
        ):
            path = root / directory
            path.mkdir()
            env[variable] = str(path)
        env.update(
            BUILD_WORKSPACE_DIRECTORY=str(self.workspace),
            USERPROFILE=env["HOME"],
            PATH="",
        )
        environment = mock.patch.dict(os.environ, env, clear=True)
        environment.start()
        self.addCleanup(environment.stop)

    def test_declared_tools_load_acquisition_policy_with_origin(self) -> None:
        query = self.gen.CargoQuery.from_bazel()
        for key, value in (
            ("unstable.git", '"shallow-deps"'),
            ("net.git-fetch-with-cli", "true"),
        ):
            with self.subTest(key=key):
                output = query.run(["-Zunstable-options", "config", "get", key, "--show-origin"])
                setting, origin = output.strip().split(" # ", 1)
                self.assertEqual(setting, f"{key} = {value}")
                self.assertIn(str(query.policy), origin)
        self.assertNotIn("RUSTC_BOOTSTRAP", os.environ)
        self.assertNotIn("RUSTC", os.environ)

    def test_cargo_rejects_array_policy_and_unactivated_inspection(self) -> None:
        query = self.gen.CargoQuery.from_bazel()
        args = ["-Zunstable-options", "config", "get", "unstable.git", "--show-origin"]
        query.run(args)
        malformed = self.workspace / "invalid-acquisition.toml"
        malformed.write_text('[unstable]\ngit = ["shallow-deps"]\n', encoding="utf-8")
        with self.assertRaises(SystemExit) as raised:
            replace(query, policy=malformed).run(args)
        self.assertNotEqual(raised.exception.code, 0)
        unactivated = subprocess.run(
            [str(query.cargo), "--config", str(query.policy), *args],
            cwd=query.workspace,
            env=os.environ.copy(),
            capture_output=True,
            text=True,
            encoding="utf-8",
        )
        self.assertNotEqual(unactivated.returncode, 0)
        self.assertIn("nightly", unactivated.stderr.lower())

    def test_missing_declared_tool_and_version_mismatch_fail(self) -> None:
        with mock.patch.dict(os.environ, {"GEN_BUILDS_CARGO": ""}):
            with self.assertRaisesRegex(ValueError, "GEN_BUILDS_CARGO"):
                self.gen.CargoQuery.from_bazel()
        (self.workspace / "rust-toolchain.toml").write_text(
            '[toolchain]\nchannel = "0.0.0"\n', encoding="utf-8",
        )
        with self.assertRaisesRegex(ValueError, "must match rust-toolchain.toml"):
            self.gen.CargoQuery.from_bazel()

    def test_cli_requires_workspace_and_rejects_unknown_arguments(self) -> None:
        with mock.patch.dict(os.environ, {"BUILD_WORKSPACE_DIRECTORY": ""}):
            for args in ([], ["--check"]):
                with self.subTest(args=args), mock.patch.object(sys, "argv", [str(SCRIPT), *args]):
                    with self.assertRaisesRegex(ValueError, "bazel run //scripts:gen_builds"):
                        self.gen.main()
            with mock.patch.object(sys, "argv", [str(SCRIPT), "--unknown"]):
                with self.assertRaises(SystemExit) as raised:
                    self.gen.main()
                self.assertIn("usage:", raised.exception.code)


class ResolverProcessTests(unittest.TestCase):
    def test_stdout_remains_protocol_data(self) -> None:
        gen = load_gen_builds()
        diagnostics = io.StringIO()
        with tempfile.TemporaryDirectory() as scratch, redirect_stderr(diagnostics):
            output = gen.run([
                sys.executable, "-c", "print('package v1.0.0|std')",
            ], cwd=scratch)
        self.assertEqual(output, "package v1.0.0|std\n")
        self.assertTrue(diagnostics.getvalue())

    def test_nonzero_exit_remains_failure(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch, redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as raised:
                gen.run([sys.executable, "-c", "raise SystemExit(23)"], cwd=scratch)
        self.assertEqual(raised.exception.code, 23)


class DefaultAliasNormalizationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.gen = load_gen_builds()

    def test_adds_default_when_implied_features_are_already_on(self) -> None:
        table = {
            "default": ["production"],
            "production": ["lite", "full", "token-counting"],
            "lite": [],
            "full": [],
            "token-counting": [],
        }
        feats = {"production", "lite", "full", "token-counting"}
        self.assertEqual(
            self.gen.normalize_default_alias(feats, table),
            {"default", "production", "lite", "full", "token-counting"},
        )

    def test_leaves_lean_sets_without_default(self) -> None:
        table = {
            "default": ["production"],
            "production": ["lite"],
            "lite": [],
        }
        self.assertEqual(self.gen.normalize_default_alias(set(), table), set())
        self.assertEqual(self.gen.normalize_default_alias({"lite"}, table), {"lite"})

    def test_empty_default_stays_off(self) -> None:
        table = {"default": []}
        self.assertEqual(
            self.gen.normalize_default_alias({"lite"}, table),
            {"lite"},
        )

    def test_preserves_default_dependency_edges(self) -> None:
        table = {
            "default": ["production", "dep:optional-dep", "other/feat"],
            "production": ["lite"],
            "lite": [],
        }
        self.assertEqual(
            self.gen.normalize_default_alias({"production", "lite"}, table),
            {"production", "lite"},
        )

    def test_collapses_local_alias_with_resolved_dependency_edges(self) -> None:
        table = {
            "default": ["production"],
            "production": ["dep:optional-dep", "other/feat"],
        }
        self.assertEqual(
            self.gen.normalize_default_alias({"production"}, table),
            {"default", "production"},
        )


class TestResourceOwnershipTests(unittest.TestCase):
    def test_suite_owns_its_inputs_and_explicit_shared_modules(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch).resolve()
            for name in ("owned/main.rs", "owned/input.json", "other/main.rs",
                         "common/mod.rs", "fixtures/sample.rs"):
                path = root / "tests" / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("fixture")
            package = {"manifest_path": str(root / "Cargo.toml")}
            target = {"src_path": str(root / "tests/owned/main.rs")}
            patterns = gen.test_resource_globs(package, target, (root / "tests/common",))
            # Match Bazel's recursive file globs, including files below each directory.
            inputs = {path.relative_to(root).as_posix() for pattern in patterns
                      for path in root.glob(pattern + "/*") if path.is_file()}
            self.assertIn("tests/owned/input.json", inputs)
            self.assertIn("tests/common/mod.rs", inputs)
            self.assertIn("tests/fixtures/sample.rs", inputs)
            self.assertNotIn("tests/other/main.rs", inputs)
            sources = {path for path, _ in gen.target_sources(target)}
            self.assertEqual(sources, {root / "tests/owned/main.rs"})


class SharedPackageOwnershipTests(unittest.TestCase):
    def test_nested_package_owns_embedded_fixture(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            package = root / "evals/agent_adoption"
            package.mkdir(parents=True)
            (root / "evals/BUILD.bazel").write_text("# generated parent")
            for build_name in ("BUILD.bazel", "BUILD"):
                build = package / build_name
                build.write_text("# handwritten package")
                self.assertEqual(
                    gen.owner_pkg(Path("evals/agent_adoption/fixture/Cargo.lock"), root),
                    ("evals/agent_adoption", "fixture"),
                )
                self.assertEqual(
                    gen.owner_pkg(Path("evals/agent_adoption/input.json"), root),
                    ("evals/agent_adoption", ""),
                )
                build.unlink()

    def test_unowned_fixtures_keep_shared_package_layout(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            self.assertEqual(
                gen.owner_pkg(Path("tests/fixtures/example.json"), root),
                ("tests", "fixtures"),
            )
            self.assertEqual(
                gen.owner_pkg(Path("crates/example/tests/input.json"), root),
                ("crates/example", "tests"),
            )


if __name__ == "__main__":
    unittest.main()
