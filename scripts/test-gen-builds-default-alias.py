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
from contextlib import redirect_stderr, redirect_stdout
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
        "default,lite,medium,production,token-counting"
    )
    ROOT_FEATURES = {"default", "lite", "medium", "production", "token-counting"}

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
            "tracedecay-code-index v0.1.0 (/workspace/crates/tracedecay-code-index)|medium (*)",
            "tracedecay-empty v0.1.0 (/workspace/crates/tracedecay-empty)| (*)",
            "",
        ])
        self.assertEqual(self.resolve(output), {
            "tracedecay": self.ROOT_FEATURES,
            "tracedecay-code-index": {"lite", "medium"},
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

    def test_production_rejects_transitively_enabled_full_grammar_tier(self) -> None:
        for root in ("tracedecay", "tracedecay-cli"):
            for package in self.gen.GRAMMAR_PROFILE_PACKAGES:
                with self.subTest(root=root, package=package):
                    output = (
                        f"{self.ROOT}\n"
                        f"{package} v0.1.0 (/workspace/crates/{package})|lite\n"
                        f"{package} v0.1.0 (/workspace/crates/{package})|full (*)\n"
                    )
                    with self.assertRaisesRegex(ValueError, "opt-in full grammar tier"):
                        self.resolve(output, package=root)

    def test_explicit_full_and_dev_builds_keep_optional_grammars(self) -> None:
        output = (
            f"{self.ROOT}\n"
            "tracedecay-code-extraction v0.1.0 (/workspace/crates/tracedecay-code-extraction)|full,lite,medium\n"
        )
        for context in (
            ("tracedecay", "normal,build", ("full",)),
            ("tracedecay-cli", "normal,build", ("full",)),
            (None, "normal,build,dev", ("tracedecay-cli/full",)),
            ("tracedecay-query", "normal,build,dev", ("production",)),
        ):
            with self.subTest(context=context):
                self.assertEqual(
                    self.resolve(output, *context)["tracedecay-code-extraction"],
                    {"full", "lite", "medium"},
                )

    def test_production_benchmark_rejects_transitive_full_grammar_tier(self) -> None:
        root = "tracedecay-query v0.1.0 (/workspace/crates/tracedecay-query)|production"
        for package in self.gen.GRAMMAR_PROFILE_PACKAGES:
            with self.subTest(package=package):
                output = (
                    f"{root}\n"
                    f"{package} v0.1.0 (/workspace/crates/{package})|lite,medium\n"
                    f"{package} v0.1.0 (/workspace/crates/{package})|full (*)\n"
                )
                with self.assertRaisesRegex(ValueError, "opt-in full grammar tier"):
                    self.resolve(output, "tracedecay-query", features=("production",))

    def test_production_benchmark_requires_its_own_root(self) -> None:
        root = "tracedecay-query v0.1.0 (/workspace/crates/tracedecay-query)|production"
        grammars = (
            "tracedecay-code-index v0.1.0 "
            "(/workspace/crates/tracedecay-code-index)|lite,medium"
        )
        self.assertEqual(self.resolve(
            f"{root}\n{grammars}\n", "tracedecay-query", features=("production",),
        ), {
            "tracedecay-query": {"production"},
            "tracedecay-code-index": {"lite", "medium"},
        })
        with self.assertRaisesRegex(ValueError, "omitted the tracedecay-query root"):
            self.resolve(grammars, "tracedecay-query", features=("production",))

    def test_optional_full_artifacts_still_refuse_test_transport(self) -> None:
        output = f"{self.ROOT}\nexternal v1.0.0|test-transport\n"
        for root in ("tracedecay", "tracedecay-cli"):
            with self.subTest(root=root), self.assertRaisesRegex(ValueError, "test-transport"):
                self.resolve(output, package=root, features=("full",))

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


class TestFeatureSelectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gen = load_gen_builds()
        self.package = {
            "name": "tracedecay-code-index",
            "features": {"lite": [], "medium": [], "full": [], "test-helpers": []},
            "targets": [{
                "name": "code_index_suite", "kind": ["test"],
                "required-features": ["test-helpers"],
            }],
            "metadata": {"bazel": {"test_features": {"code_index_suite": {
                "default-features": False, "features": ["lite", "medium"],
            }}}},
        }

    def test_selected_suite_preserves_required_features_without_workspace_unification(self) -> None:
        selected = self.gen.test_feature_overrides(self.package)["code_index_suite"]
        self.assertEqual(selected, {"lite", "medium", "test-helpers", self.gen.NO_DEFAULT_FEATURES})
        key = (self.package["name"], "normal,build,dev", selected)
        shared = frozenset({"tracedecay-code-index/full"})
        self.assertEqual(self.gen.feature_resolution_context(key, shared), key)
        default = (self.package["name"], "normal,build,dev", frozenset())
        self.assertEqual(
            self.gen.feature_resolution_context(default, shared),
            (None, "normal,build,dev", shared),
        )
        query = mock.Mock(spec=self.gen.CargoQuery)
        query.run.return_value = "tracedecay-code-index v0.1.0 (/workspace/crates/tracedecay-code-index)|lite,medium,test-helpers\n"
        self.assertEqual(self.gen.feature_map(query, *key), {
            "tracedecay-code-index": {"lite", "medium", "test-helpers"},
        })
        command = query.run.call_args.args[0]
        self.assertIn("--no-default-features", command)
        self.assertNotIn("--workspace", command)
        self.assertEqual(command[command.index("--features") + 1], "lite,medium,test-helpers")

    def test_invalid_or_unknown_feature_profiles_fail_closed(self) -> None:
        invalid = [
            {"unknown_suite": {"default-features": False, "features": ["lite"]}},
            {"code_index_suite": {"default-features": True, "features": ["lite"]}},
            {"code_index_suite": {"default-features": False, "features": ["missing"]}},
            {"code_index_suite": {"default-features": False, "features": "lite"}},
            {"code_index_suite": {"features": ["lite"]}},
            {"code_index_suite": {"default-features": False, "features": [1]}},
            [],
        ]
        for configured in invalid:
            with self.subTest(configured=configured), self.assertRaises(ValueError):
                self.gen.test_feature_overrides(self.package | {
                    "metadata": {"bazel": {"test_features": configured}},
                })

    def test_same_named_binary_or_benchmark_keeps_its_own_required_features(self) -> None:
        overrides = self.gen.test_feature_overrides(self.package)
        for kind in ("bin", "bench", "example"):
            target = {"name": "code_index_suite", "kind": [kind], "required-features": ["full"]}
            with self.subTest(kind=kind):
                self.assertEqual(self.gen.target_requested_features(target, overrides), {"full"})
        self.assertEqual(
            self.gen.target_requested_features(self.package["targets"][0], overrides),
            overrides["code_index_suite"],
        )


class BinaryOutputNameTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gen = load_gen_builds()
        self.package = {
            "name": "tracedecay-cli",
            "targets": [
                {"name": "tracedecay", "kind": ["bin"]},
                {"name": "tracedecay-full", "kind": ["bin"]},
            ],
        }

    def test_feature_binary_preserves_the_canonical_executable_basename(self) -> None:
        configured = {"tracedecay-full": "full/tracedecay"}
        self.assertEqual(self.gen.binary_name_overrides(self.package | {
            "metadata": {"bazel": {"binary_names": configured}},
        }), configured)

    def test_unknown_escaping_reserved_or_overlapping_outputs_are_refused(self) -> None:
        invalid = [
            {"unknown": "full/tracedecay"},
            {"tracedecay-full": "../tracedecay"},
            {"tracedecay-full": "/tmp/tracedecay"},
            {"tracedecay-full": "test/tracedecay"},
            {"tracedecay-full": "tracedecay"},
            {"tracedecay-full": "tracedecay/child"},
            {"tracedecay-full": 1},
            [],
        ]
        for configured in invalid:
            with self.subTest(configured=configured), self.assertRaises(ValueError):
                self.gen.binary_name_overrides(self.package | {
                    "metadata": {"bazel": {"binary_names": configured}},
                })


class ManualBinarySelectionTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gen = load_gen_builds()

    def test_full_cli_requires_an_explicit_target_but_benchmarks_compile_by_default(self) -> None:
        full_cli = {"name": "tracedecay-full", "kind": ["bin"], "test": False, "required-features": ["full"]}
        index_bench = {"name": "tracedecay-index-bench", "kind": ["bin"], "test": False, "required-features": ["production"]}
        self.assertTrue(self.gen.requires_manual_binary("crates/tracedecay-cli", full_cli, set()))
        self.assertFalse(self.gen.requires_manual_binary("crates/tracedecay-query", index_bench, set()))

    def test_feature_opt_in_applies_only_to_test_participating_binaries(self) -> None:
        target = {"name": "example", "kind": ["bin"], "required-features": ["production"]}
        self.assertTrue(self.gen.requires_manual_binary("crates/example", target, set()))
        self.assertFalse(self.gen.requires_manual_binary("crates/example", target, {"production"}))
        self.assertFalse(self.gen.requires_manual_binary("crates/example", target | {"test": False}, set()))
        self.assertFalse(self.gen.requires_manual_binary("crates/example", target | {"kind": ["test"]}, set()))


class ProductionDeclarationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.gen = load_gen_builds()
        self.features = {
            "default": ["production"],
            "production": ["token-counting", "lite", "medium"],
            "token-counting": [],
            "lite": [],
            "medium": [],
            "full": [],
        }

    def test_invalid_declarations_fail_before_resolution_in_both_cli_modes(self) -> None:
        invalid_tables = [
            {},
            self.features | {"default": []},
            self.features | {"default": ["production", "test-transport"]},
            self.features | {"production": []},
            self.features | {"production": ["token-counting", "lite", "medium", "test-transport"]},
            # The full grammar tier is opt-in; production must not reach it.
            self.features | {"production": ["token-counting", "lite", "medium", "full"]},
        ]
        invalid_tables.extend(
            self.features | {"production": [f for f in self.features["production"] if f != missing]}
            for missing in ("token-counting", "lite", "medium")
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
            "production": ["lite", "medium", "token-counting"],
            "lite": [],
            "medium": [],
            "token-counting": [],
        }
        feats = {"production", "lite", "medium", "token-counting"}
        self.assertEqual(
            self.gen.normalize_default_alias(feats, table),
            {"default", "production", "lite", "medium", "token-counting"},
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
    def test_generation_leaves_linked_foreign_build_files_untouched(self) -> None:
        gen = load_gen_builds()
        with tempfile.TemporaryDirectory() as scratch:
            parent = Path(scratch).resolve()
            root = parent / "workspace"
            product = root / "crates/tracedecay"
            (product / "src").mkdir(parents=True)
            (product / "src/lib.rs").write_text("pub fn present() {}\n")
            (product / "Cargo.toml").write_text(
                '[package]\nname = "tracedecay"\nversion = "0.1.0"\n'
            )
            (root / "Cargo.toml").write_text("[profile.dev]\n")
            (root / "Cargo.lock").write_text("package = []\n")
            for name in (".bazelrc", "MODULE.bazel", "BUILD.bazel",
                         "scripts/BUILD.bazel", "sdks/codegen/BUILD.bazel"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("# handwritten\n")

            foreign = parent / "other-checkout"
            foreign.mkdir()
            foreign_build = foreign / "BUILD.bazel"
            original = (
                "# handwritten\n" + gen.GEN_BEGIN
                + '\nfilegroup(name = "owned_elsewhere", srcs = [])\n'
                + gen.GEN_END + "\n"
            )
            foreign_build.write_text(original)
            foreign_crate = parent / "other-crate"
            foreign_crate.mkdir()
            foreign_crate_build = foreign_crate / "BUILD.bazel"
            foreign_crate_build.write_text(gen.BANNER)
            (root / "bazel-workspace").symlink_to(foreign, target_is_directory=True)
            (root / "crates/linked").symlink_to(foreign_crate, target_is_directory=True)
            (root / "linked-file").mkdir()
            (root / "linked-file/BUILD.bazel").symlink_to(foreign_build)
            obsolete = root / "obsolete/BUILD.bazel"
            obsolete.parent.mkdir()
            obsolete.write_text(gen.BANNER)

            package = {
                "name": "tracedecay", "version": "0.1.0", "dependencies": [],
                "manifest_path": str(product / "Cargo.toml"),
                "features": {"default": ["production"],
                             "production": ["token-counting", "lite", "medium"]},
                "targets": [{"name": "tracedecay", "kind": ["lib"],
                             "crate_types": ["lib"], "edition": "2024", "test": False,
                             "src_path": str(product / "src/lib.rs")}],
            }
            query = mock.Mock(spec=gen.CargoQuery, workspace=root)

            def cargo_output(command):
                if command[0] == "metadata":
                    return json.dumps({"packages": [package]})
                self.assertEqual(command[0], "tree")
                features = "" if "--no-default-features" in command else (
                    "default,lite,medium,production,token-counting"
                )
                return f"tracedecay v0.1.0 ({product})|{features}\n"

            query.run.side_effect = cargo_output
            with mock.patch.object(gen.CargoQuery, "from_bazel", return_value=query), \
                    mock.patch.object(gen, "target_feature_annotations", return_value=None), \
                    mock.patch.object(gen, "TEST_DATA", {}), redirect_stdout(io.StringIO()):
                for arguments in ([], ["--check"]):
                    with self.subTest(arguments=arguments), \
                            mock.patch.object(sys, "argv", [str(SCRIPT), *arguments]):
                        gen.main()
                        self.assertEqual(foreign_build.read_text(), original)
                        self.assertEqual(foreign_crate_build.read_text(), gen.BANNER)
                        self.assertTrue((root / "linked-file/BUILD.bazel").is_symlink())
                        self.assertFalse(obsolete.exists())
            self.assertTrue((product / "BUILD.bazel").is_file())

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


class RuntimeBinaryReachTests(unittest.TestCase):
    SHARED = (
        (
            Path("common/mod.rs"),
            "\n".join([
                "pub fn tracedecay_bin() -> PathBuf {",
                '    PathBuf::from(std::env::var_os("TRACEDECAY_TEST_BIN").unwrap())',
                "}",
                "",
                "pub fn spawn_daemon(home: &Path) -> Child {",
                "    Command::new(tracedecay_bin()).spawn().unwrap()",
                "}",
                "",
                "pub fn start_fleet(home: &Path) -> Child {",
                "    spawn_daemon(home)",
                "}",
                "",
                "pub fn temp_home() -> TempDir {",
                "    tempfile::tempdir().unwrap()",
                "}",
                "",
            ]),
        ),
    )

    def setUp(self) -> None:
        gen = load_gen_builds()
        self.reaches = lambda own_text: gen.reaches_runtime_binary(
            next(p for v, _, p in gen.RUNTIME_BINARIES if v == "TRACEDECAY_TEST_BIN"),
            ((Path("suite/main.rs"), own_text),),
            self.SHARED,
        )

    def test_direct_call(self) -> None:
        self.assertTrue(self.reaches("fn t() { Command::new(tracedecay_bin()); }"))

    def test_call_through_shared_helper(self) -> None:
        self.assertTrue(self.reaches("fn t() { spawn_daemon(home); }"))

    def test_call_through_two_level_helper_chain(self) -> None:
        self.assertTrue(self.reaches("fn t() { start_fleet(home); }"))

    def test_shared_helpers_never_called(self) -> None:
        self.assertFalse(self.reaches("fn t() { let home = temp_home(); }"))

    def test_own_function_named_like_a_helper_is_a_definition(self) -> None:
        self.assertFalse(self.reaches("fn spawn_daemon() {}\nfn t() { temp_home(); }"))


if __name__ == "__main__":
    unittest.main()
