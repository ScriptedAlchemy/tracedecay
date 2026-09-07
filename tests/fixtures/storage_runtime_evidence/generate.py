#!/usr/bin/env python3
"""Generate S11 crash, corruption, and backup/restore SQLite evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parent
PAGE_SIZE = 1024
ARTIFACT_MANIFEST = "artifacts.json"
RUNTIME_MANIFEST = "storage-runtime-fixture-v1.json"
FIXTURE_FILES = (
    "wal-pressure.sqlite3",
    "wal-pressure.sqlite3-wal",
    "wal-pressure.blocker.json",
    "crash-after-repair-commit.sqlite3",
    "fts-stale.sqlite3",
    "authoritative-corrupt.sqlite3",
    "online-backup-source.sqlite3",
    "online-backup-copy.sqlite3",
    "online-backup-digest-mismatch.sqlite3",
)
BINDING = {
    "shard_id": {
        "brain_id": "brain.s11.evidence",
        "profile_id": "profile.s11.evidence",
        "scope": {
            "kind": "project",
            "project_id": "project.s11.evidence",
        },
    },
    "incarnation": 7,
    "authority_epoch": 19,
}


def canonical_json(value: object) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def configure(connection: sqlite3.Connection) -> None:
    connection.execute(f"PRAGMA page_size={PAGE_SIZE}")
    connection.execute("PRAGMA foreign_keys=ON")
    connection.execute("PRAGMA application_id=0x54445331")
    connection.execute("PRAGMA user_version=11")


def quick_check(connection: sqlite3.Connection) -> list[str]:
    return [str(row[0]) for row in connection.execute("PRAGMA quick_check")]


def wal_checksum(
    content: bytes,
    byte_order: str,
    state: tuple[int, int] = (0, 0),
) -> tuple[int, int]:
    first, second = state
    if len(content) % 8 != 0:
        raise ValueError("WAL checksum input must contain complete word pairs")
    for offset in range(0, len(content), 8):
        left = int.from_bytes(content[offset : offset + 4], byte_order)
        right = int.from_bytes(content[offset + 4 : offset + 8], byte_order)
        first = (first + left + second) & 0xFFFFFFFF
        second = (second + right + first) & 0xFFFFFFFF
    return first, second


def canonicalize_wal(path: Path) -> None:
    content = bytearray(path.read_bytes())
    if len(content) < 32:
        raise RuntimeError("generated WAL is missing its header")
    magic = int.from_bytes(content[0:4], "big")
    if magic not in (0x377F0682, 0x377F0683):
        raise RuntimeError("generated WAL has an unsupported magic")
    page_size = int.from_bytes(content[8:12], "big")
    if page_size == 1:
        page_size = 65_536
    frame_size = 24 + page_size
    if page_size <= 0 or (len(content) - 32) % frame_size != 0:
        raise RuntimeError("generated WAL has incomplete frames")

    first_salt = 0x53313145
    second_salt = 0x56494431
    content[16:20] = first_salt.to_bytes(4, "big")
    content[20:24] = second_salt.to_bytes(4, "big")
    byte_order = "big" if magic & 1 else "little"
    checksum = wal_checksum(bytes(content[:24]), byte_order)
    content[24:28] = checksum[0].to_bytes(4, "big")
    content[28:32] = checksum[1].to_bytes(4, "big")

    for offset in range(32, len(content), frame_size):
        content[offset + 8 : offset + 12] = first_salt.to_bytes(4, "big")
        content[offset + 12 : offset + 16] = second_salt.to_bytes(4, "big")
        checksum_input = (
            bytes(content[offset : offset + 8])
            + bytes(content[offset + 24 : offset + frame_size])
        )
        checksum = wal_checksum(checksum_input, byte_order, checksum)
        content[offset + 16 : offset + 20] = checksum[0].to_bytes(4, "big")
        content[offset + 20 : offset + 24] = checksum[1].to_bytes(4, "big")
    path.write_bytes(content)


def create_wal_pressure() -> None:
    with tempfile.TemporaryDirectory(prefix="tracedecay-s11-wal-") as temporary:
        database = Path(temporary) / "wal-pressure.sqlite3"
        writer = sqlite3.connect(database)
        configure(writer)
        assert writer.execute("PRAGMA journal_mode=WAL").fetchone()[0].lower() == "wal"
        writer.execute("PRAGMA wal_autocheckpoint=0")
        writer.executescript(
            """
            CREATE TABLE events (
                sequence INTEGER PRIMARY KEY,
                payload BLOB NOT NULL
            ) STRICT;
            INSERT INTO events VALUES (1, zeroblob(700));
            """
        )
        writer.commit()
        writer.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()

        blocker = sqlite3.connect(database)
        blocker.execute("BEGIN")
        assert blocker.execute("SELECT count(*) FROM events").fetchone()[0] == 1
        writer.executemany(
            "INSERT INTO events VALUES (?, zeroblob(700))",
            ((sequence,) for sequence in range(2, 130)),
        )
        writer.commit()

        wal = Path(f"{database}-wal")
        assert wal.stat().st_size > 32
        shutil.copyfile(database, ROOT / database.name)
        shutil.copyfile(wal, ROOT / wal.name)
        blocker.close()
        writer.close()
    canonicalize_wal(ROOT / "wal-pressure.sqlite3-wal")

    (ROOT / "wal-pressure.blocker.json").write_text(
        json.dumps(
            {
                "schema": "tracedecay.storage-runtime-evidence.wal-blocker.v1",
                "lease_id": "snapshot.s11.wal-pressure",
                "snapshot_row_count": 1,
                "committed_row_count": 129,
                "expected": {
                    "wal_frames_minimum": 1,
                    "checkpointed_less_than_log_while_blocked": True,
                    "hard_drain_required": True,
                },
            },
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )


def create_documents_schema(connection: sqlite3.Connection) -> None:
    configure(connection)
    connection.executescript(
        """
        CREATE TABLE documents (
            id INTEGER PRIMARY KEY,
            body TEXT NOT NULL
        ) STRICT;
        CREATE VIRTUAL TABLE documents_fts USING fts5(
            body,
            content='documents',
            content_rowid='id'
        );
        """
    )


def create_fts_stale() -> None:
    path = ROOT / "fts-stale.sqlite3"
    connection = sqlite3.connect(path)
    create_documents_schema(connection)
    connection.execute("INSERT INTO documents VALUES (1, 'stable baseline')")
    connection.execute("INSERT INTO documents_fts(documents_fts) VALUES ('rebuild')")
    connection.commit()
    connection.execute("INSERT INTO documents VALUES (2, 's11 repair needle')")
    connection.commit()
    assert quick_check(connection) == ["ok"]
    assert (
        connection.execute(
            "SELECT count(*) FROM documents_fts WHERE documents_fts MATCH 'needle'"
        ).fetchone()[0]
        == 0
    )
    connection.close()


def crash_child(path: Path) -> None:
    connection = sqlite3.connect(path)
    create_documents_schema(connection)
    connection.execute("INSERT INTO documents VALUES (1, 'committed repair needle')")
    connection.execute(
        """
        CREATE TABLE tracedecay_repair_receipts (
            receipt_id TEXT PRIMARY KEY NOT NULL,
            evidence_id TEXT NOT NULL,
            binding TEXT NOT NULL
        ) STRICT
        """
    )
    connection.commit()
    connection.execute("BEGIN IMMEDIATE")
    connection.execute("INSERT INTO documents_fts(documents_fts) VALUES ('rebuild')")
    connection.execute(
        """
        INSERT INTO tracedecay_repair_receipts
            (receipt_id, evidence_id, binding)
        VALUES (?, ?, ?)
        """,
        (
            canonical_json("receipt.s11.crash"),
            canonical_json("evidence.s11.crash"),
            canonical_json(BINDING),
        ),
    )
    connection.commit()
    os._exit(73)


def create_crash_after_commit() -> None:
    path = ROOT / "crash-after-repair-commit.sqlite3"
    result = subprocess.run(
        [sys.executable, str(Path(__file__).resolve()), "--crash-child", str(path)],
        check=False,
    )
    if result.returncode != 73:
        raise RuntimeError(f"crash child exited {result.returncode}, expected 73")
    connection = sqlite3.connect(path)
    assert quick_check(connection) == ["ok"]
    assert (
        connection.execute("SELECT count(*) FROM tracedecay_repair_receipts").fetchone()[0]
        == 1
    )
    connection.close()


def create_authoritative_corruption() -> None:
    path = ROOT / "authoritative-corrupt.sqlite3"
    connection = sqlite3.connect(path)
    configure(connection)
    connection.execute(
        "CREATE TABLE facts (id INTEGER PRIMARY KEY, body TEXT NOT NULL) STRICT"
    )
    connection.executemany(
        "INSERT INTO facts(body) VALUES (?)",
        ((f"authoritative-{number:03d}-" * 12,) for number in range(200)),
    )
    connection.commit()
    leaf_page = connection.execute(
        """
        SELECT pageno
        FROM dbstat
        WHERE name = 'facts' AND pagetype = 'leaf'
        ORDER BY pageno DESC
        LIMIT 1
        """
    ).fetchone()[0]
    connection.close()

    content = bytearray(path.read_bytes())
    page_offset = (int(leaf_page) - 1) * PAGE_SIZE
    cell_count = int.from_bytes(content[page_offset + 3 : page_offset + 5], "big")
    if cell_count == 0:
        raise RuntimeError("selected authoritative leaf page has no cells")
    content[page_offset + 8 : page_offset + 10] = (1).to_bytes(2, "big")
    path.write_bytes(content)

    connection = sqlite3.connect(path)
    messages = quick_check(connection)
    if messages == ["ok"]:
        raise RuntimeError("authoritative corruption was not detected")
    assert connection.execute("SELECT count(*) FROM facts").fetchone()[0] == 200
    connection.close()


def create_online_backup() -> None:
    source_path = ROOT / "online-backup-source.sqlite3"
    source = sqlite3.connect(source_path)
    configure(source)
    source.execute(
        "CREATE TABLE facts (id INTEGER PRIMARY KEY, body TEXT NOT NULL) STRICT"
    )
    source.execute("CREATE TABLE evidence_rows (value TEXT NOT NULL) STRICT")
    source.execute("INSERT INTO evidence_rows VALUES ('s11-runtime-baseline')")
    source.executemany(
        "INSERT INTO facts VALUES (?, ?)",
        (
            (number, f"backup-row-{number:03d}")
            for number in range(1, 65)
        ),
    )
    source.commit()

    copy = sqlite3.connect(ROOT / "online-backup-copy.sqlite3")
    source.backup(copy, pages=1)
    copy.close()

    mismatch = sqlite3.connect(ROOT / "online-backup-digest-mismatch.sqlite3")
    source.backup(mismatch, pages=1)
    mismatch.execute("INSERT INTO facts VALUES (999, 'digest mismatch')")
    mismatch.commit()
    mismatch.close()
    source.close()


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(64 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def write_manifest() -> None:
    artifacts = {
        name: {
            "bytes": (ROOT / name).stat().st_size,
            "sha256": sha256(ROOT / name),
        }
        for name in FIXTURE_FILES
    }
    artifact_manifest = {
        "schema": "tracedecay.storage-runtime-evidence.s11.v1",
        "generator": {
            "script": "generate.py",
            "python": sys.version.split()[0],
            "sqlite": sqlite3.sqlite_version,
            "logical_state_deterministic": True,
            "byte_reproducibility": (
                "Byte-for-byte on the recorded Python/SQLite versions; generated WAL "
                "salts and checksums are normalized."
            ),
        },
        "binding": BINDING,
        "artifacts": artifacts,
        "expectations": {
            "wal_pressure_rows": 129,
            "crash_receipt_id": "receipt.s11.crash",
            "crash_evidence_id": "evidence.s11.crash",
            "fts_search_term": "needle",
            "authoritative_rows": 200,
            "backup_rows": 64,
            "replacement_incarnation": 8,
            "replacement_authority_epoch": 20,
        },
    }
    (ROOT / ARTIFACT_MANIFEST).write_text(
        json.dumps(artifact_manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    runtime_manifest = {
        "schema_version": 1,
        "project_root": ".",
        "profile_root": ".",
        "fts_queries": {
            "graph": "needle",
            "session": "needle",
        },
        "s11": {
            "database": "online-backup-source.sqlite3",
            "binding": BINDING,
            "evidence_tables": ["evidence_rows", "facts"],
        },
    }
    (ROOT / RUNTIME_MANIFEST).write_text(
        json.dumps(runtime_manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def generate() -> None:
    ROOT.mkdir(parents=True, exist_ok=True)
    for name in (
        *FIXTURE_FILES,
        ARTIFACT_MANIFEST,
        RUNTIME_MANIFEST,
    ):
        (ROOT / name).unlink(missing_ok=True)
    create_wal_pressure()
    create_crash_after_commit()
    create_fts_stale()
    create_authoritative_corruption()
    create_online_backup()
    write_manifest()


def main() -> None:
    global ROOT
    parser = argparse.ArgumentParser()
    parser.add_argument("--crash-child", type=Path)
    parser.add_argument("--output", type=Path, default=ROOT)
    arguments = parser.parse_args()
    if arguments.crash_child is not None:
        crash_child(arguments.crash_child)
    else:
        ROOT = arguments.output.resolve()
        generate()


if __name__ == "__main__":
    main()
