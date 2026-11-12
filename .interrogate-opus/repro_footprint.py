"""design.md section 6 charges 0.150 GiB (guess) for "Symbol, edge, binding and
dependency rows" on t3code. This fills the exact sketch.rs schema (SymbolRow,
EdgeRow with their stated primary keys and secondary indexes) with values from a
random sample of the real active generation's graph pages, read only, in an
in-memory SQLite database, and scales by the manifest's real counts. Encoding is
favourable to the design: every occurrence and key is a raw 32-byte blob, spans
are packed into one integer, kind, authority and resolver are small integers.
Nothing is written to disk.
"""
import hashlib
import json
import pathlib
import random
import sqlite3
import sys

BASE = pathlib.Path.home() / ".tracedecay/projects/proj_44730fa64d0bab35"
SAMPLE = int(sys.argv[1]) if len(sys.argv) > 1 else 1200
pointer = max(BASE.glob("code-index-v1/*/active-code-generation-v1.json"), key=lambda p: p.stat().st_mtime)
gen = json.loads(pointer.read_bytes())
manifest = json.loads((pointer.parent / "code-generations-v1" / gen["generation_file"]).read_bytes())["generation"]
pages = manifest["code_graph_pages"]
stats = manifest["statistics"]
print("generation pages", len(pages), "symbols", stats["symbol_count"], "edges", stats["edge_count"])
random.seed(7)
picked = random.sample(pages, min(SAMPLE, len(pages)))

db = sqlite3.connect(":memory:")
db.executescript(
    """
    PRAGMA page_size=4096;
    CREATE TABLE symbol(segment INT, file INT, ordinal INT, occurrence BLOB, stable_key BLOB,
                        name TEXT, qualified_name TEXT, kind INT, span INT,
                        PRIMARY KEY(segment, file, ordinal)) WITHOUT ROWID;
    CREATE INDEX symbol_name ON symbol(segment, name);
    CREATE INDEX symbol_qname ON symbol(segment, qualified_name);
    CREATE INDEX symbol_key ON symbol(segment, stable_key);
    CREATE TABLE edge(segment INT, dst_key BLOB, kind INT, file INT, ordinal INT, src BLOB,
                      authority INT, evidence_span INT, resolver INT,
                      PRIMARY KEY(segment, dst_key, kind, file, ordinal)) WITHOUT ROWID;
    CREATE INDEX edge_out ON edge(segment, file, ordinal);
    """
)


def blob(text):
    return hashlib.sha256(text.encode()).digest()


seen_sym = seen_edge = 0
foreign_from = 0
sym_rows = []
edge_rows = []
sample_bytes = 0
for file_ordinal, page in enumerate(picked):
    path = BASE / "code-index-v1/code-generation-segments-v1" / ("segment-" + page["page_digest"].split(":")[-1] + ".json")
    raw = path.read_bytes()
    sample_bytes += len(raw)
    p = json.loads(raw)
    own = set()
    for ordinal, s in enumerate(p["symbols"]):
        own.add(s["occurrence"])
        sym_rows.append(
            (1, file_ordinal, ordinal, blob(s["occurrence"]), blob(s["identity"]),
             s.get("simple_name", ""), s.get("qualified_name", ""), 1, ordinal * 7919 + 1)
        )
        seen_sym += 1
    for ordinal, e in enumerate(p["edges"]):
        if e["from_occurrence"] not in own:
            foreign_from += 1
        edge_rows.append(
            (1, blob(e["to_occurrence"]), 1, file_ordinal, ordinal, blob(e["from_occurrence"]), 1,
             (e["evidence_span"]["start_byte"] << 20) | e["evidence_span"]["end_byte"]
             if isinstance(e.get("evidence_span"), dict) and "start_byte" in e["evidence_span"] else ordinal * 7919, 1)
        )
        seen_edge += 1

db.execute("BEGIN")
db.executemany("INSERT INTO symbol VALUES(?,?,?,?,?,?,?,?,?)", sym_rows)
db.commit()
sym_bytes = db.execute("PRAGMA page_count").fetchone()[0] * 4096
db.execute("BEGIN")
db.executemany("INSERT INTO edge VALUES(?,?,?,?,?,?,?,?,?)", edge_rows)
db.commit()
all_bytes = db.execute("PRAGMA page_count").fetchone()[0] * 4096
edge_bytes = all_bytes - sym_bytes
per_sym = sym_bytes / max(seen_sym, 1)
per_edge = edge_bytes / max(seen_edge, 1)
print(f"sample: {len(picked)} pages, {seen_sym} symbols, {seen_edge} edges, {sample_bytes/2**20:.1f} MiB read")
print(f"bytes per symbol row with its 3 indexes: {per_sym:.0f}")
print(f"bytes per edge row with its 1 index:     {per_edge:.0f}")
print(f"edges in sample whose from_occurrence is not a symbol of the page's own file: {foreign_from} of {seen_edge}")
est_sym = per_sym * stats["symbol_count"]
est_edge = per_edge * stats["edge_count"]
print(f"scaled to the generation: symbols {est_sym/2**30:.3f} GiB, edges {est_edge/2**30:.3f} GiB, "
      f"together {(est_sym+est_edge)/2**30:.3f} GiB")
print("design.md section 6 charges 0.150 GiB for these plus bindings and dependency rows")
