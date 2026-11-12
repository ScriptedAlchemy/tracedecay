"""Of the 200 real t3code commits the bake-off and fit test replayed, how many hit
a real sparse-path refusal in crates/tracedecay-code-index/src/production/
sparse_increment.rs. File add or remove and manifest are exact. Import edits use a
line regex, so that column is a close estimate. ChangedShare and Go method sets are
not counted, so the total is a lower bound for those."""
import re, subprocess
R = "/Users/bytedance/code/t3code"
def git(*a):
    return subprocess.check_output(["git", "-C", R, *a], text=True, errors="replace")
commits = git("log", "--first-parent", "--format=%H", "57878eab4f..516103e095").split()
imp = re.compile(r"^[+-](?![+-])\s*(import\b|export\s+(\*|\{[^}]*\}|type\s+\{).*\bfrom\b|from\s+\S+\s+import\b|use\s+[\w:{]|pub\s+use\b|mod\s+\w+;|package\s+\w|.*\brequire\(|\}\s*from\s+['\"])")
man = re.compile(r"(\.(json|toml)$)|((^|/)go\.mod$)")
exts = ["*.ts","*.tsx","*.js","*.jsx","*.mjs","*.rs","*.py","*.go"]
tot = dict(n=0, added_or_removed=0, manifest=0, imports=0, any=0); one = dict(tot)
for c in commits:
    rows = [l.split("\t") for l in git("diff","--name-status","--no-renames",c+"^1",c).splitlines() if l]
    ad = any(r[0] in ("A","D") for r in rows)
    m = any(man.search(r[-1]) for r in rows)
    diff = git("diff",c+"^1",c,"--unified=0","--",*exts)
    i = any(imp.match(l) for l in diff.splitlines() if not l.startswith(("+++","---")))
    for d, on in ((tot, True), (one, len(rows) == 1)):
        if on:
            d["n"]+=1; d["added_or_removed"]+=ad; d["manifest"]+=m; d["imports"]+=i; d["any"]+=(ad or m or i)
print("all commits     ", tot); print("one-file commits", one)
