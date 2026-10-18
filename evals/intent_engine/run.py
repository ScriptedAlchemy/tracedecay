"""Run one or more configs over the frozen dataset; writes results/<config>.json.

usage: run.py CONFIG [CONFIG ...]
"""
import json, os, sys, time, warnings, hashlib
warnings.filterwarnings("ignore")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from laya import Agent
from common import *

FILLER = [
    {"role": "user", "text": "before that, the CSS on the settings page is broken on Safari, the sidebar overlaps the form"},
    {"role": "tool", "text": "Bash: rg -n 'position: sticky|z-index' web/src/styles\nweb/src/styles/layout.css:14:  position: sticky;\nweb/src/styles/layout.css:15:  z-index: 20;\nweb/src/styles/sidebar.css:3:  position: sticky;\nweb/src/styles/sidebar.css:9:  z-index: 30;\nweb/src/styles/form.css:41:  z-index: 1;\nweb/src/styles/modal.css:2:  z-index: 1000;\nweb/src/styles/toast.css:7:  z-index: 1100;"},
    {"role": "assistant", "text": "The sidebar uses position: sticky together with a negative margin on its parent, and Safari computes the containing block differently when the parent has overflow: hidden. I changed sidebar.css to drop the negative margin and set align-self: start on the sidebar, then verified in the Playwright WebKit project that the form is no longer covered. I also noticed the toast z-index is higher than the modal, which may be intentional, so I left it."},
    {"role": "tool", "text": "Bash: npx playwright test --project=webkit settings.spec.ts\nRunning 6 tests using 2 workers\n  ok 1 settings renders sidebar (1.2s)\n  ok 2 settings form is clickable (0.9s)\n  ok 3 settings saves display name (1.4s)\n  ok 4 settings validates email (0.8s)\n  ok 5 settings shows toast on save (1.1s)\n  ok 6 settings closes modal on escape (0.7s)\n6 passed (5.9s)"},
    {"role": "assistant", "text": "All six WebKit settings tests pass. The remaining visual difference is a 2px gap under the header on Safari only, which comes from the default line-height on the h1; I can normalize it in the base stylesheet if you want, but it is cosmetic and unrelated to the overlap."},
]

def tok_len(agent, s):
    from laya.common import encode_text, serialize_state
    return len(encode_text(agent.tok, serialize_state(s), add_special_tokens=False)["input_ids"])

def budgeted_text(agent, c, turns, budget=300, per_turn=80):
    """Most-recent-first: prompt, then turns newest-first, each clipped to per_turn tokens, until budget."""
    from laya.common import encode_text
    head = "User's latest message: %s\n\nEarlier turns, most recent first:" % c["current_prompt"]
    used = tok_len(agent, head); lines = []
    for t in reversed(turns):
        line = "%s: %s" % (ROLE[t["role"]], t["text"])
        ids = encode_text(agent.tok, line, add_special_tokens=False)["input_ids"]
        if len(ids) > per_turn:
            line = agent.tok.decode(ids[:per_turn]) + " ..."
            ids = ids[:per_turn]
        if used + len(ids) + 1 > budget:
            break
        lines.append(line); used += len(ids) + 1
    return head + ("\n" + "\n".join(lines) if lines else " (none)")

def states_for(cfg, agent, c):
    long_turns = FILLER + c["turns"]
    return {
        "A_noul_prompt": prompt_only(c),
        "B_noul_text": text_window(c),
        "B2_noul_text_promptfirst": text_window(c, prompt_first=True),
        "D_noul_json": json_window(c),
        "C_choice_text": text_window(c),
        "C0_choice_prompt": prompt_only(c),
        # long-state truncation variants (filler older turns prepended)
        "L1_long_text_chrono": text_window(c, turns=long_turns),
        "L2_long_list": [{"role": t["role"], "text": t["text"]} for t in long_turns]
                         + [{"role": "user", "text": c["current_prompt"], "latest": True}],
        "L3_long_budget300": budgeted_text(agent, c, long_turns, 300, 80),
        "L4_long_budget150": budgeted_text(agent, c, long_turns, 150, 60),
    }[cfg]

def questions_for(cfg):
    return CHOICE_Q if cfg.startswith("C") else noul_questions()

def main():
    agent = Agent("convaiinnovations/laya", device="cpu")
    ds = load(os.path.join(os.path.dirname(os.path.abspath(__file__)), "dataset.json"))
    sha = hashlib.sha256(open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "dataset.json"), "rb").read()).hexdigest()
    os.makedirs("results", exist_ok=True)
    for cfg in sys.argv[1:]:
        qs = questions_for(cfg); out = []
        agent.predict("warmup", qs)
        for c in ds:
            st = states_for(cfg, agent, c)
            t = time.perf_counter(); r = agent.predict(st, qs); ms = (time.perf_counter() - t) * 1000
            rec = {"id": c["id"], "labels": c["labels"], "context_needed": c["context_needed"], "ms": ms,
                   "state_tokens": r["usage"]["state_tokens"], "dropped": r["usage"]["state_tokens_dropped"]}
            if cfg.startswith("C"):
                rec["probs"] = r["answers"]["intent"]["probabilities"]
                rec["choice"] = r["answers"]["intent"]["choice"]
            else:
                rec["probs"] = {k: v["noul"] for k, v in r["answers"].items()}
            out.append(rec)
        json.dump({"config": cfg, "dataset_sha256": sha, "rows": out}, open("results/%s.json" % cfg, "w"), indent=1)
        print(cfg, "done", flush=True)

if __name__ == "__main__":
    main()
