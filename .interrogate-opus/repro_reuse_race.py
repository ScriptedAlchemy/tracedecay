"""design.md section 5: "A build registers its input segments before reading any it
means to reuse. Reconcile computes liveness in the deleting transaction with build
inputs in the predicate." The lookup is GraphLane::lookup_reuse in graph.sqlite
(sketch.rs:976), the registration is Catalog::hold_inputs in catalog.sqlite
(sketch.rs:713), and catalog::publish rejects only an empty alive set and an over-long
list (sketch.rs:702). Reuse reads nothing from the segment: it "references that
segment with the document alive rather than re-indexing it" (design.md:75).
"""
import json, os, sys
sys.path.insert(0, os.path.dirname(__file__))
from model import Model

m = Model()
s1 = m.write_segment({0: [("target", "caller.a")]})
m.publish_slot("/wt-old", {s1: [0]})
reuse_table = {"caller.rs|digest|receipt|producer": (s1, 0)}

# Build for /wt-new. Step 1: reuse lookup in graph.sqlite hits.
hit = reuse_table["caller.rs|digest|receipt|producer"]
print("build: lookup_reuse hit", hit)

# Reconcile runs between the two steps: /wt-old is proven gone, nothing names s1.
m.cat.execute("UPDATE slot SET graph=NULL WHERE locator='/wt-old'")
print("reconcile (build inputs not yet registered) deleted", m.collect())

# Step 2: hold_inputs, then the CAS. Nothing in either step checks the registry.
m.cat.execute("INSERT INTO build(input, owned) VALUES(?,?)", (json.dumps([hit[0]]), "[]"))
print("build: hold_inputs ok")
snap = m.publish_slot("/wt-new", {hit[0]: [hit[1]]})
print("build: CAS committed, slot now names snapshot", snap, "over segment", hit[0])
lease = m.open_current("/wt-new")
print("callers(target) on the published snapshot:", m.callers(lease, "target"), " required: ['caller.a'] or a typed failure")
