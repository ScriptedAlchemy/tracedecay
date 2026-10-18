"""Builds dataset.json (frozen before any model run). Labels written by hand, no model in the loop."""
import json, hashlib, collections

def U(t): return {"role": "user", "text": t}
def A(t): return {"role": "assistant", "text": t}
def T(t): return {"role": "tool", "text": t}

C = []
def case(turns, prompt, labels, ctx):
    C.append({"id": "c%03d" % (len(C) + 1), "turns": turns, "current_prompt": prompt,
              "labels": labels, "context_needed": ctx})

SR, MS, CG, IM, RE, RV, TO, PC, UC = ("session_recall", "memory_store", "call_graph", "impact",
    "refactor_edit", "review_changes", "type_orientation", "project_context", "unexpected_changes")

# ---------------- session_recall ----------------
case([U("the celery worker keeps OOMing on the nightly export"),
      A("Memory climbs in export_rows; it materializes the whole queryset."),
      T("Bash: rg -n 'iterator\\(' app/exports")],
     "didn't we already hit this a couple weeks back and settle on chunking with a server-side cursor? dig up what we decided", [SR], False)
case([U("Codex worked out the root cause of the flaky checkout e2e test in another window yesterday"),
      A("I don't see any notes about that in the repo or the PR description.")],
     "can you find what it said?", [SR], True)
case([A("I've added the feature flag check to the pricing endpoint."),
      T("Bash: go test ./internal/pricing/..."), A("All tests pass.")],
     "what was the reasoning for keeping the legacy pricing path behind the flag instead of deleting it? I know we talked it through on monday", [SR], False)
case([U("the terraform plan wants to recreate the RDS instance"),
      A("That's because engine_version changed from 14.9 to 15.4.")],
     "same thing happened before and someone worked around it, I think it was in the session where we did the vpc split. what did we do then?", [SR], False)
case([U("heads up, the reconnect test is the one the other agent fixed last thursday"),
      T("Bash: cargo test -p sync"),
      A("reconnect_after_timeout failed: expected Connected, got Backoff.")],
     "that broke again, go look at how it was handled", [SR], True)
case([U("let's continue the kafka consumer rewrite"),
      A("I don't have context on prior work here. Which branch?")],
     "feature/consumer-v2. you had a plan with like 5 steps when we stopped yesterday, start from wherever you left off", [SR], False)
case([A("I can wire the webhook signature check with HMAC-SHA256."), U("hold on")],
     "I'm pretty sure a teammate's Claude session already wrote this for the billing service and the transcript is in our shared logs. check before you reinvent it", [SR], False)
case([U("why did we pin pydantic to 1.10?"),
      A("I don't see a comment explaining the pin in pyproject.toml or the git log.")],
     "it came up in one of our chats a while ago, look there", [SR], False)
case([U("I had you write a migration script for the orders table sometime last week"),
      A("I don't see it in the repo; it may never have been committed.")],
     "yeah it never got committed. can you get it back?", [SR], True)
case([A("Should I use the same retry policy as the payments client?"), U("which one was that"),
      A("I'm not sure, I don't have it in this session.")],
     "find it", [SR], True)
case([U("rebuilding the release notes for 3.2"), T("Bash: git log v3.1..HEAD --oneline | wc -l"),
      A("There are 214 commits.")],
     "what did I tell you yesterday about which ones to leave out?", [SR], False)
case([U("the summary the review agent gave this morning was really good"), A("Glad it helped.")],
     "paste its list of risks here again, I closed that tab", [SR], True)
case([U("deploy failed with exit 137"), A("137 usually means the container was OOM-killed.")],
     "have we seen this in any earlier session? grep the transcripts for 137", [SR], False)
case([U("the other agent and I spent an hour on the bazel remote cache config earlier"),
      A("Want me to start investigating the cache misses from scratch?")],
     "no, start from what it found", [SR], True)

# ---------------- memory_store ----------------
case([T("Bash: npm install zod"), A("Added zod to dependencies.")],
     "we use pnpm in this repo, not npm. from now on always use pnpm", [MS], False)
case([A("I formatted the new Go files with gofmt and the YAML with 4-space indent. Do you want 2-space YAML just here or going forward?")],
     "going forward, everywhere", [MS], True)
case([A("FYI staging Postgres is at pg-stg-02.internal:6432 behind pgbouncer, per the runbook. Want me to keep that in mind for future sessions?")],
     "yes please", [MS], True)
case([U("run the integration tests"), T("Bash: make test-integration"),
      A("They fail with connection refused; DOCKER_HOST isn't set.")],
     "right, on my machine docker runs through colima, the socket is ~/.colima/default/docker.sock. keep that for next time so I don't have to tell you again", [MS], False)
case([A("I wrote the new endpoint tests using unittest.TestCase.")],
     "we only use pytest-style functions with fixtures in this codebase, never TestCase classes. please don't make me say that again", [MS], False)
case([U("commit that"), T("Bash: git commit -m 'Fix null deref in parser'"),
      A("Committed. The repo's recent history uses Conventional Commit prefixes like fix: and feat:.")],
     "yeah, stick to that style in every commit from here on", [MS], False)
case([U("add a dependency on lodash"), A("Done, added lodash@4.17.21.")],
     "actually we avoid lodash, the team decided on native array methods or remeda. remember this", [MS], False)
case([A("The API returns timestamps in local time; should I convert to UTC in the serializer?"), U("yes"),
      A("Done. Is UTC-everywhere a general rule for this service or just this endpoint?")],
     "general rule, all services actually", [MS], True)
case([U("deploy to staging"), T("Bash: ./scripts/deploy.sh staging"), A("Deployed.")],
     "fyi for the future: never deploy on fridays after 3pm, our on-call is thin. that applies to any deploy you do for me", [MS], False)
case([U("the tests keep timing out"),
      A("The full suite takes ~9 minutes; I can run just the affected package with go test ./pkg/auth/... instead.")],
     "yes, do that by default always, only run the full suite when I ask", [MS], False)
case([A("I noticed you prefer early returns over nested ifs, so I restructured handle_request that way.")],
     "correct, and that's how I want it in every file you touch. note it down", [MS], False)
case([U("which python should I use for this"), A("The repo's .python-version says 3.12 but CI uses 3.11."),
      U("CI is right, 3.11 is the target"), A("Got it. Should I record that as a project fact?")],
     "sure", [MS], True)
case([A("I used log.Printf for the new error path."), U("we use zerolog"),
      A("Switched it to zerolog. Want this saved as a convention?")],
     "do it", [MS], True)
case([A("I ran black with the default line length 88."), U("we use 100"),
      A("Reformatted with --line-length 100.")],
     "and keep using that", [MS], True)

# ---------------- call_graph ----------------
case([U("looking at the nightly batch"), A("The nightly batch entry point is jobs/nightly.py.")],
     "who calls reconcile_ledger and from which jobs?", [CG], False)
case([U("where's the retry logic for outbound http"), T("Bash: rg -n 'jitter' src/net"),
      A("It's backoff_with_jitter in src/net/retry.rs.")],
     "who uses it?", [CG], True)
case([A("Here's an overview of the router file: routes are registered in register_routes().")],
     "walk me through what happens from the moment POST /checkout hits the router until the order row is written, every function in between", [CG], False)
case([A("OrderService.cancel() calls refundPayment() and releaseInventory().")],
     "and what does that second one call?", [CG], False)
case([U("the metrics counter double-increments"), A("The increment happens in recordHit in metrics/cache.go.")],
     "is it being invoked from two places?", [CG], False)
case([], "trace how a websocket message gets from the client SDK through the gateway into the room actor", [CG], False)
case([U("this normalize_email helper looks sketchy"), A("It lowercases and strips plus-addressing.")],
     "find everything that reaches it, directly or indirectly", [CG], False)
case([A("flush_buffers is defined in io/writer.c.")],
     "where's it reached from at startup?", [CG], True)
case([A("Found emit_audit_event in audit/emit.py.")],
     "where's it coming from when a user deletes a project?", [CG], True)

# ---------------- impact ----------------
case([A("I'd change parse_config to return Result<Config, ConfigError> instead of panicking.")],
     "what would that break?", [IM], False)
case([], "if I drop the legacy_id column from users, which queries and tests will fall over?", [IM], False)
case([U("working on the event bus"), A("EventEnvelope is defined in proto/events.proto.")],
     "I'm adding a required field to EventEnvelope. what's the blast radius?", [IM], False)
case([U("I just edited src/lib/money.ts to round half-even"), T("Bash: git diff --stat"),
      A("1 file changed, 6 insertions(+), 3 deletions(-).")],
     "which tests should I rerun for this?", [IM], False)
case([A("Option A is to make Session non-Clone; option B is to wrap it in Arc.")],
     "which one touches less code?", [IM], True)
case([], "before I change the default timeout in HttpClientConfig from 30s to 5s, show me everything downstream that depends on it", [IM], False)
case([U("thinking of making Money immutable"),
      A("That would mean removing the add_in_place and set_currency methods.")],
     "how much fallout are we talking?", [IM], True)
case([A("Should I change the default page_size from 50 to 20?")],
     "first tell me how bad that'd be", [IM], True)

# ---------------- refactor_edit ----------------
case([U("switch the postgres adapter to use the connection pool"), T("Edit: src/adapters/postgres.ts"),
      A("The postgres adapter now acquires connections from pool.acquire().")],
     "ok do the same for the other adapters", [RE], True)
case([], "rename fetchUserData to loadUserProfile across the whole frontend, including tests and storybook", [RE], False)
case([A("I replaced the unwrap() in config.rs line 88 with ? and a context message.")],
     "there are like 30 more of those in the crate, fix them all the same way", [RE], False)
case([U("utils/misc.py is a mess")], "move all the date helpers out of utils/misc.py into a new utils/dates.py and fix the imports", [RE], False)
case([U("the Logger constructor now takes a level arg"), A("Updated api/server.go to pass LevelInfo.")],
     "now everywhere else", [RE], True)
case([A("I changed the first assert_eq! in test_parser.rs to include a context message. Want me to apply it to the remaining 41 call sites?")],
     "yep, go", [RE], True)
case([], "swap every moment() call for the dayjs equivalent in the dashboard package", [RE], False)
case([A("Converted UserCard.jsx from a class component to hooks.")],
     "great, now the remaining 12", [RE], True)

# ---------------- review_changes ----------------
case([T("Bash: git add -p"), A("Staged 4 hunks across 3 files.")],
     "look it over before I commit", [RV], True)
case([], "can you review PR #482? focus on the migration and error handling", [RV], False)
case([U("I pushed my changes to fix-rate-limit"), A("Branch fix-rate-limit is 3 commits ahead of main.")],
     "anything wrong in there?", [RV], True)
case([], "sanity check my diff, I'm worried I left debug prints and a hardcoded token", [RV], False)
case([A("I've finished the auth middleware refactor; 7 files changed.")],
     "go through your changes once more with a critical eye before we open the PR", [RV], False)
case([U("Priya opened a PR for the caching layer"), A("PR #1207: 'Add read-through cache for catalog'.")],
     "thoughts?", [RV], True)
case([A("I've pushed 3 commits to the branch.")], "go over them", [RV], True)

# ---------------- type_orientation ----------------
case([A("Storage is a trait with put, get and delete.")], "what implements it?", [TO], False)
case([], "where do we construct RetryPolicy objects? I want to see every place one gets built", [TO], False)
case([], "which code writes to Order.status and which code only reads it?", [TO], False)
case([U("I think there are two different implementations of slugify floating around"),
      A("I found slugify in utils/text.py.")],
     "find the other copy and any other near-duplicates", [TO], False)
case([], "show me the class hierarchy under BaseExporter in the java module", [TO], False)
case([A("The Shape sealed interface is in geometry/Shape.kt.")],
     "list the subclasses and where each gets instantiated", [TO], False)
case([U("ctx.deadline seems to get clobbered"), A("deadline is a field on RequestCtx in rpc/ctx.go.")],
     "who sets it?", [TO], True)
case([A("There's parse_date in utils/time.py and something similar in reports/fmt.py.")],
     "are they the same thing? any others?", [TO], True)

# ---------------- project_context ----------------
case([], "how does the ios-client repo call the /v2/feed endpoint? I want to make sure I don't break them", [PC, IM], False)
case([U("our admin dashboard lives in a separate repo, acme/admin-web"), A("Okay. This repo is the API.")],
     "check how they're parsing the pagination cursor over there", [PC], True)
case([], "the shared proto definitions are in the platform-schemas repo next to this one. pull the current definition of UserEvent from there", [PC], False)
case([], "our sibling service ledger already has an idempotency-key middleware. look at how they did it and port it here", [PC], False)
case([U("the terraform modules are in ../infra-modules"), A("This repo references source = \"../infra-modules/vpc\".")],
     "what variables does that module expect?", [PC], True)
case([U("the Android app is in a separate repo"), A("Noted, this one is the backend.")],
     "do they still send the old X-Client-Version header?", [PC], True)

# ---------------- unexpected_changes ----------------
case([T("Bash: git log --oneline -5"), A("Top commit is 'wip: tweak timeouts' (a1b2c3d). I didn't create it.")],
     "I didn't either. figure out where it came from", [UC], True)
case([], "there are modified files in src/legacy that neither of us touched this session, what's going on", [UC], False)
case([A("I edited handlers/user.go to add validation."), T("Bash: git status"),
      A("git status also shows handlers/admin.go and go.sum modified.")],
     "wait, I never asked for those", [UC], True)
case([], "someone force-pushed over my branch overnight, show me what changed versus my last commit", [UC], False)
case([U("rebase onto main"), T("Bash: git rebase origin/main"), A("Rebased cleanly.")],
     "hm, why does package-lock.json look totally different now? I didn't touch it", [UC], False)
case([U("I stepped away for lunch"), T("Bash: git status --short"),
      A("M src/billing/invoice.rb\n?? tmp/patch.diff")],
     "I didn't leave those there", [UC], True)

# ---------------- multi-label ----------------
case([], "rename UserId to AccountId everywhere and tell me which tests that touches", [RE, IM], False)
case([U("Priya's PR changes compute_tax"), A("PR #88 modifies compute_tax's rounding.")],
     "review it and list who calls compute_tax", [RV, CG], False)
case([], "last week in another session we agreed on a naming scheme for feature flags; find it and apply it to all the flags in config/flags.ts", [SR, RE], False)
case([A("The ios-client repo calls /v2/feed with a cursor param.")],
     "if I rename cursor to page_token what breaks over there?", [IM, PC], True)

# ---------------- none ----------------
case([U("the cleanup command deletes too eagerly")], "add a --dry-run flag to the cleanup command", [], False)
case([], "document how the memory allocator works in the README", [], False)
case([U("disk is filling up on the worker")], "delete session files older than 30 days in the cleanup job", [], False)
case([], "the chat history panel scrolls jumpy when new messages arrive, fix it", [], False)
case([], "write a function that stores user preferences in localStorage", [], False)
case([U("the save button disappears on small screens")], "make the button always visible on mobile", [], False)
case([], "add a deprecation warning that says 'from now on use the v2 endpoints'", [], False)
case([], "the 'remember me' checkbox on the login form doesn't persist anything", [], False)
case([], "what's the difference between a mutex and a rwlock?", [], False)
case([A("All done: the endpoint, tests and docs are updated.")], "thanks, that's all for now", [], False)
case([A("I added the migration file. Should I run the tests?")], "yes", [], True)
case([A("Draft of the setup section is ready. Want me to write it to docs/setup.md?")], "save it", [], True)
case([U("fix the 500 on /login"), A("Fixed: added a null check on user.org."), T("Bash: curl -i localhost:8080/login"),
      A("Still returns 500, now from session.go line 42.")],
     "that broke again?", [], True)
case([A("I bumped the version in package.json to 2.4.0.")], "do the same for the changelog", [], True)
case([A("Should this retry setting be configurable per-tenant or a global constant?")], "global constant for now", [], True)
case([U("the history endpoint is slow"), A("It's doing N+1 queries on events.")], "fix it", [], True)
case([], "run the linters and fix whatever they complain about", [], False)
case([], "what does this regex do: ^(?:[a-z0-9-]+\\.)+[a-z]{2,}$", [], False)
case([], "bump tokio to 1.40 and make sure it builds", [], False)
case([], "add a session timeout of 15 minutes to the express app", [], False)
case([], "write a parser for the Zoom transcript VTT files we export", [], False)
case([], "write a blog post outline about how we refactored our billing system last year", [], False)
case([], "add a 'previous' button to the pagination component", [], False)
case([A("Tests pass. Want me to open the PR?")], "sure, and use the usual template", [], True)
case([], "the memory usage graph in grafana spiked after the deploy, can you look at the heap profile I attached", [], False)
case([], "the cache should remember the last 100 lookups, implement an LRU", [], False)
case([], "lol ok, coffee break, back in 10", [], False)
case([U("always-on profiling is too expensive"), A("I can gate it behind a PROFILING_ENABLED env var.")],
     "do that, default off", [], True)
case([], "write a code review checklist for new hires as a markdown doc", [], False)
case([], "add a call to flush() at the end of shutdown()", [], False)
case([], "add an 'impact' field to the incident report form", [], False)
case([A("I've restored the previous version of the config from git.")], "great, now remember to restart the service", [], False)
case([A("Should I apply the same fix to the staging config too?")], "no, just prod", [], True)
case([A("Want me to add a unit test for this edge case?")], "do it", [], True)
case([A("There are 14 call sites using the old API. Want me to update them all?")], "not yet, just the one in main.go", [], True)
case([A("I can save the flag naming rule as a convention, or just apply it here.")], "just here", [], True)
case([U("the other agent is working on the frontend in parallel"), A("Okay, I'll stay inside /api.")],
     "yes, and don't touch its files", [], True)

if __name__ == "__main__":
    raw = json.dumps(C, indent=1, ensure_ascii=False).encode()
    open("dataset.json", "wb").write(raw)
    print("sha256", hashlib.sha256(raw).hexdigest())
    cnt = collections.Counter(l for c in C for l in c["labels"])
    print("cases", len(C), "context_needed", sum(c["context_needed"] for c in C),
          "none", sum(not c["labels"] for c in C), "multi", sum(len(c["labels"]) > 1 for c in C))
    print(dict(cnt))
    ctx = collections.Counter(l or "none" for c in C if c["context_needed"] for l in (c["labels"] or [None]))
    print("ctx by cat", dict(ctx))
