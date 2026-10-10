"""Opt-in real Turso MVCC experiment; ROWID model is not a production adapter.

The separate exact-DDL compatibility probe does not weaken or migrate the
present TraceDecay WITHOUT ROWID schema to make this experiment pass.
"""
from __future__ import annotations

import gc
import hashlib
import json
import platform
import sqlite3
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

# Explicit SQL fixtures mirror the four production tables, with no SQL parser
# or relaxed constraints. JSON fields exercise storage, not Rust domain codecs.
LEDGERS = {
    "checkpoint": ("td_runtime_writer_checkpoint_v1", {
        "shard_json": '"prototype.shard"', "incarnation": 1, "authority_epoch": 1,
        "commit_sequence": 1, "watermark_json": "{}", "transaction_scope_json": "{}",
        "original_receipt_json": "{}", "operation_id": "operation.1",
        "durability_json": '"full"', "committed_at_micros": 1,
    }, ("shard_json", "incarnation"),
        {"incarnation": 0, "authority_epoch": 0, "commit_sequence": 0}),
    "idempotency": ("td_runtime_writer_idempotency_v2", {
        "shard_json": '"prototype.shard"', "incarnation": 1, "authority_epoch": 1,
        "idempotency_key": "key.1", "request_digest": "digest.1",
        "operation_id": "operation.1", "transaction_id": "transaction.1",
        "commit_sequence": 1, "durability_json": '"full"', "priority_json": '"foreground"',
        "opened_at_micros": 1, "committed_at_micros": 1,
    }, ("shard_json", "incarnation", "authority_epoch", "idempotency_key"),
        {"incarnation": 0, "authority_epoch": 0, "commit_sequence": 0}),
    "outbox": ("td_runtime_writer_outbox_v1", {
        "source_shard_json": '"prototype.shard"', "source_incarnation": 1,
        "source_authority_epoch": 1, "effect_id": "effect.1", "ordering_key": "ordered",
        "source_sequence": 1, "state": "pending", "entry_json": "{}",
        "source_receipt_json": "{}", "transaction_scope_json": "{}",
        "operation_id": "operation.1", "durability_json": '"full"', "updated_at_micros": 1,
    }, ("source_shard_json", "source_incarnation", "source_authority_epoch", "effect_id"),
        {"source_incarnation": 0, "source_authority_epoch": 0,
         "source_sequence": -1, "state": "invalid"}),
    "inbox": ("td_runtime_writer_inbox_v1", {
        "target_shard_json": '"prototype.shard"', "target_incarnation": 1,
        "target_authority_epoch": 1, "effect_id": "effect.1", "ordering_key": "ordered",
        "source_sequence": 1, "target_sequence": 1, "identity_json": "{}",
        "receipt_json": "{}", "committed_at_micros": 1,
    }, ("target_shard_json", "target_incarnation", "target_authority_epoch", "effect_id"),
        {"target_incarnation": 0, "target_authority_epoch": 0,
         "source_sequence": -1, "target_sequence": 0}),
}


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


def ledger_connection(path, engine):
    if engine == "sqlite":
        connection = sqlite3.connect(str(path), isolation_level=None)
        assert rows(connection, "PRAGMA journal_mode='wal'") == [("wal",)]
        return connection
    assert engine == "turso"
    return connect(path)


def ledger_begin(connection, engine):
    rows(connection, "BEGIN IMMEDIATE" if engine == "sqlite" else "BEGIN CONCURRENT")


def ledger_insert(connection, name, values, suffix="", ignore=False):
    table = LEDGERS[name][0]
    columns = tuple(values)
    cursor = connection.execute(
        f"INSERT {'OR IGNORE ' if ignore else ''}INTO {table} ({','.join(columns)}) "
        f"VALUES ({','.join('?' for _ in columns)}) {suffix}", tuple(values.values()))
    cursor.fetchall()
    return cursor.rowcount


def ledger_snapshot(connection):
    return {name: rows(connection, f"SELECT * FROM {table} ORDER BY {','.join(keys)}")
            for name, (table, _, keys, _) in LEDGERS.items()}


def ledger_values(name, sequence):
    values = dict(LEDGERS[name][1])
    for column in ("commit_sequence", "source_sequence", "target_sequence",
                   "committed_at_micros", "updated_at_micros"):
        if column in values:
            values[column] = sequence
    for column, prefix in (("operation_id", "operation"), ("idempotency_key", "key"),
                           ("transaction_id", "transaction"), ("effect_id", "effect"),
                           ("request_digest", "digest")):
        if column in values:
            values[column] = f"{prefix}.{sequence}"
    return values


def ledger_checkpoint_cas(connection, previous):
    cursor = connection.execute("""UPDATE td_runtime_writer_checkpoint_v1
        SET authority_epoch=?3, commit_sequence=?4, watermark_json=?5,
            transaction_scope_json=?6, original_receipt_json=?7, operation_id=?8,
            durability_json=?9, committed_at_micros=?10
        WHERE shard_json=?1 AND incarnation=?2
          AND authority_epoch=?11 AND commit_sequence=?12""",
        tuple(ledger_values("checkpoint", previous + 1).values()) + (1, previous))
    cursor.fetchall()
    return cursor.rowcount


def ledger_stage(connection, sequence):
    if sequence == 1:
        assert ledger_insert(connection, "checkpoint", ledger_values("checkpoint", 1), ignore=True) == 1
    else:
        assert ledger_checkpoint_cas(connection, sequence - 1) == 1
    for name in ("idempotency", "outbox", "inbox"):
        assert ledger_insert(connection, name, ledger_values(name, sequence), ignore=True) == 1


def ledger_publish(connection, engine, key, request_digest):
    # Representative storage admission only: production Rust receipt binding,
    # operation-policy validation and JSON codecs are not implemented here.
    ledger_begin(connection, engine)
    try:
        existing = rows(connection, """SELECT request_digest,commit_sequence
            FROM td_runtime_writer_idempotency_v2 WHERE shard_json=? AND incarnation=1
            AND authority_epoch=1 AND idempotency_key=?""", ('"prototype.shard"', key))
        if existing:
            connection.rollback()
            return ("exact_replay" if existing[0][0] == request_digest else "idempotency_conflict", existing[0][1])
        previous = rows(connection, "SELECT commit_sequence FROM td_runtime_writer_checkpoint_v1")
        sequence = previous[0][0] + 1 if previous else 1
        assert key == f"key.{sequence}" and request_digest == f"digest.{sequence}"
        ledger_stage(connection, sequence)
        connection.commit()
        return ("committed", sequence)
    except Exception:
        connection.rollback()
        raise


def ledger_constraints(connection, engine):
    observed = []
    baseline = ledger_snapshot(connection)
    for name, (table, valid, keys, checks) in LEDGERS.items():
        info = rows(connection, f"PRAGMA table_info({table})")
        assert {column[1] for column in info if column[5]} == set(keys)
        assert all(column[3] == 1 for column in info), (table, info)
        candidates = [("null_primary_key", column, {**valid, column: None}, "not null")
                      for column in keys]
        candidates += [("check", column, {**valid, column: value}, "check")
                       for column, value in checks.items()]
        candidates.append(("duplicate_primary_key", "composite", valid, "unique"))
        if name in ("outbox", "inbox"):
            incarnation = "source_incarnation" if name == "outbox" else "target_incarnation"
            candidates.append(("unique_effect_index", incarnation, {**valid, incarnation: 2}, "unique"))
        for kind, column, candidate, message_token in candidates:
            ledger_begin(connection, engine)
            try:
                if kind in {"null_primary_key", "check"}:
                    # Isolate this constraint from the seeded PK/unique keys.
                    # Rolling back restores the seed in the full snapshot.
                    rows(connection, f"DELETE FROM {table}")
                ledger_insert(connection, name, candidate)
            except Exception as error:
                assert type(error).__name__ in {"IntegrityError", "DatabaseError"}, type(error).__name__
                assert message_token in str(error).lower(), (kind, str(error))
                observed.append({"table": name, "kind": kind, "column": column,
                                 "class": type(error).__name__, "message": str(error)})
            else:
                raise AssertionError(f"{engine} accepted forbidden {table}/{kind}/{column}")
            finally:
                connection.rollback()
            assert ledger_snapshot(connection) == baseline
    assert len(observed) == 34
    return observed


def verify_ledger_reopen(path, engine):
    connection = ledger_connection(path, engine)
    try:
        assert rows(connection, "SELECT commit_sequence FROM td_runtime_writer_checkpoint_v1") == [(2,)]
        assert rows(connection, "SELECT idempotency_key,request_digest,commit_sequence FROM td_runtime_writer_idempotency_v2") == [("key.2", "digest.2", 2)]
        assert rows(connection, "SELECT effect_id,state,entry_json FROM td_runtime_writer_outbox_v1") == [("effect.2", "acknowledged", '{"updated":true}')]
        assert rows(connection, "SELECT effect_id,receipt_json,target_sequence FROM td_runtime_writer_inbox_v1") == [("effect.2", '{"upsert":true}', 2)]
        snapshot = ledger_snapshot(connection)
        assert ledger_publish(connection, engine, "key.2", "digest.2") == ("exact_replay", 2)
        assert ledger_publish(connection, engine, "key.2", "digest.changed") == ("idempotency_conflict", 2)
        assert ledger_snapshot(connection) == snapshot
        assert rows(connection, "PRAGMA integrity_check") == [("ok",)]
        return {"reopened_in_new_process": True, "integrity_check": "ok",
                "checkpoint": 2, "remaining_receipt_sequences": [2],
                "persisted_update_upsert_delete_and_admission_verified": True}
    finally:
        connection.close()


def adapted_ledger_differential(path, engine):
    original = Path(__file__).with_name("turso_ledger_schema.sql").read_text().partition("\n")[2]
    adapted = Path(__file__).with_name("ledger_rowid_schema.sql").read_text().partition("\n")[2]
    assert original.count(") WITHOUT ROWID;") == 4
    assert adapted == original.replace(") WITHOUT ROWID;", ");"), "adaptation changed constraints or indexes"
    connection = ledger_connection(path, engine)
    try:
        ddl = original if engine == "sqlite" else adapted
        connection.executescript(ddl)
        assert ledger_publish(connection, engine, "key.1", "digest.1") == ("committed", 1)
        constraints = ledger_constraints(connection, engine)
        baseline = ledger_snapshot(connection)
        ledger_begin(connection, engine)
        ledger_stage(connection, 2)
        assert all(len(values) == (1 if name == "checkpoint" else 2)
                   for name, values in ledger_snapshot(connection).items())
        connection.rollback()
        assert ledger_snapshot(connection) == baseline, "rollback leaked a ledger row or sequence"
        ledger_begin(connection, engine)
        assert ledger_checkpoint_cas(connection, 0) == 0
        connection.rollback()
        assert ledger_snapshot(connection) == baseline
        assert ledger_publish(connection, engine, "key.2", "digest.2") == ("committed", 2)
        assert rows(connection, "SELECT commit_sequence FROM td_runtime_writer_idempotency_v2 ORDER BY commit_sequence") == [(1,), (2,)]
        snapshot = ledger_snapshot(connection)
        assert ledger_publish(connection, engine, "key.2", "digest.2") == ("exact_replay", 2)
        assert ledger_publish(connection, engine, "key.2", "digest.changed") == ("idempotency_conflict", 2)
        assert ledger_snapshot(connection) == snapshot
        ledger_begin(connection, engine)
        updated = connection.execute("""UPDATE td_runtime_writer_outbox_v1
            SET state=?5,entry_json=?6,updated_at_micros=?7
            WHERE source_shard_json=?1 AND source_incarnation=?2
            AND source_authority_epoch=?3 AND effect_id=?4 AND state=?8 AND entry_json=?9""",
            ('"prototype.shard"', 1, 1, "effect.2", "dispatched", '{"updated":true}', 3, "pending", "{}"))
        updated.fetchall()
        assert updated.rowcount == 1
        updated.close()
        del updated
        for name, changed, column in (
            ("outbox", {"state": "acknowledged"}, "state"),
            ("inbox", {"receipt_json": '{"upsert":true}'}, "receipt_json"),
        ):
            table, _, keys, _ = LEDGERS[name]
            values = ledger_values(name, 2)
            if name == "outbox":
                values["entry_json"] = '{"updated":true}'
            values.update(changed)
            assert ledger_insert(connection, name, values,
                f"ON CONFLICT ({','.join(keys)}) DO UPDATE SET {column}=excluded.{column}") == 1
        for name, predicate in (("idempotency", "commit_sequence"),
                                ("outbox", "source_sequence"), ("inbox", "target_sequence")):
            deleted = connection.execute(f"DELETE FROM {LEDGERS[name][0]} WHERE {predicate}=?", (1,))
            deleted.fetchall()
            assert deleted.rowcount == 1
            deleted.close()
            del deleted
        connection.commit()
    finally:
        connection.close()
    # In this constraint-heavy probe, close alone left the native file locked.
    # Drop Python references and collect before process handoff; the exact
    # retention mechanism has not been established.
    del connection
    gc.collect()
    child = subprocess.run([sys.executable, __file__, "--verify-ledger-reopen", engine, str(path)],
                           capture_output=True, text=True, timeout=20)
    assert child.returncode == 0, f"{engine} ledger reopen failed:\n{child.stdout}\n{child.stderr}"
    return {"engine": engine, "schema": "original_without_rowid" if engine == "sqlite" else "adapted_rowid",
            "engine_version": sqlite3.sqlite_version if engine == "sqlite" else _turso.__version__,
            "ddl_sha256": hashlib.sha256(ddl.encode()).hexdigest(),
            "constraint_rejections": constraints, "explicit_not_null_primary_key_components": 14,
            "null_and_check_cases_isolated_from_seed_key_collisions": True,
            "all_columns_explicit_not_null_verified": True,
            "rollback_discarded_mutations_in_all_four_ledgers": True,
            "stale_checkpoint_cas_affected_zero_rows": True, "retry_receipt_sequences_before_prune": [1, 2],
            "recovery": json.loads(child.stdout)}


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
    if len(sys.argv) == 4 and sys.argv[1] == "--verify-ledger-reopen":
        print(json.dumps(verify_ledger_reopen(Path(sys.argv[3]), sys.argv[2])))
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
        result["adapted_ledger_differential"] = [adapted_ledger_differential(root / f"ledger-{engine}.db", engine)
                                                for engine in ("sqlite", "turso")]
        sqlite_cases, turso_cases = [item["constraint_rejections"] for item in result["adapted_ledger_differential"]]
        assert [(item["table"], item["kind"], item["column"]) for item in sqlite_cases] == [
            (item["table"], item["kind"], item["column"]) for item in turso_cases]
        result["adapted_ledger_constraint_outcomes_match_sqlite"] = True
        print(json.dumps(result, sort_keys=True))


if __name__ == "__main__":
    main()
