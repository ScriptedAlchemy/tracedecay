# TraceDecay storage and rename compatibility audit

Date: 2026-06-21

This document records the current policy after the storage unification and rename cleanup work. It is not a compatibility promise for branch-only experiments.

## Current policy

TraceDecay defaults to user/profile-level storage. Project data is registered under the user profile and scoped to the current project for normal MCP, CLI, dashboard, memory, session, and LCM operations. Tools may search or list other projects only when explicitly instructed to do so.

Project-local storage is reserved for explicit local installs and maintenance paths. Normal operation must not create project-local databases just because the current working directory is a repository.

Hermes uses TraceDecay as its memory and context provider. Unpinned Hermes profiles use profile-level TraceDecay storage; profiles with an explicit project root stay scoped to that project.

Codex, Cursor, Hermes, and other agent installers should remove generated project-local artifacts they own while preserving user-authored config entries. Cleanup must not follow symlinked project directories outside the repository.

Windows CI and local test runs should use normal test concurrency. If a test is flaky under concurrency, fix the underlying race instead of serializing the whole suite or adding artificial caps.

## Intentional exceptions

The tree-sitter grammar bundle can keep using the upstream package and repository name that it already has. Do not vendor or rename it locally unless the dependency is published under a new canonical name first.

Old daemon and service names may appear in user-facing cleanup commands when the command is specifically about removing an already-installed old service. Those references are migration instructions, not runtime fallbacks.

Hermes config migration may still recognize an old memory-provider spelling and rewrite it to `tracedecay`. That is a one-way config upgrade path, not a second supported provider name.

Historical changelog entries, benchmark output, and dated design notes may preserve names that existed when they were written. Do not treat those references as current runtime support.

## Review checklist

- No new project-local database path is introduced for default operation.
- New session, memory, dashboard, curation, and LCM data resolves through the user/profile store by default.
- MCP tools scope to the current project by default and only cross project boundaries on explicit request.
- Agent installers write current plugin/config names and remove only generated legacy project-local artifacts.
- CI commands do not set global serialization or test-thread caps.
- Remaining old-name references are limited to the intentional exceptions above.
