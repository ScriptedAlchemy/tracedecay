# Retired: task-plan graph and multi-agent executor

**Status:** permanently removed; no delivery PR.

TraceDecay does not parse Markdown/YAML plans, track rewrite completion, choose
next PR work, allocate development agents, or expose a task board/executor. The
former design duplicated developer planning as a product and delayed the actual
storage, retrieval, context, workflow, and UI work.

Do not recreate its plan/task databases, DAGs, ledgers, packets, leases,
attempts, readiness/fairness policy, edit bundles, generated bindings, Kanban,
or Orchestration Lab under new names.

Retained product behavior has direct owners:

- Plan 16: project/repository/worktree/ref scope and safe cleanup.
- Plans 22–23: advisory context and temporal session/agent evidence.
- Plan 32: real user-authored typed workflows executed by the daemon.
- Plan 17: supported public API and SDK bindings.

Developer agents may coordinate through their native tools. That activity is
not TraceDecay runtime state. These Markdown plans are documentation only and
cannot be imported or executed by any product workflow.
