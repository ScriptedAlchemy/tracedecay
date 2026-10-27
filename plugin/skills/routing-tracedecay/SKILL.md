---
name: routing-tracedecay
description: Use when you know the kind of code question but not which TraceDecay tool answers it; names the one call to make. Skip when the task administers TraceDecay itself (projects, configuration, storage). Load this skill before the first tool call.
metadata:
  opencode/slash: "true"
---

# Routing TraceDecay

Use this when you know the kind of question and not the tool. Start with the
opening call for that question, then follow only the evidence needed to answer
it. Stop when the question is answered. Locate a file before opening it.

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
| Read a known symbol's body | exploring-code | `tracedecay_source_body` with its `node_id` |
| Understand or review a PR or committed ref range | reviewing-changes | `tracedecay_pr_context` |
| Inspect working-tree or staged file/hunk metadata | reviewing-changes | `tracedecay_git_diff` with `scope: working_tree` or `scope: staged` |
| Read the raw working-tree or staged patch | reviewing-changes | Shell `git diff` or `git diff --cached` |
| Inspect possible consumers and tests for specified files | assessing-impact | `tracedecay_diff_context` with `files` |
| Select test files for a changed-file manifest | assessing-impact | `tracedecay_affected` with `files` |
| Read a completed feedback cycle's test selection | assessing-impact | `tracedecay_affected_tests` with its `request_handle` |
| A build or type error | fixing-build-and-type-errors | `tracedecay_diagnose` |

A name you already have is a lookup, not a tour of the repository. A question
one `grep` in a file you can already name would answer does not need the graph.

For PR review, read the briefing's ranked entrypoints before widening the diff.
`diff_context` seeds its analysis with every indexed symbol in the supplied
files; its symbol list does not identify the exact declarations changed by a
patch. Use the patch or PR comparison to establish that distinction.
`tracedecay_git_diff` returns structured metadata without hunk bodies. To read
deleted or non-head code, use shell `git show '<commit-oid>:path/to/file'` at the
briefing's recorded revision and inspect its recorded span.

**Report:** the answer supported by the returned evidence, plus its freshness
line when it is not `fresh` and any coverage needed to qualify the conclusion.
