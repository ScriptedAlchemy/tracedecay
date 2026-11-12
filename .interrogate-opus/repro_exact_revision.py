"""An exact-revision read per design.md section 1 and sketch.rs.

Roots: slots, holds, builds. "A hold, an open read lease or unexpired cursor, or
an admitted exact-revision request while it runs." open() on a miss "schedules a
mint and returns Warming". "Every root release and publication wakes reconcile"
(requirement 24 in the trace).
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from model import Model, Warming

m = Model()
base = m.write_segment({0: [("target", "caller.a")]})
m.publish_slot("/wt", {base: [0]})

pending = []
ATTEMPTS = 6
for attempt in range(1, ATTEMPTS + 1):
    try:
        lease = m.open_revision("rev-A", pending.append)
        print(f"attempt {attempt}: lease, callers =", m.callers(lease, "target"))
        break
    except Warming:
        print(f"attempt {attempt}: Warming, mint scheduled")
    # The request has returned, so nothing admits it any more. The mint runs
    # under a build, publishes, and the build ends.
    rev = pending.pop()
    seg = m.write_segment({0: [("target", "caller.rev")]})
    snap = m.publish_unrooted({seg: [0]}, witness="w-" + rev, revision=rev)
    # Publication wakes reconcile, before any client can retry.
    deleted = m.collect()
    print(f"           mint published snapshot {snap}; woken reconcile deleted segments {deleted}")
else:
    print(f"RESULT: no answer after {ATTEMPTS} attempts, {ATTEMPTS} mints written and deleted")
