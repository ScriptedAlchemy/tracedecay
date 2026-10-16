# Session-recall hint model eval

The Codex prompt hook suggests `tracedecay_message_search` / `tracedecay_lcm_grep`
when a prompt asks for earlier context. The keyword predicate
(`asks_for_session_recall` in
`crates/tracedecay-agent-hosts/src/hooks/tool_hints/classifiers.rs`) catches
every phrasing in the Rust eval matrix but few others. This eval measures an
optional second opinion from [Laya](https://pypi.org/project/laya/), a local
non-autoregressive decision model that answers typed questions with
calibrated probabilities.

## How the hook uses the model

Opt-in through the environment of the process that runs the Codex hooks:

| Variable | Meaning | Default |
| --- | --- | --- |
| `TRACEDECAY_HINT_RECALL_MODEL_URL` | base URL of a running `laya-serve` | unset: keyword rules only |
| `TRACEDECAY_HINT_RECALL_MODEL_TIMEOUT_MS` | whole-request budget | `250` |

The model is consulted only for a bare user prompt that no keyword rule
classified. It asks a single yes/no (`noul`) question. A probability of 0.6 or
more yields the session-recall hint. An unset URL, an unreachable server, a
timeout, a non-200 status or a malformed answer all leave the keyword result
standing. Delivery budget, dedupe and escalation
(`tracedecay-policy::hint_delivery`) are unchanged.

```bash
pip install 'laya[serve]'
LAYA_HOST=127.0.0.1 LAYA_PORT=8000 LAYA_MODELS=english LAYA_DEFAULT_MODEL=english laya-serve
export TRACEDECAY_HINT_RECALL_MODEL_URL=http://127.0.0.1:8000
```

## Running the eval

```bash
python3 evals/hint_recall_model/run.py --url http://127.0.0.1:8000 --verbose
```

`heldout.json` contains 78 prompts that were written apart from the keyword
lists:

- 30 natural requests for earlier context;
- 25 hard negatives that mention memory, sessions, history or transcripts
  without asking for recall;
- 23 prompts for other categories, plus off-topic chat.

## Results

Measured with `laya` 0.4.8, English checkpoint at revision `7b928d8`, CPU,
over `laya-serve`. The keyword column already includes the two phrases this
change removed.

| Held-out set (78 prompts) | recall requests caught | false hints |
| --- | --- | --- |
| keyword rules | 3 / 30 | 0 / 48 |
| model alone (p ≥ 0.6) | 21 / 30 | 0 / 48 |
| shipped policy (keywords, then model) | **22 / 30** | **0 / 48** |

On the 60 prompt-only cases of the Rust eval matrix, the policy keeps all
8/8 recall cases. It adds one false hint: "change the button text to Save
and run the narrow test" (p = 0.73).

Before this change the keyword list also fired on "document how the memory
curator works in the README" and on "delete transcript files older than 30
days in the cleanup job". The bare phrases `memory curator` and
`transcript files` were removed. Both prompts are now silent regression
cases in `cases_real_world.rs`.

Latency over HTTP on a shared 4-core cloud CPU was about 200 ms median, which
is close to the default 250 ms budget, so slow requests fall back to keywords.
Expect a few milliseconds on a GPU or Apple silicon.
