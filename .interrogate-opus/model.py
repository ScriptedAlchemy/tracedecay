"""Minimal executable model of sketch.rs's catalog and graph lane.

Every operation below follows the text of design.md and sketch.rs. Nothing is
added that the sketch does not specify. Real SQLite files, real transactions.

  open()         sketch.rs:232  "Root resolution and hold acquisition are atomic."
  SnapshotView   sketch.rs:691  "What a ReadLease carries. Resolved once at open."
  callers()      sketch.rs:394  "One range probe per visible segment, alive filter"
  swap_merged()  sketch.rs:744  replaces inputs in every referencing snapshot,
                                remaps, "unregistering the inputs"
  collect()      sketch.rs:717  live = referenced by a snapshot named by any slot,
                                hold or live build, plus segments owned by live builds
  delete_segments() sketch.rs:1031  DELETE by segment key range
"""
import json
import os
import sqlite3
import tempfile


class Warming(Exception):
    pass


class Model:
    def __init__(self, directory=None):
        self.dir = directory or tempfile.mkdtemp(prefix="model-")
        self.cat = sqlite3.connect(os.path.join(self.dir, "catalog.sqlite"), isolation_level=None)
        self.graph = sqlite3.connect(os.path.join(self.dir, "graph.sqlite"), isolation_level=None)
        self.cat.executescript(
            """
            CREATE TABLE slot(locator TEXT PRIMARY KEY, incarnation INT, repo TEXT, graph INT, seq INT);
            CREATE TABLE snapshot(id INTEGER PRIMARY KEY, witness TEXT, revision TEXT);
            CREATE TABLE segment(id INTEGER PRIMARY KEY, documents INT);
            CREATE TABLE ref(snapshot INT, segment INT, alive TEXT, PRIMARY KEY(snapshot, segment));
            CREATE TABLE hold(id INTEGER PRIMARY KEY, snapshot INT);
            CREATE TABLE build(id INTEGER PRIMARY KEY, input TEXT, owned TEXT);
            """
        )
        self.graph.executescript(
            """
            CREATE TABLE edge(segment INT, dst TEXT, file INT, ordinal INT, src TEXT,
                              PRIMARY KEY(segment, dst, file, ordinal)) WITHOUT ROWID;
            """
        )
        self.next_segment = 1

    # ---- writes -----------------------------------------------------------
    def write_segment(self, docs):
        """docs: {ordinal: [(dst, src), ...]}. Immutable after this."""
        sid = self.next_segment
        self.next_segment += 1
        self.graph.execute("BEGIN")
        for ordinal, edges in docs.items():
            for i, (dst, src) in enumerate(edges):
                self.graph.execute("INSERT INTO edge VALUES(?,?,?,?,?)", (sid, dst, ordinal, i, src))
        self.graph.execute("COMMIT")
        self.cat.execute("INSERT INTO segment VALUES(?,?)", (sid, len(docs)))
        return sid

    def publish_slot(self, locator, refs, witness="w", revision=None):
        """The CAS on the slot root. refs: {segment: [alive ordinals]}"""
        self.cat.execute("BEGIN IMMEDIATE")
        cur = self.cat.execute("INSERT INTO snapshot(witness, revision) VALUES(?,?)", (witness, revision))
        snap = cur.lastrowid
        for seg, alive in refs.items():
            self.cat.execute("INSERT INTO ref VALUES(?,?,?)", (snap, seg, json.dumps(sorted(alive))))
        row = self.cat.execute("SELECT seq FROM slot WHERE locator=?", (locator,)).fetchone()
        if row is None:
            self.cat.execute("INSERT INTO slot VALUES(?,?,?,?,?)", (locator, 1, "repo", snap, 1))
        else:
            self.cat.execute("UPDATE slot SET graph=?, seq=seq+1 WHERE locator=?", (snap, locator))
        self.cat.execute("COMMIT")
        return snap

    def publish_unrooted(self, refs, witness, revision):
        """An exact-revision mint. sketch.rs catalog::publish only CASes a slot
        root, and RootSet has only live_slots, holds and live_builds, so an
        exact-revision snapshot gets a row and no root."""
        self.cat.execute("BEGIN IMMEDIATE")
        cur = self.cat.execute("INSERT INTO snapshot(witness, revision) VALUES(?,?)", (witness, revision))
        snap = cur.lastrowid
        for seg, alive in refs.items():
            self.cat.execute("INSERT INTO ref VALUES(?,?,?)", (snap, seg, json.dumps(sorted(alive))))
        self.cat.execute("COMMIT")
        return snap

    # ---- reads ------------------------------------------------------------
    def open(self, snapshot):
        """Root resolution and hold are atomic. The view is resolved once."""
        self.cat.execute("BEGIN IMMEDIATE")
        hold = self.cat.execute("INSERT INTO hold(snapshot) VALUES(?)", (snapshot,)).lastrowid
        refs = self.cat.execute("SELECT segment, alive FROM ref WHERE snapshot=?", (snapshot,)).fetchall()
        self.cat.execute("COMMIT")
        return {"hold": hold, "snapshot": snapshot, "segments": [(s, set(json.loads(a))) for s, a in refs]}

    def open_current(self, locator):
        snap = self.cat.execute("SELECT graph FROM slot WHERE locator=?", (locator,)).fetchone()[0]
        return self.open(snap)

    def open_revision(self, revision, schedule_mint):
        """Selection::GitRevision: "An absent exact revision schedules a mint
        and returns Warming. No build on admission." sketch.rs:232"""
        row = self.cat.execute(
            "SELECT id FROM snapshot WHERE revision=? ORDER BY id DESC", (revision,)
        ).fetchone()
        if row is None:
            schedule_mint(revision)
            raise Warming(revision)
        return self.open(row[0])

    def close(self, lease):
        self.cat.execute("DELETE FROM hold WHERE id=?", (lease["hold"],))

    def callers(self, lease, dst):
        """One range probe per visible segment, alive filter."""
        out = []
        for seg, alive in lease["segments"]:
            rows = self.graph.execute(
                "SELECT src, file FROM edge WHERE segment=? AND dst=? ORDER BY file, ordinal", (seg, dst)
            ).fetchall()
            out += [src for src, file in rows if file in alive]
        return sorted(out)

    # ---- maintenance ------------------------------------------------------
    def merge(self, inputs):
        """graph_lane::merge then catalog::swap_merged, union of alive across
        every snapshot that references the inputs."""
        union = {}
        for seg in inputs:
            union[seg] = set()
            for (alive,) in self.cat.execute("SELECT alive FROM ref WHERE segment=?", (seg,)):
                union[seg] |= set(json.loads(alive))
        out = self.next_segment
        self.next_segment += 1
        remap = {}
        new_ord = 0
        self.graph.execute("BEGIN")
        for seg in inputs:
            for old in sorted(union[seg]):
                remap[(seg, old)] = new_ord
                for dst, ordinal, src in self.graph.execute(
                    "SELECT dst, ordinal, src FROM edge WHERE segment=? AND file=?", (seg, old)
                ).fetchall():
                    self.graph.execute("INSERT INTO edge VALUES(?,?,?,?,?)", (out, dst, new_ord, ordinal, src))
                new_ord += 1
        self.graph.execute("COMMIT")
        self.cat.execute("BEGIN IMMEDIATE")
        self.cat.execute("INSERT INTO segment VALUES(?,?)", (out, new_ord))
        snaps = {}
        for snap, seg, alive in self.cat.execute(
            "SELECT snapshot, segment, alive FROM ref WHERE segment IN (%s)" % ",".join(map(str, inputs))
        ).fetchall():
            snaps.setdefault(snap, set()).update(remap[(seg, o)] for o in json.loads(alive))
        for snap, alive in snaps.items():
            self.cat.execute(
                "DELETE FROM ref WHERE snapshot=? AND segment IN (%s)" % ",".join(map(str, inputs)), (snap,)
            )
            self.cat.execute("INSERT INTO ref VALUES(?,?,?)", (snap, out, json.dumps(sorted(alive))))
        for seg in inputs:
            self.cat.execute("DELETE FROM segment WHERE id=?", (seg,))  # "unregistering the inputs"
        self.cat.execute("COMMIT")
        return out

    def rooted_snapshots(self):
        roots = {r[0] for r in self.cat.execute("SELECT graph FROM slot WHERE graph IS NOT NULL")}
        roots |= {r[0] for r in self.cat.execute("SELECT snapshot FROM hold")}
        return roots

    def collect(self):
        """Predicate over catalog rows, then delete by segment range. Inputs of
        a merge are unregistered, so they are unnamed once swap_merged commits;
        lane rows with no registered segment are garbage."""
        roots = self.rooted_snapshots()
        for snap in [r[0] for r in self.cat.execute("SELECT id FROM snapshot")]:
            if snap not in roots:
                self.cat.execute("DELETE FROM ref WHERE snapshot=?", (snap,))
                self.cat.execute("DELETE FROM snapshot WHERE id=?", (snap,))
        named = {r[0] for r in self.cat.execute("SELECT DISTINCT segment FROM ref")}
        registered = {r[0] for r in self.cat.execute("SELECT id FROM segment")}
        in_lane = {r[0] for r in self.graph.execute("SELECT DISTINCT segment FROM edge")}
        deleted = sorted((in_lane | registered) - named)
        for seg in deleted:
            self.graph.execute("DELETE FROM edge WHERE segment=?", (seg,))
            self.cat.execute("DELETE FROM segment WHERE id=?", (seg,))
        return deleted
