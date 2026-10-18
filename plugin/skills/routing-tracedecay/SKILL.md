---
name: routing-tracedecay
description: Use when you know the kind of code question but not which TraceDecay tool answers it; names the one call to make. Skip when the task administers TraceDecay itself (projects, configuration, storage). Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Routing TraceDecay

Use this when you know the kind of question and not the tool. One row is one
call. After that call answers the question, stop. Do not open a file you have
not already located.

| Moment | Skill | Opening call |
| --- | --- | --- |
| Landed in a repo and need the code that matters for a task | exploring-code | `tracedecay_context` with the task text |
| Who calls this symbol | tracing-functions | `tracedecay_callers` |
| What this symbol calls | tracing-functions | `tracedecay_callees` |
| Is it safe to change this symbol | assessing-impact | `tracedecay_impact` |
| Find an exact string or config key | exploring-code | `tracedecay_grep` |
| Recover what a prior session said | managing-session-context | `tracedecay_message_search` |
| Recall a durable project fact | project-memory | `tracedecay_fact_store_search` |
| Facts about one entity | project-memory | `tracedecay_fact_store_probe` |
| Facts naming several entities together | project-memory | `tracedecay_fact_store_reason` |
| Facts about an entity's neighbors | project-memory | `tracedecay_fact_store_related` |
| Find a symbol by its bare name | exploring-code | `tracedecay_find_exact_symbol` |
| Find a symbol by its full qualified name | exploring-code | `tracedecay_by_qualified_name` |
| Read a known symbol's signature without its body | exploring-code | `tracedecay_signature` |
| Review a diff | reviewing-changes | `tracedecay_diff_context` |
| Which tests reach a change | assessing-impact | `tracedecay_affected_tests` |
| A build or type error | fixing-build-and-type-errors | `tracedecay_diagnose` |

A name you already have is a lookup, not a tour of the repository. A question
one `grep` in a file you can already name would answer does not need the graph.

**Report:** the answer the one call returned, plus its freshness line when it is
not `fresh`. Do not chain calls the row did not name.
