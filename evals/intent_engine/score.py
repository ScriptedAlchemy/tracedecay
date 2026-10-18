"""Score results/*.json -> summary.json + printed tables."""
import json, glob, os, statistics, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from common import CATS

TH = [0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9]

def auc(pos, neg):
    if not pos or not neg: return None
    s = sum((p > n) + 0.5 * (p == n) for p in pos for n in neg)
    return s / (len(pos) * len(neg))

def prf(rows, cat, t):
    tp = sum(1 for r in rows if r["probs"][cat] >= t and cat in r["labels"])
    fp = sum(1 for r in rows if r["probs"][cat] >= t and cat not in r["labels"])
    fn = sum(1 for r in rows if r["probs"][cat] < t and cat in r["labels"])
    p = tp / (tp + fp) if tp + fp else None
    rc = tp / (tp + fn) if tp + fn else None
    f = 2 * tp / (2 * tp + fp + fn) if tp + fp + fn else None
    return {"tp": tp, "fp": fp, "fn": fn, "precision": p, "recall": rc, "f1": f}

def case_metrics(rows, ths):
    """ths: per-cat threshold dict. top1: argmax over cats passing threshold."""
    exact = top1 = anyhit = 0
    none_rows = [r for r in rows if not r["labels"]]
    none_fh = 0
    for r in rows:
        fired = {c for c in CATS if r["probs"][c] >= ths[c]}
        exact += fired == set(r["labels"])
        passing = [(r["probs"][c] - ths[c], c) for c in fired]
        pick = max(passing)[1] if passing else None
        top1 += (pick is None and not r["labels"]) or (pick in r["labels"])
        if not r["labels"] and fired: none_fh += 1
        if r["labels"]: anyhit += bool(fired & set(r["labels"]))
    n_pos = sum(1 for r in rows if r["labels"])
    return {"n": len(rows), "exact_set_acc": exact / len(rows), "top1_acc": top1 / len(rows),
            "pos_cases_with_a_correct_hint": anyhit / n_pos if n_pos else None,
            "none_false_hint_rate": none_fh / len(none_rows) if none_rows else None,
            "none_n": len(none_rows)}

def reliability(rows, cats=CATS):
    bins = [(0, .1), (.1, .3), (.3, .5), (.5, .7), (.7, .9), (.9, 1.01)]
    out = []; ece = 0; tot = 0
    pairs = [(r["probs"][c], c in r["labels"]) for r in rows for c in cats]
    for lo, hi in bins:
        b = [(p, y) for p, y in pairs if lo <= p < hi]
        if b:
            mp = sum(p for p, _ in b) / len(b); fr = sum(y for _, y in b) / len(b)
            out.append({"bin": "%.1f-%.1f" % (lo, min(hi, 1)), "n": len(b), "mean_p": round(mp, 3), "frac_pos": round(fr, 3)})
            ece += len(b) * abs(mp - fr); tot += len(b)
    return {"buckets": out, "ece": round(ece / tot, 4)}

def lat(rows):
    ms = sorted(r["ms"] for r in rows)
    return {"median_ms": round(statistics.median(ms)), "p90_ms": round(ms[int(0.9 * (len(ms) - 1))]),
            "median_state_tokens": statistics.median(r["state_tokens"] for r in rows),
            "max_state_tokens": max(r["state_tokens"] for r in rows),
            "cases_truncated": sum(r["dropped"] > 0 for r in rows)}

def score_config(res, fixed_t=None):
    rows = res["rows"]; ctx = [r for r in rows if r["context_needed"]]; nctx = [r for r in rows if not r["context_needed"]]
    per = {}
    for c in CATS:
        pos = [r["probs"][c] for r in rows if c in r["labels"]]
        neg = [r["probs"][c] for r in rows if c not in r["labels"]]
        sweep = {str(t): prf(rows, c, t) for t in TH}
        best = max(TH, key=lambda t: (sweep[str(t)]["f1"] or 0, t))
        per[c] = {"n_pos": len(pos), "auc": auc(pos, neg), "auc_ctx": auc(
            [r["probs"][c] for r in ctx if c in r["labels"]], [r["probs"][c] for r in ctx if c not in r["labels"]]),
            "sweep": sweep, "best_t": best, "best_f1": sweep[str(best)]["f1"]}
    # leave-one-out threshold choice: for each held-out case pick best-F1 threshold on the rest
    loo = {}
    for c in CATS:
        tp = fp = fn = 0
        for i, r in enumerate(rows):
            rest = rows[:i] + rows[i + 1:]
            t = max(TH, key=lambda t: (prf(rest, c, t)["f1"] or 0, t))
            hit = r["probs"][c] >= t; y = c in r["labels"]
            tp += hit and y; fp += hit and not y; fn += (not hit) and y
        per[c]["loo_f1"] = 2 * tp / (2 * tp + fp + fn) if tp + fp + fn else None
        per[c]["loo_p"] = tp / (tp + fp) if tp + fp else None
        per[c]["loo_r"] = tp / (tp + fn) if tp + fn else None
    out = {"per_cat": per, "macro_loo_f1": statistics.mean(per[c]["loo_f1"] or 0 for c in CATS)}
    for name, t in [("t0.5", {c: 0.5 for c in CATS}), ("t0.6", {c: 0.6 for c in CATS}),
                    ("t0.7", {c: 0.7 for c in CATS}),
                    ("best_t_in_sample", {c: per[c]["best_t"] for c in CATS})] + ([("fixed", fixed_t)] if fixed_t else []):
        mf = statistics.mean(prf(rows, c, t[c])["f1"] or 0 for c in CATS)
        out[name] = {"macro_f1": mf, "macro_f1_ctx": statistics.mean(prf(ctx, c, t[c])["f1"] or 0 for c in CATS),
                     "all": case_metrics(rows, t), "ctx": case_metrics(ctx, t), "no_ctx": case_metrics(nctx, t)}
    out["macro_auc"] = statistics.mean(per[c]["auc"] for c in CATS)
    out["macro_auc_ctx"] = statistics.mean(per[c]["auc_ctx"] for c in CATS if per[c]["auc_ctx"] is not None)
    out["reliability"] = reliability(rows)
    out["latency"] = lat(rows)
    if "choice" in rows[0]:
        def ch(rs):
            return sum((r["choice"] == "none" and not r["labels"]) or r["choice"] in r["labels"] for r in rs) / len(rs)
        out["choice_top1_acc"] = {"all": ch(rows), "ctx": ch(ctx), "no_ctx": ch(nctx)}
        nr = [r for r in rows if not r["labels"]]
        out["choice_none_false_hint_rate"] = sum(r["choice"] != "none" for r in nr) / len(nr)
        out["choice_argmax_per_cat"] = {c: {
            "tp": sum(r["choice"] == c and c in r["labels"] for r in rows),
            "fp": sum(r["choice"] == c and c not in r["labels"] for r in rows),
            "fn": sum(r["choice"] != c and c in r["labels"] for r in rows)} for c in CATS}
    return out

if __name__ == "__main__":
    here = os.path.dirname(os.path.abspath(__file__))
    fixed = json.load(open(os.path.join(here, "fixed_thresholds.json"))) if os.path.exists(os.path.join(here, "fixed_thresholds.json")) else None
    summary = {}; allres = {}
    for f in sorted(glob.glob(os.path.join(here, "results", "*.json"))):
        res = json.load(open(f)); allres[res["config"]] = res
    if "B_noul_text" in allres and "C_choice_text" in allres:
        # derived: noul probs for session_recall/memory_store, choice probs for the rest (two calls: 2 noul rows + 1 choice row)
        b = {r["id"]: r for r in allres["B_noul_text"]["rows"]}
        rows = []
        for r in allres["C_choice_text"]["rows"]:
            p = dict(r["probs"]); p["session_recall"] = b[r["id"]]["probs"]["session_recall"]; p["memory_store"] = b[r["id"]]["probs"]["memory_store"]
            rows.append({**r, "probs": p, "ms": r["ms"] + 2 * 340})
            rows[-1].pop("choice")
        allres["H_hybrid_text"] = {"config": "H_hybrid_text", "rows": rows}
    for k, res in allres.items():
        summary[k] = score_config(res, fixed)
    json.dump(summary, open(os.path.join(here, "summary.json"), "w"), indent=1)
    f2 = lambda x: "  -  " if x is None else "%.2f" % x
    print("config                     macroAUC ctxAUC | t0.5: mF1 top1 top1ctx noneFH | t0.6: mF1 top1 top1ctx noneFH | best: mF1 top1 top1ctx noneFH | ECE  | med/p90 ms")
    for k, s in summary.items():
        line = "%-26s %s    %s  |" % (k, f2(s["macro_auc"]), f2(s["macro_auc_ctx"]))
        for nm in ("t0.5", "t0.6", "best_t_in_sample"):
            m = s[nm]; line += " %s %s %s %s |" % (f2(m["macro_f1"]), f2(m["all"]["top1_acc"]), f2(m["ctx"]["top1_acc"]), f2(m["all"]["none_false_hint_rate"]))
        line += " LOOmF1 %s | %.3f | %d/%d" % (f2(s["macro_loo_f1"]), s["reliability"]["ece"], s["latency"]["median_ms"], s["latency"]["p90_ms"])
        if "choice_top1_acc" in s:
            line += "  argmax top1 all/ctx %.2f/%.2f noneFH %.2f" % (s["choice_top1_acc"]["all"], s["choice_top1_acc"]["ctx"], s["choice_none_false_hint_rate"])
        print(line)
    print()
    for k, s in summary.items():
        print(k, "per-cat AUC (ctx AUC) | bestT F1 P/R")
        for c in CATS:
            p = s["per_cat"][c]; b = p["sweep"][str(p["best_t"])]
            print("   %-19s n=%2d auc %s (ctx %s)  best t=%.2f f1 %s p %s r %s | LOO f1 %s p %s r %s" % (c, p["n_pos"], f2(p["auc"]), f2(p["auc_ctx"]), p["best_t"], f2(p["best_f1"]), f2(b["precision"]), f2(b["recall"]), f2(p["loo_f1"]), f2(p["loo_p"]), f2(p["loo_r"])))
