---
name: routing-tracedecay
description: Pick the one TraceDecay tool for the moment you are in, then stop.
---

# Routing TraceDecay

Use this when you know the kind of question and not the tool. One row is one
call. After that call answers the question, stop. Do not open a file you have
not already located. If the opening call is not in the current tools/list,
call `tracedecay_tool_search` with that name first, then make the call.

| Moment | Skill | Opening call |
| --- | --- | --- |
| Landed in a repo and need the code that matters for a task | exploring-code | `tracedecay_context` with the task text |
| Who calls this symbol | tracing-functions | `tracedecay_callers` |
| What this symbol calls | tracing-functions | `tracedecay_callees` |
| Is it safe to change this symbol | assessing-impact | `tracedecay_impact` |
| Find an exact string or config key | exploring-code | `tracedecay_grep` |
| Recover what a prior session said | managing-session-context | `tracedecay_message_search` |
| Recall a durable project fact | project-memory | `tracedecay_fact_store_search` |
| Review a diff | reviewing-changes | `tracedecay_diff_context` |
| Which tests reach a change | assessing-impact | `tracedecay_affected_tests` |
| A build or type error | fixing-build-and-type-errors | `tracedecay_diagnose` |

A name you already have is a lookup, not a tour of the repository. A question
one `grep` in a file you can already name would answer does not need the graph.
