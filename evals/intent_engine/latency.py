"""Latency scaling: number of noul questions x state length. Writes results_latency.json."""
import json, os, sys, time, statistics, warnings
warnings.filterwarnings("ignore")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from laya import Agent
from common import *
from run import FILLER, budgeted_text, tok_len

a = Agent("convaiinnovations/laya", device="cpu")
ds = load(os.path.join(os.path.dirname(os.path.abspath(__file__)), "dataset.json"))
c = next(x for x in ds if x["id"] == "c005")
states = {"short": prompt_only(c), "window": text_window(c),
          "budget150": budgeted_text(a, c, FILLER + c["turns"], 150, 60),
          "budget300": budgeted_text(a, c, FILLER + c["turns"], 300, 80),
          "full_460": text_window(c, turns=FILLER + c["turns"])}
qs_all = noul_questions()
out = []
a.predict("warm", qs_all)
for sname, st in states.items():
    for n in (1, 2, 4, 9):
        qs = dict(list(qs_all.items())[:n]); ts = []
        for _ in range(7):
            t = time.perf_counter(); r = a.predict(st, qs); ts.append((time.perf_counter() - t) * 1000)
        rec = {"state": sname, "state_tokens": tok_len(a, st), "questions": n, "input_tokens": r["usage"]["input_tokens"],
               "median_ms": round(statistics.median(ts)), "p90_ms": round(sorted(ts)[5])}
        out.append(rec); print(rec, flush=True)
    ts = []
    for _ in range(7):
        t = time.perf_counter(); a.predict(st, CHOICE_Q); ts.append((time.perf_counter() - t) * 1000)
    rec = {"state": sname, "questions": "choice10", "median_ms": round(statistics.median(ts))}
    out.append(rec); print(rec, flush=True)
json.dump(out, open("results_latency.json", "w"), indent=1)
