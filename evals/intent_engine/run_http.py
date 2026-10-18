"""Run noul configs against a decision server over HTTP; writes results/<tag>_<config>.json.

usage: run_http.py --url http://127.0.0.1:8009/v1/systemone --tag kev4b [--subset subset.json] [--send-model kev-latest] CONFIG ...
CONFIG in A_noul_prompt, B_noul_text, B2_noul_text_promptfirst (same states and questions as run.py)."""
import argparse, hashlib, json, os, sys, time, urllib.request
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from common import *

STATES = {"A_noul_prompt": prompt_only, "B_noul_text": text_window,
          "B2_noul_text_promptfirst": lambda c: text_window(c, prompt_first=True)}

def post(url, body, timeout=1800):
    req = urllib.request.Request(url, json.dumps(body).encode(), {"content-type": "application/json"})
    t = time.perf_counter()
    with urllib.request.urlopen(req, timeout=timeout) as r:
        out = json.loads(r.read())
    return out, (time.perf_counter() - t) * 1000

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", required=True); ap.add_argument("--tag", required=True)
    ap.add_argument("--subset"); ap.add_argument("--send-model")
    ap.add_argument("--sample-out", help="write the first raw response here")
    ap.add_argument("--noul-temperature", type=float, help="apply softmax(log p / T) to each noul (released preset T when the server runs uncalibrated); raw kept as probs_raw")
    ap.add_argument("configs", nargs="+")
    a = ap.parse_args()
    ds = load(os.path.join(HERE, "dataset.json"))
    sha = hashlib.sha256(open(os.path.join(HERE, "dataset.json"), "rb").read()).hexdigest()
    if a.subset:
        keep = set(json.load(open(a.subset))["ids"]); ds = [c for c in ds if c["id"] in keep]
    qs = noul_questions()
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    for cfg in a.configs:
        name = "%s_%s" % (a.tag, cfg); path = os.path.join(HERE, "results", name + ".json")
        done = {}
        if os.path.exists(path + ".partial"):
            for line in open(path + ".partial"):
                r = json.loads(line); done[r["id"]] = r
        part = open(path + ".partial", "a")
        for i, c in enumerate(ds):
            if c["id"] in done: continue
            body = {"state": STATES[cfg](c), "questions": qs}
            if a.send_model: body["model"] = a.send_model
            resp, ms = post(a.url, body)
            if a.sample_out and not os.path.exists(a.sample_out):
                json.dump({"request": body, "response": resp}, open(a.sample_out, "w"), indent=1)
            ans = resp["answers"]
            srv_ms = resp.get("latency_ms") or (resp.get("timing") or {}).get("inference_ms")
            rec = {"id": c["id"], "labels": c["labels"], "context_needed": c["context_needed"], "ms": ms, "server_ms": srv_ms,
                   "state_tokens": resp.get("usage", {}).get("input_tokens", 0), "dropped": 0,
                   "probs": {k: float(v["noul"]) for k, v in ans.items()}}
            if a.noul_temperature:
                T = a.noul_temperature; rec["probs_raw"] = rec["probs"]
                rec["probs"] = {k: (p ** (1 / T)) / (p ** (1 / T) + (1 - p) ** (1 / T)) if 0 < p < 1 else p for k, p in rec["probs_raw"].items()}
            part.write(json.dumps(rec) + "\n"); part.flush(); done[c["id"]] = rec
            print(cfg, i, c["id"], "%.0f ms" % ms, flush=True)
        rows = [done[c["id"]] for c in ds]
        json.dump({"config": name, "dataset_sha256": sha, "noul_temperature": a.noul_temperature, "subset": a.subset and os.path.basename(a.subset), "rows": rows},
                  open(path, "w"), indent=1)
        os.remove(path + ".partial")
        print(cfg, "done", flush=True)

if __name__ == "__main__":
    main()
