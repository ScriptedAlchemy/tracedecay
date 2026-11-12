"""Raw S3+ operation record for removing the 49 divergent worktrees, against the
design's claim. Design: "Reclamation reads no payload", "Reclamation never re-reads
unchanged content | Met by construction", sketch.rs:255 reconcile "Reads no payload"."""
import json
ev = [json.loads(l) for l in open("/tmp/tracedecay-storage-redesign/arena/bakeoff/S3plus.jsonl")]
for label in ("remove_same_commit_worktree", "remove_divergent_worktrees", "idle_reconcile"):
    e = next(x for x in ev if x["label"] == label)
    print(f"{label}: read {e['read_bytes']:,} written {e['written_bytes']:,} returned {e['returned_bytes']:,} merges {e['merges']}")
e = next(x for x in ev if x["label"] == "remove_divergent_worktrees")
print(f"read / returned = {e['read_bytes']/e['returned_bytes']:.2f}, written / returned = {e['written_bytes']/e['returned_bytes']:.2f}")
print(f"per worktree: read {e['read_bytes']/49/2**20:.1f} MiB, written {e['written_bytes']/49/2**20:.1f} MiB")
m = json.load(open("/tmp/tracedecay-storage-redesign/arena/bakeoff/S3plus-merges.json"))
d = [x for x in m if x["reason"] == "dead"]
big = max(d, key=lambda x: x["input_encoded_bytes"])
print(f"dead-compaction merges {len(d)}, encoded input {sum(x['input_encoded_bytes'] for x in d):,} bytes, removed {sum(x['dead_documents_removed'] for x in d):,} docs")
print(f"largest: input {big['input_encoded_bytes']:,} bytes, {big['input_documents']} docs in, {big['output_documents']} out")
