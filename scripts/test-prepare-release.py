#!/usr/bin/env python3
"""Behavioral tests for changelog promotion during release prep."""

from __future__ import annotations

import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "scripts" / "prepare-release.py"

CONVENTIONAL_HEADER = (
    "## [0.0.73](https://github.com/ScriptedAlchemy/tracedecay/"
    "compare/v0.0.72...v0.0.73) (2026-08-04)"
)
LINKED_KEEPACHANGELOG = (
    "## [0.0.68](https://github.com/ScriptedAlchemy/tracedecay/"
    "compare/v0.0.67...v0.0.68) - 2026-08-03"
)


def run(
    changelog: Path,
    *extra: str,
    cargo_toml: Path | None = None,
) -> subprocess.CompletedProcess[str]:
    command = ["python3", str(SCRIPT), *extra, "--changelog", str(changelog)]
    if cargo_toml is not None:
        command.extend(["--cargo-toml", str(cargo_toml)])
    return subprocess.run(
        command,
        check=False,
        capture_output=True,
        text=True,
    )


def write_changelog(directory: Path, text: str) -> Path:
    path = directory / "CHANGELOG.md"
    path.write_text(text, encoding="utf-8")
    return path


def test_refuses_unrecognized_versionish_header(temp: Path) -> None:
    changelog = write_changelog(
        temp,
        "\n".join(
            [
                "# Changelog",
                "",
                "## [Unreleased]",
                "",
                "### Added",
                "",
                "- new thing",
                "",
                "## [0.0.68] not-a-version-header",
                "",
                "- old thing",
                "",
            ]
        ),
    )
    before = changelog.read_text(encoding="utf-8")
    result = run(changelog, "0.0.73")
    assert result.returncode == 1, result.stderr
    assert "unrecognized version header" in result.stderr
    assert changelog.read_text(encoding="utf-8") == before


def test_refuses_merge_into_preceding_published_version(temp: Path) -> None:
    changelog = write_changelog(
        temp,
        "\n".join(
            [
                "# Changelog",
                "",
                CONVENTIONAL_HEADER,
                "",
                "### Bug Fixes",
                "",
                "- already shipped",
                "",
                "## [Unreleased]",
                "",
                "### Removed",
                "",
                "- future 0.1.0 break",
                "",
                LINKED_KEEPACHANGELOG,
                "",
                "### Fixed",
                "",
                "- older shipped fix",
                "",
            ]
        ),
    )
    before = changelog.read_text(encoding="utf-8")
    result = run(changelog, "0.0.73")
    assert result.returncode == 1, result.stdout + result.stderr
    assert "already-published [0.0.73]" in result.stderr
    assert changelog.read_text(encoding="utf-8") == before


def test_promotes_unreleased_when_version_absent(temp: Path) -> None:
    changelog = write_changelog(
        temp,
        "\n".join(
            [
                "# Changelog",
                "",
                "## [Unreleased]",
                "",
                "### Added",
                "",
                "- new thing",
                "",
                LINKED_KEEPACHANGELOG,
                "",
                "### Fixed",
                "",
                "- older shipped fix",
                "",
            ]
        ),
    )
    result = run(changelog, "0.0.73")
    assert result.returncode == 0, result.stderr
    text = changelog.read_text(encoding="utf-8")
    assert "## [Unreleased]" in text
    assert "## [0.0.73] - " in text
    assert "- new thing" in text
    assert LINKED_KEEPACHANGELOG in text
    assert text.index("## [Unreleased]") < text.index("## [0.0.73] - ")
    assert text.index("## [0.0.73] - ") < text.index(LINKED_KEEPACHANGELOG)


def test_merges_when_sparse_version_follows_unreleased(temp: Path) -> None:
    changelog = write_changelog(
        temp,
        "\n".join(
            [
                "# Changelog",
                "",
                "## [Unreleased]",
                "",
                "### Added",
                "",
                "- late note",
                "",
                "## [0.0.73] - 2026-08-04",
                "",
                "### Fixed",
                "",
                "- early staged fix",
                "",
            ]
        ),
    )
    result = run(changelog, "0.0.73")
    assert result.returncode == 0, result.stderr
    text = changelog.read_text(encoding="utf-8")
    assert "- late note" in text
    assert "- early staged fix" in text
    unreleased_body = text.split("## [Unreleased]", 1)[1].split("## [0.0.73]", 1)[0]
    assert "- late note" not in unreleased_body


def test_empty_unreleased_is_noop(temp: Path) -> None:
    changelog = write_changelog(
        temp,
        "\n".join(
            [
                "# Changelog",
                "",
                "## [Unreleased]",
                "",
                CONVENTIONAL_HEADER,
                "",
                "### Bug Fixes",
                "",
                "- already shipped",
                "",
            ]
        ),
    )
    before = changelog.read_text(encoding="utf-8")
    result = run(changelog, "0.0.73")
    assert result.returncode == 0, result.stderr
    assert "empty" in result.stdout
    assert changelog.read_text(encoding="utf-8") == before


def main() -> None:
    with tempfile.TemporaryDirectory() as temp_name:
        temp = Path(temp_name)
        for name, test in (
            ("unrecognized", test_refuses_unrecognized_versionish_header),
            ("preceding", test_refuses_merge_into_preceding_published_version),
            ("promote", test_promotes_unreleased_when_version_absent),
            ("merge", test_merges_when_sparse_version_follows_unreleased),
            ("empty", test_empty_unreleased_is_noop),
        ):
            directory = temp / name
            directory.mkdir()
            test(directory)
    print("prepare-release changelog tests passed")


if __name__ == "__main__":
    main()
