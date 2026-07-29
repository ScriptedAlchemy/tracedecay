#!/usr/bin/env python3
"""Static validation for checked-in S11 SQLite runtime evidence."""

from __future__ import annotations

import hashlib
import importlib.util
import json
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent
ARTIFACT_MANIFEST = "artifacts.json"
RUNTIME_MANIFEST = "storage-runtime-fixture-v1.json"
EXPECTED_ARTIFACTS = {
    "wal-pressure.sqlite3",
    "wal-pressure.sqlite3-wal",
    "wal-pressure.blocker.json",
    "crash-after-repair-commit.sqlite3",
    "fts-stale.sqlite3",
    "authoritative-corrupt.sqlite3",
    "online-backup-source.sqlite3",
    "online-backup-copy.sqlite3",
    "online-backup-digest-mismatch.sqlite3",
}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(64 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def quick_check(connection: sqlite3.Connection) -> list[str]:
    return [str(row[0]) for row in connection.execute("PRAGMA quick_check")]


def validate_hashes(manifest: dict[str, object]) -> None:
    artifacts = manifest["artifacts"]
    assert isinstance(artifacts, dict)
    assert set(artifacts) == EXPECTED_ARTIFACTS
    for name, expected in artifacts.items():
        assert isinstance(name, str)
        assert isinstance(expected, dict)
        assert set(expected) == {"bytes", "sha256"}
        assert isinstance(expected["bytes"], int) and expected["bytes"] > 0
        assert (
            isinstance(expected["sha256"], str)
            and len(expected["sha256"]) == 64
            and all(character in "0123456789abcdef" for character in expected["sha256"])
        )
        path = ROOT / name
        assert path.is_file(), f"missing fixture artifact: {name}"
        assert path.stat().st_size == expected["bytes"], name
        assert sha256(path) == expected["sha256"], name
        content = path.read_bytes()
        for live_root in (b"/home/", b"/fast/", b".tracedecay/"):
            assert live_root not in content, f"live profile path leaked into {name}"
    discovered = {
        path.name
        for path in ROOT.iterdir()
        if path.is_file()
        and (
            path.name.endswith(".sqlite3")
            or path.name.endswith(".sqlite3-wal")
            or path.name.endswith(".sqlite3-shm")
            or path.name.endswith(".sqlite3-journal")
            or path.name.endswith(".blocker.json")
        )
    }
    assert discovered == EXPECTED_ARTIFACTS


def validate_wal(manifest: dict[str, object]) -> None:
    with tempfile.TemporaryDirectory(prefix="tracedecay-s11-wal-validate-") as temporary:
        database = Path(temporary) / "wal-pressure.sqlite3"
        shutil.copyfile(ROOT / database.name, database)
        shutil.copyfile(ROOT / f"{database.name}-wal", Path(f"{database}-wal"))
        connection = sqlite3.connect(database)
        assert quick_check(connection) == ["ok"]
        assert connection.execute("PRAGMA journal_mode").fetchone()[0].lower() == "wal"
        assert (
            connection.execute("SELECT count(*) FROM events").fetchone()[0]
            == manifest["expectations"]["wal_pressure_rows"]
        )
        connection.close()

    blocker = json.loads((ROOT / "wal-pressure.blocker.json").read_text())
    assert blocker["snapshot_row_count"] == 1
    assert blocker["committed_row_count"] == manifest["expectations"]["wal_pressure_rows"]
    assert blocker["expected"]["hard_drain_required"] is True
    assert (ROOT / "wal-pressure.sqlite3-wal").stat().st_size > 32


def validate_crash(manifest: dict[str, object]) -> None:
    connection = sqlite3.connect(ROOT / "crash-after-repair-commit.sqlite3")
    assert quick_check(connection) == ["ok"]
    assert (
        connection.execute(
            "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'needle'"
        ).fetchone()[0]
        == 1
    )
    receipt = connection.execute(
        "SELECT receipt_id, evidence_id, binding FROM tracedecay_repair_receipts"
    ).fetchone()
    assert json.loads(receipt[0]) == manifest["expectations"]["crash_receipt_id"]
    assert json.loads(receipt[1]) == manifest["expectations"]["crash_evidence_id"]
    assert json.loads(receipt[2]) == manifest["binding"]
    connection.close()


def validate_fts_repair(manifest: dict[str, object]) -> None:
    with tempfile.TemporaryDirectory(prefix="tracedecay-s11-fts-validate-") as temporary:
        database = Path(temporary) / "fts-stale.sqlite3"
        shutil.copyfile(ROOT / database.name, database)
        connection = sqlite3.connect(database)
        assert quick_check(connection) == ["ok"]
        term = manifest["expectations"]["fts_search_term"]
        assert (
            connection.execute(
                "SELECT count(*) FROM documents WHERE body LIKE ?",
                (f"%{term}%",),
            ).fetchone()[0]
            == 1
        )
        assert (
            connection.execute(
                "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH ?",
                (term,),
            ).fetchone()[0]
            == 0
        )
        connection.execute("INSERT INTO documents_fts(documents_fts) VALUES ('rebuild')")
        connection.commit()
        assert (
            connection.execute(
                "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH ?",
                (term,),
            ).fetchone()[0]
            == 1
        )
        assert quick_check(connection) == ["ok"]
        connection.close()


def validate_authoritative_corruption(manifest: dict[str, object]) -> None:
    connection = sqlite3.connect(ROOT / "authoritative-corrupt.sqlite3")
    messages = quick_check(connection)
    assert messages != ["ok"]
    assert any("database main" in message or "facts" in message for message in messages)
    assert (
        connection.execute("SELECT count(*) FROM facts").fetchone()[0]
        == manifest["expectations"]["authoritative_rows"]
    )
    connection.close()


def validate_online_backup(manifest: dict[str, object]) -> None:
    rows = []
    contents = []
    for name in (
        "online-backup-source.sqlite3",
        "online-backup-copy.sqlite3",
        "online-backup-digest-mismatch.sqlite3",
    ):
        connection = sqlite3.connect(ROOT / name)
        assert quick_check(connection) == ["ok"]
        rows.append(connection.execute("SELECT count(*) FROM facts").fetchone()[0])
        assert connection.execute("SELECT count(*) FROM evidence_rows").fetchone()[0] == 1
        contents.append(connection.execute("SELECT id, body FROM facts ORDER BY id").fetchall())
        connection.close()
    assert rows == [manifest["expectations"]["backup_rows"]] * 2 + [
        manifest["expectations"]["backup_rows"] + 1
    ]
    assert contents[0] == contents[1]
    assert sha256(ROOT / "online-backup-copy.sqlite3") != sha256(
        ROOT / "online-backup-digest-mismatch.sqlite3"
    )


def validate_reproducible_generation() -> None:
    with tempfile.TemporaryDirectory(prefix="tracedecay-s11-reproducible-") as temporary:
        regenerated = Path(temporary)
        subprocess.run(
            [
                sys.executable,
                str(ROOT / "generate.py"),
                "--output",
                str(regenerated),
            ],
            check=True,
        )
        expected = EXPECTED_ARTIFACTS | {ARTIFACT_MANIFEST, RUNTIME_MANIFEST}
        assert {path.name for path in regenerated.iterdir()} == expected
        for name in sorted(expected):
            assert (regenerated / name).read_bytes() == (ROOT / name).read_bytes(), name


def validate_consumers() -> None:
    repository = ROOT.parents[2]
    adapter_path = repository / "benchmarks/runtime/storage_workloads.py"
    specification = importlib.util.spec_from_file_location(
        "storage_runtime_workload_kernel",
        adapter_path,
    )
    assert specification is not None and specification.loader is not None
    adapter = importlib.util.module_from_spec(specification)
    sys.modules[specification.name] = adapter
    specification.loader.exec_module(adapter)
    adapter.validate_workloads()
    assert adapter.BENCHMARK_AUTHORITY == "measurement_fixture_not_product_contract"
    assert "tracedecay-store" in adapter.DECLARED_CRATE_LANES
    assert "tracedecay-rusqlite-runtime" in adapter.DECLARED_CRATE_LANES

    identities = adapter.runtime_test_identities(
        platform="fixture",
        shard="storage-evidence",
        storage_mode="isolated-sqlite",
    )
    assert any(
        identity.crate_tag == "tracedecay-rusqlite-runtime"
        for identity in identities
    )


def main() -> None:
    manifest = json.loads((ROOT / ARTIFACT_MANIFEST).read_text(encoding="utf-8"))
    assert manifest["schema"] == "tracedecay.storage-runtime-evidence.s11.v1"
    runtime_manifest = json.loads((ROOT / RUNTIME_MANIFEST).read_text(encoding="utf-8"))
    assert runtime_manifest == {
        "schema_version": 1,
        "project_root": ".",
        "profile_root": ".",
        "fts_queries": {"graph": "needle", "session": "needle"},
        "s11": {
            "database": "online-backup-source.sqlite3",
            "binding": manifest["binding"],
            "evidence_tables": ["evidence_rows", "facts"],
        },
    }
    validate_hashes(manifest)
    validate_wal(manifest)
    validate_crash(manifest)
    validate_fts_repair(manifest)
    validate_authoritative_corruption(manifest)
    validate_online_backup(manifest)
    validate_reproducible_generation()
    validate_consumers()
    print(
        "validated deterministic S11 SQLite evidence: canonical manifest, "
        "WAL blocker, crash receipt, FTS repair, quarantine source, "
        "online backup, and digest mismatch"
    )


if __name__ == "__main__":
    main()
