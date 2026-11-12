"""A held read lease against compaction, per sketch.rs. Graph lane."""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from model import Model

m = Model()
s1 = m.write_segment({0: [("target", "caller.a")], 1: [("target", "caller.b")], 2: [("other", "x")]})
s2 = m.write_segment({0: [("target", "caller.c")], 1: [("other", "y")]})
snap = m.publish_slot("/wt", {s1: [0, 1], s2: [0, 1]})

lease = m.open_current("/wt")
print("lease holds snapshot", lease["snapshot"], "over segments", [s for s, _ in lease["segments"]])
print("callers(target), hop 1:", m.callers(lease, "target"))

# Next reconcile: a dead document in s1 makes it a compaction candidate. The
# hold names the snapshot, and the snapshot row is remapped, as the design says.
out = m.merge([s1, s2])
deleted = m.collect()
print("merge wrote segment", out, "; collect deleted", deleted)
print(
    "snapshot row now names",
    m.cat.execute("SELECT segment FROM ref WHERE snapshot=?", (snap,)).fetchall(),
    "; hold still present:",
    bool(m.cat.execute("SELECT 1 FROM hold WHERE id=?", (lease["hold"],)).fetchone()),
)

# The same lease continues. No error path exists: zero rows is a normal answer.
result = m.callers(lease, "target")
print("callers(target), same lease, after reconcile:", result)
print("WRONG: empty success under a live hold" if not result else "ok")

# Control: a lease opened after the merge sees the remapped data, so the data was not lost.
print("control, fresh lease:", m.callers(m.open_current("/wt"), "target"))
