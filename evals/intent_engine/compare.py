"""Head-to-head tables from summary.json for the report. usage: compare.py CONFIG [CONFIG ...]"""
import json, os, sys, collections
HERE = os.path.dirname(os.path.abspath(__file__)); sys.path.insert(0, HERE)
from common import CATS, load
S = json.load(open(os.path.join(HERE, "summary.json")))
cfgs = [c for c in sys.argv[1:] if c in S]
ds = load(os.path.join(HERE, "dataset.json"))
cnt = collections.Counter(l for c in ds for l in c["labels"])
print("n=%d none=%d ctx=%d" % (len(ds), sum(not c["labels"] for c in ds), sum(c["context_needed"] for c in ds)), dict(cnt))
f = lambda x: "-" if x is None else "%.2f" % x
print("\n| metric | " + " | ".join(cfgs) + " |\n|---|" + "---|" * len(cfgs))
rows = [("macro AUC", lambda s: s["macro_auc"]), ("macro AUC (ctx subset)", lambda s: s["macro_auc_ctx"]),
        ("macro F1 @ best t (in-sample)", lambda s: s["best_t_in_sample"]["macro_f1"]),
        ("macro F1 @ LOO-chosen t", lambda s: s["macro_loo_f1"]),
        ("macro F1 @ 0.6", lambda s: s["t0.6"]["macro_f1"]),
        ("none false-hint @ best t", lambda s: s["best_t_in_sample"]["all"]["none_false_hint_rate"]),
        ("none false-hint @ 0.6", lambda s: s["t0.6"]["all"]["none_false_hint_rate"]),
        ("ctx top-1 acc @ best t", lambda s: s["best_t_in_sample"]["ctx"]["top1_acc"]),
        ("ctx top-1 acc @ 0.6", lambda s: s["t0.6"]["ctx"]["top1_acc"]),
        ("ctx exact-set acc @ 0.6", lambda s: s["t0.6"]["ctx"]["exact_set_acc"]),
        ("all top-1 acc @ best t", lambda s: s["best_t_in_sample"]["all"]["top1_acc"]),
        ("all top-1 acc @ 0.6", lambda s: s["t0.6"]["all"]["top1_acc"]),
        ("ECE", lambda s: s["reliability"]["ece"]),
        ("median ms / call", lambda s: s["latency"]["median_ms"])]
for name, fn in rows:
    print("| %s | %s |" % (name, " | ".join(f(fn(S[c])) if name != "median ms / call" else str(fn(S[c])) for c in cfgs)))
print("\nper-category F1 at best t (t) / F1 at 0.6 / AUC")
print("| category (n) | " + " | ".join(cfgs) + " |\n|---|" + "---|" * len(cfgs))
for k in CATS:
    cells = []
    for c in cfgs:
        p = S[c]["per_cat"][k]
        cells.append("%s (%.2g) / %s / %s" % (f(p["best_f1"]), p["best_t"], f(p["sweep"]["0.6"]["f1"]), f(p["auc"])))
    print("| %s (%d) | %s |" % (k, cnt[k], " | ".join(cells)))
print("\nrecommended thresholds (in-sample best-F1 t; LOO F1)")
for c in cfgs:
    print(c, {k: (S[c]["per_cat"][k]["best_t"], round(S[c]["per_cat"][k]["loo_f1"] or 0, 2)) for k in CATS})
