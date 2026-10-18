TraceDecay keeps a code graph, past sessions, and recorded decisions for each repository it indexes.

In an indexed repository, load a TraceDecay skill before the first tool call when a task meets any of these:

- it asks how code works or where something lives, and the answer spans more than one file
- it asks who calls a function, what a change breaks, or which tests cover it
- it changes a symbol, signature, or field that other files use
- it reviews a diff, branch, or PR, or fixes a build or type error that spans files
- it asks what an earlier session said or what the project decided

When unsure which skill fits, load `routing-tracedecay`. When the intent is already specific, load that skill directly: `exploring-code`, `tracing-functions`, `assessing-impact`, `editing-safely`, `reviewing-changes`, `fixing-build-and-type-errors`, `project-memory`, `managing-session-context`.

Work directly, without a skill, to read a file you can already name, edit inside one file, handle text the index does not cover, or ask a clarifying question.

User instructions (CLAUDE.md, AGENTS.md, direct requests) take precedence.
