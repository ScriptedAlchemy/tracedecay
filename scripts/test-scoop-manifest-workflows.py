#!/usr/bin/env python3

import json
import re
import textwrap
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def generated_manifest(workflow_name: str, output_name: str) -> dict[str, object]:
    workflow = (ROOT / ".github" / "workflows" / workflow_name).read_text(
        encoding="utf-8"
    )
    match = re.search(
        rf"cat > {re.escape(output_name)} << EOF\n(?P<body>.*?)\n\s+EOF",
        workflow,
        flags=re.DOTALL,
    )
    if match is None:
        raise AssertionError(f"{workflow_name} has no {output_name} heredoc")
    rendered = textwrap.dedent(match.group("body"))
    for variable, value in [
        ("VERSION", "5.1.0"),
        ("TAG", "v5.1.0"),
        ("SHA256", "0" * 64),
    ]:
        rendered = rendered.replace(f"${{{variable}}}", value)
    rendered = rendered.replace(r"\$", "$")
    return json.loads(rendered)


def assert_hook_contract(
    manifest: dict[str, object], package_id: str, state_path: str
) -> None:
    assert manifest["bin"] == "tracedecay.exe"
    prepare = manifest["pre_uninstall"]
    restore = manifest["post_install"]
    cleanup = manifest["post_uninstall"]
    assert isinstance(prepare, str)
    assert isinstance(restore, str)
    assert isinstance(cleanup, str)
    assert f"--package-id {package_id}" in prepare
    assert f"--package-id {package_id}" in restore
    assert "package-hook scoop prepare" in prepare
    assert "package-hook scoop restore" in restore
    assert state_path in prepare
    assert state_path in restore
    assert state_path in cleanup
    assert "$cmd -in @('update', 'uninstall')" in prepare
    assert "$cmd -eq 'update'" in restore
    assert "$cmd -eq 'uninstall'" in cleanup


def main() -> None:
    stable = generated_manifest("release.yml", "tracedecay.json")
    beta = generated_manifest("release-beta.yml", "tracedecay-beta.json")
    assert_hook_contract(
        stable,
        "tracedecay",
        "TraceDecay/service/tracedecay/scoop-state.json",
    )
    assert_hook_contract(
        beta,
        "tracedecay-beta",
        "TraceDecay/service/tracedecay-beta/scoop-state.json",
    )
    assert stable["pre_uninstall"] != beta["pre_uninstall"]
    assert beta["checkver"] == {
        "github": "https://github.com/ScriptedAlchemy/tracedecay",
        "regex": "v([0-9.]+-beta[0-9.]*)",
    }
    print("Scoop manifest workflow contracts passed")


if __name__ == "__main__":
    main()
