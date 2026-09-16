#!/usr/bin/env python3
"""Real compiler -> durable cycle -> impact/list, including opaque pagination.

Run: python3 tests/tool_sweep_suite/feedback_journey.py --bin /path/to/tracedecay \
    --out /path/to/new-evidence-directory
The two phases use the same private profile with a daemon restart between them.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from orchestrator import _phase_environment, run_bounded_command
from runner import McpClient
from outcomes import objects, response_problem_code

RUNTIME = Path.cwd()
PROJECT = RUNTIME / "project"
EVIDENCE: Path
BIN: Path
SOURCE = Path(__file__).resolve().parents[2]
MISSING = "missing_feedback"
RUSTC = os.environ.get("RUSTC", "rustc")


def write_json(name: str, value: object) -> None:
    (EVIDENCE / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def command(args: list[str], *, timeout: int = 120, cwd: Path | None = None) -> dict[str, object]:
    started = time.monotonic()
    run = subprocess.run(args, cwd=cwd or PROJECT, text=True, capture_output=True, timeout=timeout)
    try:
        stdout: object = json.loads(run.stdout)
    except json.JSONDecodeError:
        stdout = run.stdout
    return {"command": args, "exit_code": run.returncode, "elapsed_ms": round((time.monotonic() - started) * 1000), "stdout": stdout, "stderr": run.stderr}


def tool(name: str, arguments: dict[str, object], stem: str, *, timeout: int = 120) -> dict[str, object]:
    row = command([str(BIN), "tool", "--project", str(PROJECT), name, "--args", json.dumps(arguments), "--json"], timeout=timeout)
    row.update({"tool": name, "arguments": arguments})
    write_json(stem + ".json", row)
    return row


def catalog() -> None:
    client = McpClient(BIN, PROJECT, EVIDENCE / "mcp-catalog.stderr")
    try:
        client.initialize(30_000)
        wanted = {
            "tracedecay_diagnose", "tracedecay_feedback_advisory_cycle",
            "tracedecay_feedback_impact", "tracedecay_feedback_list",
        }
        definitions = [item for item in client.list_tools(30_000) if item.get("name") in wanted]
        write_json("negotiated-tool-schemas.json", definitions)
        if {item.get("name") for item in definitions} != wanted:
            raise RuntimeError(f"missing public feedback tools: {wanted - {item.get('name') for item in definitions}}")
    finally:
        client.close()


def produce() -> None:
    PROJECT.mkdir()
    (PROJECT / "src").mkdir()
    sources = {}
    for stem in ("lib", "other"):
        # Two file-scoped cycles exceed the list page without exceeding a cycle's bound.
        text = ("pub mod other;\n" if stem == "lib" else "")
        text += f"pub fn feedback_entry_{stem}(input: u32) -> u32 {{\n"
        text += "".join(f"    {MISSING}_{stem}_{i}(input);\n" for i in range(60))
        text += "    input\n}\n#[cfg(test)]\nmod tests {\n    #[test]\n"
        text += f"    fn feedback_entry_{stem}_test() {{ assert_eq!(super::feedback_entry_{stem}(1), 1); }}\n}}\n"
        path = PROJECT / "src" / f"{stem}.rs"
        path.write_text(text)
        sources[str(path)] = hashlib.sha256(path.read_bytes()).hexdigest()
    (PROJECT / "Cargo.toml").write_text('[package]\nname="feedback-public-replay"\nversion="0.1.0"\nedition="2024"\n')
    for args in (["git", "init", "-q"], ["git", "config", "user.email", "audit@example.invalid"], ["git", "config", "user.name", "TraceDecay Audit"], ["git", "add", "."], ["git", "commit", "-q", "-m", "seed"]):
        assert command(list(args))["exit_code"] == 0
    init = command([str(BIN), "init", str(PROJECT)], timeout=180)
    write_json("01-init.json", init)
    assert init["exit_code"] == 0, init
    catalog()
    compiler_dir = RUNTIME / "compiler-output"
    compiler_dir.mkdir()
    compiled = command([str(RUSTC), "--crate-type=lib", "--edition=2024", "--error-format=short", "src/lib.rs", "--out-dir", str(compiler_dir)])
    write_json("02-real-rustc-failure.json", compiled)
    assert compiled["exit_code"] != 0 and MISSING in compiled["stderr"], compiled
    deadline = time.monotonic() + 90
    attempt = 0
    while True:
        diagnose = tool("tracedecay_diagnose", {"cargo_output": compiled["stderr"], "include_callers": True, "max_diagnostics": 120, "format": "json"}, f"03-diagnose-{attempt}")
        published = next((item for item in objects(diagnose["stdout"]) if isinstance(item.get("published"), dict)), None)
        if diagnose["exit_code"] == 0 and published is not None:
            break
        assert str(diagnose["stderr"]).startswith("Error: project route error (code-graph-unavailable):") and time.monotonic() < deadline, diagnose
        attempt += 1
        time.sleep(0.25)
    assert published["published"]["status"] == "published", published
    assert published["published"]["inserted"] == 120, published["published"]
    cycles = []
    for stem in ("lib", "other"):
        row = tool("tracedecay_feedback_advisory_cycle", {"document_uri": (PROJECT / "src" / f"{stem}.rs").as_uri(), "format": "json"}, f"04-cycle-{stem}", timeout=180)
        cycle = next((item for item in objects(row["stdout"]) if isinstance(item.get("cycle"), dict) and isinstance(item.get("finding_handles"), list)), None)
        assert row["exit_code"] == 0 and cycle is not None, row
        assert len(cycle["finding_handles"]) == 60, cycle
        cycles.append(cycle)
    diagnostic = next((item for item in objects(diagnose["stdout"]) if item.get("code") == "E0425" and item.get("file") == "src/other.rs" and item.get("callers")), None)
    assert diagnostic is not None, diagnose
    expected_test = next(c["node_id"] for c in diagnostic["callers"] if c["name"] == "feedback_entry_other_test")
    state = {"sources": sources, "cycles": cycles, "expected_test": expected_test}
    (RUNTIME / "producer-state.json").write_text(json.dumps(state, indent=2) + "\n")
    write_json("producer-oracle.json", state)


def evidence(row: dict) -> dict:
    assert row["exit_code"] == 0, row
    envelope = row["stdout"]
    assert envelope["outcome"]["outcome"] == "evidence", envelope
    return envelope["outcome"]["value"]


def consume() -> None:
    state = json.loads((RUNTIME / "producer-state.json").read_text())
    expected = {f["finding_id"]: c["cycle"]["cycle_id"] for c in state["cycles"] for f in c["finding_handles"]}
    assert len(expected) == 120
    cycle = state["cycles"][-1]
    packet = evidence(tool("tracedecay_feedback_impact", {"request_handle": cycle["read_handles"]["impact_handle"], "format": "json"}, "05-impact"))
    impact = packet["payload"]
    assert impact["cycle_id"] == cycle["cycle"]["cycle_id"], impact
    assert impact["impact"] == cycle["cycle"]["impact"], impact
    assert state["expected_test"] in impact["impact"]["affected_tests"], impact
    assert impact["impact"]["target"]["file"] in impact["impact"]["affected_files"], impact
    assert impact["state"] == cycle["cycle"]["impact_state"], impact
    assert impact["state"] in ("complete", "partial"), impact
    # Retrieval completeness is separate from the retained projection state.
    assert packet["coverage"]["completeness"] == "complete", packet
    assert packet["execution"]["termination"] == "completed", packet
    handle = state["cycles"][-1]["read_handles"]["list_handle"]
    seen, cursors, pages = [], set(), []
    while True:
        assert handle not in cursors, "continuation repeated"
        cursors.add(handle)
        packet = evidence(tool("tracedecay_feedback_list", {"request_handle": handle, "format": "json"}, f"06-list-{len(pages)}"))
        findings = packet["payload"]["findings"]
        ids = [item["finding"]["finding_id"] for item in findings]
        assert ids == sorted(ids)
        assert all(item["cycle_id"] == expected[item["finding"]["finding_id"]] for item in findings)
        assert packet["page"]["total"] == len(expected), packet
        assert packet["page"]["returned"] == len(ids), packet
        assert packet["coverage"]["returned"] == len(ids), packet
        assert packet["coverage"]["completeness"] == "complete", packet
        seen.extend(ids)
        pages.append(packet["page"])
        cursor = packet["page"]["cursor"]
        if cursor is None:
            break
        assert cursor["kind"] == "opaque", cursor
        handle = cursor["cursor"]  # Pass opaque bytes verbatim back to the authority.
    assert len(pages) > 1 and len(seen) == len(expected) and set(seen) == set(expected), seen
    assert seen == sorted(seen), seen
    for name in ("feedback_impact", "feedback_list"):
        for label, handle in (("unknown", "tool-sweep-unknown-request-handle.v1"), ("wrong-operation", state["cycles"][0]["finding_handles"][0]["get_handle"])):
            row = tool("tracedecay_" + name, {"request_handle": handle, "format": "json"}, f"07-{name}-{label}")
            assert row["exit_code"] != 0, row
            assert response_problem_code(row["stdout"]) == ("not_found_or_not_authorized", "not_found_or_not_authorized"), row
    for path, digest in state["sources"].items():
        assert hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest
    write_json("consumer-oracle.json", {"result": "passed", "unique_findings": len(seen), "pages": pages, "impact_cycles": 1, "typed_denials": 4, "source_unchanged": True})


def main() -> None:
    global EVIDENCE, BIN
    if len(sys.argv) == 2 and sys.argv[1] in ("produce", "consume"):
        EVIDENCE = Path(os.environ["TRACEDECAY_FEEDBACK_EVIDENCE"])
        BIN = Path(os.environ["TRACEDECAY_AUDIT_BIN"])
        {"produce": produce, "consume": consume}[sys.argv[1]]()
        return
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    BIN, EVIDENCE = args.bin.resolve(), args.out.resolve()
    EVIDENCE.mkdir(parents=True, exist_ok=False)
    runtime = EVIDENCE / "runtime"
    runtime.mkdir(mode=0o700)

    with tempfile.TemporaryDirectory(prefix="td-feedback-") as temporary:
        env = _phase_environment(runtime, temp_root=Path(temporary))
        env.update({
            "TRACEDECAY_DAEMON_HARNESS_PROFILE_DIR": str(runtime / "profile"),
            "TRACEDECAY_FEEDBACK_EVIDENCE": str(EVIDENCE),
            "TRACEDECAY_AUDIT_BIN": str(BIN),
            "HOTPATH_METRICS_SERVER_OFF": "1",
        })
        with BIN.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        version = subprocess.check_output([str(BIN), "--version"], cwd=runtime, env=env, text=True, timeout=30).strip()
        write_json("binary.json", {"binary": str(BIN), "sha256": digest, "version": version})
        for phase in ("produce", "consume"):
            with (EVIDENCE / f"{phase}.stdout").open("wb") as stdout, (EVIDENCE / f"{phase}.stderr").open("wb") as stderr:
                result = run_bounded_command([
                    str(SOURCE / "scripts/with-isolated-tracedecay-daemon.sh"),
                    "--bin", str(BIN), "--lifecycle-label", phase, "--",
                    sys.executable, str(Path(__file__).resolve()), phase,
                ], cwd=runtime, environment=env, remaining_s=420, stdout=stdout, stderr=stderr)
            assert result.returncode == 0, (EVIDENCE / f"{phase}.stderr").read_text()
    print(json.dumps({"result": "passed", "evidence": str(EVIDENCE), "binary_sha256": digest}))


if __name__ == "__main__":
    main()
