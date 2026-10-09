"""Opt-in real Turso MVCC experiment; ROWID model is not a production adapter.

The separate exact-DDL compatibility probe does not weaken or migrate the
present TraceDecay WITHOUT ROWID schema to make this experiment pass.
"""
from __future__ import annotations

import hashlib
import json
import platform
import subprocess
import sys
import tempfile
from pathlib import Path

if sys.version_info < (3, 10):
    raise RuntimeError("pyturso 0.8.0 needs CPython >= 3.10; set TURSO_PYTHON")
if platform.system() != "Darwin" or platform.machine() != "arm64":
    raise RuntimeError("the pinned prototype wheel targets macOS arm64")

import turso
from turso import _turso

WHEEL_SHA256 = "62bf87c6966f6b1d9c8b71ae060779758cbc55ee649567cef4399a15c7a4c65b"


def rows(connection, sql, parameters=()):
    # Drain SELECTs before COMMIT so half-stepped cursors cannot affect results.
    return connection.execute(sql, parameters).fetchall()


def connect(path, mvcc=True):
    connection = turso.connect(str(path), isolation_level=None, experimental_features="without_rowid")
    mode = "mvcc" if mvcc else "wal"
    assert rows(connection, f"PRAGMA journal_mode='{mode}'") == [(mode,)]
    return connection


def begin(connection):
    rows(connection, "BEGIN CONCURRENT")


def digest(operation):
    return hashlib.sha256(operation.encode()).hexdigest()


def conflict_details(error):
    if type(error).__name__ == "BusySnapshot" or str(error).lower() in {
        "write-write conflict", "database is locked", "database is busy", "busy snapshot",
    }:
        return {"class": type(error).__name__, "message": str(error)}
    raise error


def independent_rows(path):
    admin = connect(path)
    rows(admin, "CREATE TABLE effects(id INTEGER PRIMARY KEY, value INTEGER NOT NULL)")
    rows(admin, "INSERT INTO effects VALUES (1,0),(2,0)")
    left, right = connect(path), connect(path)
    try:
        begin(left)
        begin(right)
        rows(left, "UPDATE effects SET value=1 WHERE id=1")
        rows(right, "UPDATE effects SET value=1 WHERE id=2")
        assert left.in_transaction and right.in_transaction
        assert rows(admin, "SELECT * FROM effects ORDER BY id") == [(1, 0), (2, 0)]
        # Both real write transactions mutated BEFORE either commits. Calls
        # are coordinated sequentially, but database transaction lifetimes
        # overlap on separate connections to the very same physical file.
        left.commit()
        assert rows(right, "SELECT value FROM effects WHERE id=1") == [(0,)]
        right.commit()
        assert rows(admin, "SELECT * FROM effects ORDER BY id") == [(1, 1), (2, 1)]
        return {"overlapping_begun_and_mutated_transactions": 2,
                "both_independent_row_commits_succeeded": True,
                "snapshot_survived_peer_commit": True}
    finally:
        left.close()
        right.close()
        admin.close()


def receipt(connection, operation):
    return rows(connection, "SELECT digest,commit_sequence FROM receipts WHERE operation=?", (operation,))


def checkpoint_cas(connection, previous):
    cursor = connection.execute(
        "UPDATE checkpoint SET commit_sequence=? WHERE id=1 AND commit_sequence=?",
        (previous + 1, previous))
    cursor.fetchall()
    if cursor.rowcount != 1:
        raise RuntimeError(f"checkpoint compare-and-swap rejected stale sequence {previous}")


def record_receipt(connection, operation, request_digest, sequence):
    rows(connection, "INSERT INTO receipts VALUES (?,?,?)", (operation, request_digest, sequence))


def publish(connection, operation, request_digest, effect_id):
    """Reduced admission path: replay/conflict precede all domain mutations."""
    begin(connection)
    try:
        existing = receipt(connection, operation)
        if existing:
            original_digest, sequence = existing[0]
            connection.rollback()
            return ("exact_replay" if request_digest == original_digest else "idempotency_conflict", sequence)
        previous = rows(connection, "SELECT commit_sequence FROM checkpoint WHERE id=1")[0][0]
        rows(connection, "UPDATE effects SET value=value+1 WHERE id=?", (effect_id,))
        checkpoint_cas(connection, previous)
        record_receipt(connection, operation, request_digest, previous + 1)
        connection.commit()
        return ("committed", previous + 1)
    except Exception:
        connection.rollback()
        raise


def singleton_checkpoint(path):
    admin = connect(path)
    rows(admin, "CREATE TABLE effects(id INTEGER PRIMARY KEY,value INTEGER NOT NULL)")
    rows(admin, "CREATE TABLE checkpoint(id INTEGER PRIMARY KEY,commit_sequence INTEGER NOT NULL)")
    rows(admin, "CREATE TABLE receipts(operation TEXT PRIMARY KEY,digest TEXT NOT NULL,commit_sequence INTEGER NOT NULL UNIQUE)")
    rows(admin, "INSERT INTO effects VALUES (1,0),(2,0)")
    rows(admin, "INSERT INTO checkpoint VALUES (1,0)")
    left, right = connect(path), connect(path)
    conflict, phase = None, None
    try:
        begin(left)
        begin(right)
        assert rows(left, "SELECT commit_sequence FROM checkpoint") == [(0,)]
        assert rows(right, "SELECT commit_sequence FROM checkpoint") == [(0,)]
        rows(left, "UPDATE effects SET value=1 WHERE id=1")
        rows(right, "UPDATE effects SET value=1 WHERE id=2")
        assert left.in_transaction and right.in_transaction
        checkpoint_cas(left, 0)
        record_receipt(left, "operation.left", digest("operation.left"), 1)
        left.commit()
        try:
            phase = "checkpoint_update"
            checkpoint_cas(right, 0)
            phase = "receipt_insert"
            record_receipt(right, "operation.right", digest("operation.right"), 1)
            phase = "commit"
            right.commit()
        except Exception as error:
            if isinstance(error, RuntimeError) and str(error).startswith("checkpoint compare-and-swap rejected"):
                conflict = {"class": "compare_and_swap", "message": str(error)}
            else:
                conflict = conflict_details(error)
            right.rollback()
        assert conflict is not None, "shared checkpoint must reject stale concurrent transaction"
        assert phase == "checkpoint_update", phase
        assert rows(admin, "SELECT * FROM effects ORDER BY id") == [(1, 1), (2, 0)]
        assert receipt(admin, "operation.right") == [], "aborted mutation leaked receipt"
        assert rows(admin, "SELECT commit_sequence FROM checkpoint") == [(1,)]
        assert publish(right, "operation.right", digest("operation.right"), 2) == ("committed", 2)
        assert rows(admin, "SELECT commit_sequence FROM receipts ORDER BY commit_sequence") == [(1,), (2,)]
        assert publish(right, "operation.right", digest("operation.right"), 2) == ("exact_replay", 2)
        assert publish(right, "operation.right", digest("operation.different"), 1) == ("idempotency_conflict", 2)
        assert rows(admin, "SELECT * FROM effects ORDER BY id") == [(1, 1), (2, 1)]
        assert rows(admin, "SELECT commit_sequence FROM checkpoint") == [(2,)]
        assert rows(admin, "SELECT COUNT(*) FROM receipts") == [(2,)]
        return {"overlapping_begun_and_mutated_transactions": 2, "conflict_phase": phase,
                "conflict": conflict, "rollback_discarded_effect_and_no_receipt_published": True,
                "fresh_transaction_retry_sequence": 2, "contiguous_receipt_sequences": [1, 2],
                "exact_replay_and_conflict_leave_sequence_unchanged": True}
    finally:
        left.close()
        right.close()
        admin.close()


def exact_ledger_compatibility(path, mvcc):
    connection = connect(path, mvcc)
    ddl = Path(__file__).with_name("turso_ledger_schema.sql").read_text()
    phase = "exact_schema_install"
    try:
        connection.executescript(ddl)
        phase = "checkpoint_insert"
        rows(connection, """INSERT INTO td_runtime_writer_checkpoint_v1 VALUES (
            'shard',1,1,1,'{}','{}','{}','operation','full',1)""")
        phase = "checkpoint_update"
        cursor = connection.execute("""UPDATE td_runtime_writer_checkpoint_v1
            SET commit_sequence=2 WHERE shard_json='shard' AND incarnation=1
            AND authority_epoch=1 AND commit_sequence=1""")
        cursor.fetchall()
        assert cursor.rowcount == 1
        assert rows(connection, "SELECT commit_sequence FROM td_runtime_writer_checkpoint_v1") == [(2,)]
        return {"journal_mode": "mvcc" if mvcc else "wal", "compatible": True}
    except turso.DatabaseError as error:
        assert str(error) == "Parse error: CREATE INDEX on WITHOUT ROWID tables is not supported", str(error)
        return {"journal_mode": "mvcc" if mvcc else "wal", "compatible": False,
                "failed_phase": phase, "class": type(error).__name__, "message": str(error)}
    finally:
        connection.close()


def verify_reopen(path):
    connection = connect(path)
    try:
        assert rows(connection, "SELECT * FROM effects ORDER BY id") == [(1, 1), (2, 1)]
        assert rows(connection, "SELECT commit_sequence FROM checkpoint") == [(2,)]
        assert rows(connection, "SELECT commit_sequence FROM receipts ORDER BY commit_sequence") == [(1,), (2,)]
        assert rows(connection, "SELECT operation,digest,commit_sequence FROM receipts ORDER BY commit_sequence") == [
            ("operation.left", digest("operation.left"), 1),
            ("operation.right", digest("operation.right"), 2),
        ]
        assert publish(connection, "operation.right", digest("operation.right"), 2) == ("exact_replay", 2)
        assert publish(connection, "operation.right", digest("operation.different"), 1) == ("idempotency_conflict", 2)
        assert rows(connection, "SELECT * FROM effects ORDER BY id") == [(1, 1), (2, 1)]
        assert rows(connection, "SELECT commit_sequence FROM checkpoint") == [(2,)]
        assert rows(connection, "SELECT COUNT(*) FROM receipts") == [(2,)]
        integrity = rows(connection, "PRAGMA integrity_check")
        assert integrity == [("ok",)], integrity
        print(json.dumps({"reopened_in_new_process": True, "integrity_check": integrity,
                          "checkpoint": 2, "receipt_sequences": [1, 2],
                          "persisted_idempotency_replay_and_conflict_verified": True}))
    finally:
        connection.close()


def main():
    assert _turso.__version__ == "0.8.0", _turso.__version__
    if len(sys.argv) == 3 and sys.argv[1] == "--verify-reopen":
        verify_reopen(Path(sys.argv[2]))
        return
    with tempfile.TemporaryDirectory(prefix="tracedecay-turso-prototype-") as directory:
        root = Path(directory)
        result = {"engine": "actual Turso Rust native Python extension", "engine_version": _turso.__version__,
                  "wheel_sha256": WHEEL_SHA256, "sqlite_dialect_version": turso.sqlite_version,
                  "python": platform.python_version(), "os": platform.platform(),
                  "transaction_mode": "BEGIN CONCURRENT", "journal_mode": "mvcc"}
        result["independent_rows"] = independent_rows(root / "independent.db")
        checkpoint_path = root / "checkpoint.db"
        result["singleton_checkpoint"] = singleton_checkpoint(checkpoint_path)
        child = subprocess.run([sys.executable, __file__, "--verify-reopen", str(checkpoint_path)],
                               capture_output=True, text=True, timeout=20, check=True)
        result["recovery"] = json.loads(child.stdout)
        result["exact_ledger_compatibility"] = [exact_ledger_compatibility(root / f"compat-{mode}.db", mode == "mvcc")
                                              for mode in ("wal", "mvcc")]
        print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
