"""Requirement 7: identical caller bytes can resolve differently in two snapshots.

sketch.rs:856  FileRow: integer primary key packing (segment, file_ordinal),
               "Unique index on the reuse key."
sketch.rs:867  DecisionRow: "Same bytes with a different decision digest are a
               different row." Keyed (segment, file ordinal).
sketch.rs:891  EdgeRow: primary key (segment, dst_key, kind, file_ordinal, ordinal).
               No decision digest.
sketch.rs:633  SegmentRef: one alive set per (snapshot, segment), over file ordinals.
design.md:75   "it references that segment with the document alive"

So a snapshot that reuses caller.rs's file row in segment 1 has that ordinal alive,
which also makes segment 1's edges for that ordinal visible.
"""
import sqlite3

db = sqlite3.connect(":memory:")
db.executescript(
    """
    CREATE TABLE file(id INTEGER PRIMARY KEY, segment INT, ordinal INT, key TEXT);
    CREATE UNIQUE INDEX file_reuse ON file(key);
    CREATE TABLE decision(segment INT, ordinal INT, digest TEXT, PRIMARY KEY(segment, ordinal, digest));
    CREATE TABLE edge(segment INT, dst_key TEXT, kind TEXT, file INT, ordinal INT, src TEXT,
                      PRIMARY KEY(segment, dst_key, kind, file, ordinal)) WITHOUT ROWID;
    """
)
CALLER = "caller.rs|sha256:same-bytes|receipt|extractor"
# Branch A, segment 1: caller.rs's call to helper() resolves into a.rs.
db.execute("INSERT INTO file(segment, ordinal, key) VALUES(1, 0, ?)", (CALLER,))
db.execute("INSERT INTO decision VALUES(1, 0, 'decision-A')")
db.execute("INSERT INTO edge VALUES(1, 'a.rs#helper', 'calls', 0, 0, 'caller.rs#main')")
snapshot_a = {1: {0}}


def callers(view, key):
    out = []
    for seg, alive in view.items():
        for src, file in db.execute("SELECT src, file FROM edge WHERE segment=? AND dst_key=?", (seg, key)):
            if file in alive:
                out.append(src)
    return out


# Branch B: caller.rs has the same bytes, but b.rs now provides helper, so the
# resolver decides differently. The reuse lookup hits on segment 1.
print("Route 1: new segment holds a full document for caller.rs, as an alive set needs")
try:
    db.execute("INSERT INTO file(segment, ordinal, key) VALUES(2, 0, ?)", (CALLER,))
    print("  inserted")
except sqlite3.IntegrityError as error:
    print("  IntegrityError:", error)

print("Route 2: reuse the file row in segment 1, add only the decision and edges in segment 2")
db.execute("INSERT INTO decision VALUES(2, 0, 'decision-B')")
db.execute("INSERT INTO edge VALUES(2, 'b.rs#helper', 'calls', 0, 0, 'caller.rs#main')")
snapshot_b = {1: {0}, 2: {0}}
print("  snapshot B, callers(a.rs#helper):", callers(snapshot_b, "a.rs#helper"), " required: []")
print("  snapshot B, callers(b.rs#helper):", callers(snapshot_b, "b.rs#helper"), " required: ['caller.rs#main']")

print("Route 3: alive bit off in segment 1, to hide the stale edge")
snapshot_b = {2: {0}}
symbols = db.execute(
    "SELECT count(*) FROM file WHERE segment IN (%s) AND ordinal=0" % ",".join(map(str, snapshot_b))
).fetchone()[0]
print("  snapshot B, callers(a.rs#helper):", callers(snapshot_b, "a.rs#helper"))
print("  snapshot B now sees file rows for caller.rs:", symbols, " required: 1 (symbols and payload live with the file row)")

print("Union merge of segments 1 and 2, needed by a cap merge when A and B both stay rooted:")
try:
    db.execute("INSERT INTO file(segment, ordinal, key) VALUES(3, 0, ?)", (CALLER,))
    db.execute("INSERT INTO file(segment, ordinal, key) VALUES(3, 1, ?)", (CALLER,))
except sqlite3.IntegrityError as error:
    print("  IntegrityError:", error)
