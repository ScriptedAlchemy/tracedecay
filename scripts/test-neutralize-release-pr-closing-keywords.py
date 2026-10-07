#!/usr/bin/env python3
"""The release PR body rewrite must leave no GitHub closing keyword."""

from __future__ import annotations

import importlib.util
import os
import re
import shutil
import subprocess
import sys
import tempfile
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


class ManualReleasePrRefreshTests(unittest.TestCase):
    """A manual release-pr refresh runs only the lockfile script, from a
    linked worktree of the operator's repository."""

    def run_refresh(
        self, scratch_path: Path, cargo_stub: str, bazel_stub: str = ""
    ) -> tuple[subprocess.CompletedProcess[str], dict[str, str], Path, Path, bytes]:
        main = scratch_path / "main"
        checkout = scratch_path / "checkout"
        remote = scratch_path / "remote.git"
        global_config = scratch_path / "global.gitconfig"
        global_config.write_text("[user]\n\tname = Global Operator\n")
        git_env = {
            **os.environ,
            "GIT_CONFIG_GLOBAL": str(global_config),
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.invalid",
        }

        def git(*args: str, cwd: Path = main) -> None:
            subprocess.run(["git", *args], cwd=cwd, check=True, env=git_env)

        (main / "scripts").mkdir(parents=True)
        for name in (
            "update-release-pr-lockfile.sh",
            "neutralize-release-pr-closing-keywords.sh",
            "neutralize-release-pr-closing-keywords.py",
        ):
            shutil.copy2(ROOT / "scripts" / name, main / "scripts" / name)
        (main / "scripts" / "bazel").mkdir()
        generator = main / "scripts" / "bazel" / "gen_builds.py"
        generator.write_text(bazel_stub)
        for package in ("sdks/typescript", "plugin/chatgpt-extension"):
            path = main / package / "package.json"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('{"scripts":{"build":"node build.mjs"}}\n')
            (path.parent / "build.mjs").write_text(
                "import {mkdirSync, writeFileSync} from 'node:fs';\n"
                "if (process.cwd().endsWith('chatgpt-extension')) {\n"
                "  mkdirSync('embedded', {recursive: true});\n"
                "  writeFileSync('embedded/app.html', 'app beta.58\\n');\n"
                "  writeFileSync('embedded/server.mjs', 'server beta.58\\n');\n"
                "}\n"
            )
        for path in (
            "crates/tracedecay/BUILD.bazel",
            "crates/tracedecay-cli/BUILD.bazel",
            "crates/tracedecay-project/BUILD.bazel",
        ):
            build = main / path
            build.parent.mkdir(parents=True, exist_ok=True)
            build.write_text('version = "1.0.0-beta.57"\n')
        (main / "package.json").write_text('{"private":true}\n')
        subprocess.run(
            ["pnpm", "install", "--lockfile-only"],
            cwd=main,
            check=True,
            capture_output=True,
            text=True,
        )
        (main / "version.txt").write_text("1.0.0-beta.58\n")
        (main / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.95.0"\n')
        (main / "Cargo.lock").write_text("version = 4\n")
        git("init", "-q", "-b", "main")
        git("config", "user.name", "Operator")
        git("config", "user.email", "operator@example.invalid")
        git("init", "-q", "--bare", str(remote))
        git("remote", "add", "origin", str(remote))
        git("add", ".")
        git("commit", "-q", "-m", "fixture")
        git("worktree", "add", "-q", "-b", "release-please--x", str(checkout))

        bin_dir = scratch_path / "bin"
        bin_dir.mkdir()
        body_path = scratch_path / "body.md"
        body_path.write_text(RELEASE_PLEASE_BODY)
        edited_path = scratch_path / "edited.md"
        stubs = {
            "cargo": cargo_stub.format(lockfile=checkout / "Cargo.lock"),
            "gh": (
                "#!/bin/sh\n"
                'case "$1 $2" in\n'
                f'  "pr view") cat "{body_path}" ;;\n'
                '  "pr edit") while [ "$#" -gt 0 ]; do\n'
                '      if [ "$1" = --body-file ]; then '
                f'cp "$2" "{edited_path}"; fi; shift; done ;;\n'
                "  *) exit 64 ;;\n"
                "esac\n"
            ),
        }
        for name, source in stubs.items():
            stub = bin_dir / name
            stub.write_text(source)
            stub.chmod(0o755)

        shared_config_before = (main / ".git" / "config").read_bytes()
        result = subprocess.run(
            ["bash", "scripts/update-release-pr-lockfile.sh"],
            cwd=checkout,
            env={
                **git_env,
                "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                "RELEASE_PR_JSON": '{"number": 42, "headBranchName": "release-please--x"}',
                "GH_TOKEN": "unused",
                "GITHUB_REPOSITORY": "ScriptedAlchemy/tracedecay",
            },
            capture_output=True,
            text=True,
        )
        return result, git_env, edited_path, remote, shared_config_before

    def test_build_refresh_neutralizes_the_release_pr_body(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            result, _, edited_path, _, _ = self.run_refresh(
                Path(scratch),
                "#!/bin/sh\nexit 0\n",
                """from pathlib import Path
for path in (
    "crates/tracedecay/BUILD.bazel",
    "crates/tracedecay-cli/BUILD.bazel",
    "crates/tracedecay-project/BUILD.bazel",
):
    Path(path).write_text('version = "1.0.0-beta.58"\\n')
""",
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(edited_path.exists(), "the release PR body was never rewritten")
            self.assertEqual(edited_path.read_text(), RELEASE_PLEASE_BODY_NEUTRALIZED)

    def test_lockfile_commit_leaves_the_operator_git_config_untouched(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            scratch_path = Path(scratch)
            shared_config = scratch_path / "main" / ".git" / "config"
            global_config = scratch_path / "global.gitconfig"
            result, git_env, _, remote, shared_config_before = self.run_refresh(
                scratch_path,
                "#!/bin/sh\nprintf 'version = 4\\n# bumped\\n' > '{lockfile}'\n",
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(shared_config.read_bytes(), shared_config_before)
            self.assertEqual(global_config.read_text(), "[user]\n\tname = Global Operator\n")
            operator = subprocess.run(
                ["git", "config", "user.name"],
                cwd=scratch_path / "main",
                env=git_env,
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            self.assertEqual(operator, "Operator")
            pushed = subprocess.run(
                [
                    "git",
                    "log",
                    "-1",
                    "--format=%an <%ae>|%cn <%ce>|%s",
                    "release-please--x",
                ],
                cwd=remote,
                env=git_env,
                check=True,
                capture_output=True,
                text=True,
            ).stdout.strip()
            bot = "github-actions[bot] <41898282+github-actions[bot]@users.noreply.github.com>"
            self.assertEqual(
                pushed, f"{bot}|{bot}|chore(release): update generated metadata"
            )


if __name__ == "__main__":
    unittest.main(verbosity=2)
