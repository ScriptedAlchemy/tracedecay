# Workspace refactoring and API migration

Status: planned

## Outcome

TraceDecay provides two distinct refactoring capabilities:

1. a symbol-aware workspace rename for changes that preserve one symbol's identity and semantics; and
2. a bounded API-migration workflow for changes that promote new primary APIs, retain deliberate compatibility surfaces, replace complete definitions where required, and coordinate related terminology updates without misrepresenting the work as a rename.

Both capabilities preview impact before writing, fail closed when their evidence is stale or ambiguous, apply through one transactional edit path, and return a machine-readable manifest. They compose the graph, edit, diagnostics, formatting, and verification capabilities already owned elsewhere; they do not create a second parser, reference resolver, text-replacement engine, diagnostics runner, or tool catalog.

The motivating migration family includes provider-specific production names such as:

- `CaptureClaudeObservationRequest`, `CaptureClaudeObservationOutcome`, and
  `CaptureClaudeObservationRequestError`;
- `capture_claude_observation`;
- `ClaudeObservationProjection` and `ClaudeSessionMessageProjection`; and
- a provider-specific projector-version identifier.

A migration can promote provider-neutral primary names such as
`CaptureObservationRequest`, `CaptureObservationOutcome`,
`CaptureObservationRequestError`, `capture_observation`, `ObservationProjection`,
and `SessionMessageProjection`, while deliberately retaining the old public names
as compatibility aliases or thin wrappers. The projector-version identifier may
change while its persisted numeric or string value remains unchanged.

## Current gap

The installed catalog already exposes useful primitives:

- `tracedecay_rename_preview` is read-only;
- `tracedecay_replace_symbol` replaces one complete definition;
- `tracedecay_multi_str_replace` and `tracedecay_str_replace` perform explicit per-file text edits; and
- callers, references, impact, affected-file, diagnostics, and test discovery can inform a manual refactor.

It does not expose one symbol-aware apply operation that carries a preview across a workspace transaction. Agents must currently sequence definition, import, call-site, test, and documentation edits themselves. That sequencing can leave the checkout temporarily uncompilable, can miss re-exports or less obvious consumers, and cannot prove that a preview remained valid between edits. This plan composes the existing capabilities behind one revalidated transaction rather than teaching agents a longer manual replacement recipe.

## Non-goals

- A semantic generalization is not reported or applied as a pure rename.
- The tools do not infer that string literals, wire values, hash domains, schema columns, migration identifiers, or persisted discriminators should follow a source identifier.
- The tools do not edit generated output, macro expansions without source spans, or unresolved text by guessing.
- The tools do not advertise a language or symbol kind as supported until its resolver and edit adapter pass the corresponding fixtures.
- The migration workflow is not a general-purpose patch language, autonomous rewrite framework, or replacement for compiler diagnostics.

## Ownership

- The code graph remains the authority for stable symbol identity, clean
  generations, canonical historical and cross-project relations, bounded
  traversal, test attribution, definitions, references, callers, public
  re-exports, affected files, and graph freshness.
- Analyzer evidence may add active-document, version-reference, and dispatch
  candidates with independent provenance. Analyzer disagreement with graph
  truth stays explicit and never rewrites graph identity, clean generations,
  canonical relations, bounded traversal, or test attribution.
- Language extractors and resolvers remain the authority for binding a source
  occurrence to a symbol and for reporting unsupported syntax or ambiguous
  identity.
- The existing edit kernel remains the sole owner of preconditioned text edits,
  file preimages, transactional writes, rollback, and edit receipts. Symbol
  replacement and string replacement are reused rather than copied.
- Existing diagnostics and formatter integrations remain the owners of post-edit
  analysis and repository formatting.
- The tool catalog owns schemas, discoverability, capability metadata, and host
  rendering. It lists apply tools only after their implementation and acceptance
  gates ship. See [V2 tool catalog crate](08-tool-catalog-crate.md).
- This plan owns rename and API-migration semantics, preview/apply contracts,
  compatibility-preservation rules, stale-preview behavior, and the end-to-end
  acceptance matrix.
- [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s
  `prepareRename` and `rename` bind only to read-only candidate/preview
  UseCaseIds. They never bind directly to `tracedecay_rename_symbol`,
  API-migration apply, another write-effect entry, `workspace/applyEdit`, or
  opaque server commands. This plan is the sole path that can turn accepted
  candidate evidence into a canonical immutable preview/manifest and
  `EditTransaction` with graph identity, CAS/preconditions, protected-value
  policy, formatting, diagnostics, affected-test selection, verification,
  rollback, and receipt. General LSP `textDocument/codeAction` is deferred and
  is not current ownership here; it cannot ship until a separate owner defines a
  typed candidate-consumption operation, policy classification, canonical
  preview/`EditTransaction` route, and acceptance fixtures. Move-symbol and
  semantic API migration remain TraceDecay workflows owned here because LSP has
  no portable, complete contract for either; an LSP provider may assist
  candidate resolution and post-edit verification but never supplies apply
  authority.

## Product surface

### Pure symbol rename

`tracedecay_rename_preview` remains read-only and becomes the canonical planner for a one-symbol rename. It resolves the expected old symbol identity, validates the proposed repository-style name, enumerates impact, classifies every known occurrence, and emits an immutable preview manifest.

`tracedecay_rename_symbol` is the apply operation. It accepts a preview identifier and digest rather than independently rediscovering a target from a bare name. `dry_run: true` executes the same revalidation and edit planning path but performs no writes.

A pure rename may update a bound symbol across:

- its definition;
- imports, use statements, and public re-exports;
- qualified and unqualified paths;
- type annotations and generic arguments;
- constructors, fieldless constructors, and patterns;
- trait declarations, trait implementations, and resolved trait-method calls;
- inherent methods and their resolved calls;
- enum variants and variant patterns;
- tests and examples; and
- documentation references that the language adapter can bind to the symbol.

Comments or prose that merely contain the old spelling are reported as text-only sites. They are changed only when explicitly selected. String and byte literals are never treated as symbol references solely because their text matches.

### Semantic API migration

`tracedecay_api_migration_plan` plans a named, bounded family of dependent operations. `tracedecay_api_migration_apply` consumes the resulting immutable plan and digest. These tools are used when the old and new APIs do not have a one-to-one identity-preserving relationship.

A migration plan supports only explicit operation kinds:

- `promote_primary`: introduce or rename the provider-neutral primary symbol and move selected production consumers to it;
- `compat_type_alias`: retain an old public type name as an alias to the new primary type when language semantics preserve the required compatibility;
- `compat_wrapper`: retain an old function or method as a thin delegating wrapper with an explicit deprecation policy when requested;
- `replace_definition`: replace a complete definition under an expected symbol identity and definition digest, using the existing symbol-replacement primitive;
- `rename_bound_symbol`: include a pure symbol rename as one operation in the family;
- `replace_term`: update explicitly selected diagnostics, errors, comments, or documentation terminology;
- `remove_delivery_name`: remove delivery-history terminology, including PR-numbered source identifiers, from production APIs while leaving protected persisted values unchanged; and
- `assert_stable_value`: prove that selected persisted, wire, schema, or hash-domain values remain byte-for-byte unchanged.

Operations declare dependencies. For example, a compatibility wrapper is planned after the new primary function exists, and production call sites are moved before the migration verifies that only approved compatibility boundaries still reference the old name.

The provider-neutral promotion use case must support one plan containing the related request, outcome, error, function, projection, session-message projection, and projector-version identifier changes. The plan distinguishes:

- new primary definitions and primary production uses;
- deliberate old-name aliases and wrappers that must remain;
- compatibility tests that are expected to exercise old names;
- terminology-only edits; and
- stable values that must not change.

Type aliases are preferred only when they preserve the intended source and type compatibility. If an alias cannot preserve required behavior, the planner blocks that operation or requires an explicit wrapper/conversion design; it does not silently substitute a different compatibility mechanism.

## Planning contract

### Symbol identity and scope

Every plan request carries:

- the canonical project and worktree identity;
- `node_id`, expected qualified name, symbol kind, defining file, and expected old name for every existing symbol;
- the proposed new name or replacement definition digest;
- explicit include roots and file-kind switches for production, tests, examples, and documentation;
- exclude paths and keep lists for symbols, files, or individual site identifiers;
- requested compatibility aliases or wrappers;
- explicitly selected textual sites, if any; and
- required formatting, diagnostics, and scoped verification gates.

A bare spelling is never sufficient identity for apply. If the expected node, kind, path, old name, or definition digest does not match current state, planning or apply fails closed.

Scope cannot escape the canonical project. Symlinks, submodules, vendored trees, generated files, and files not linked into the graph are reported separately and require an explicit supported policy; path traversal or implicit workspace expansion is rejected.

### Preview manifest

The planner returns a versioned machine-readable manifest containing at least:

- `preview_id`, schema version, logical operation name, and manifest digest;
- project/worktree identity, repository revision, graph revision, and per-file content preconditions;
- expected old and proposed new symbol identities;
- requested scope, exclusions, keep rules, and compatibility rules;
- impact summaries for callers, references, public API/re-exports, affected tests, documentation, and files;
- one record for every known site with file, range, bound symbol, site kind, expected old text, proposed text, and disposition;
- hazards and capability limits;
- the formatter, diagnostics, and scoped verification plan; and
- the expected stable-value assertions.

Site disposition is one of:

- `changed`: an in-scope, bound occurrence will be edited;
- `unchanged`: the site already has the requested state or is an approved compatibility surface;
- `skipped`: policy intentionally leaves the site unchanged, with a reason such as excluded path, keep rule, generated source, unsupported macro form, documentation not selected, or stable value;
- `blocked`: safe apply is impossible until the reported ambiguity, collision, unsupported required site, stale state, or invalid name is resolved.

Markdown rendering summarizes the same typed manifest. It must not invent counts or omit blocked sites that are present in JSON.

### Impact and hazard analysis

Before apply, the planner must report:

- direct and transitive callers where available;
- imports and public re-export paths;
- public API changes and retained compatibility paths;
- affected files, tests, examples, and documentation;
- name collisions in the target namespace;
- shadowing or changed name resolution at each edited site;
- ambiguous symbols and unresolved textual matches;
- macro definitions, invocations, and generated expansions with separate support status;
- unlinked files that contain the old spelling;
- stale or incomplete graph evidence; and
- repository-style naming violations.

For Rust, naming validation follows symbol kind: type-like names, functions and methods, constants/statics, modules, fields, and variants are validated against the repository's configured conventions. Language adapters expose equivalent validation and supported-site capabilities without a regex fallback.

Any blocked site that is required by the requested scope prevents apply. An optional unsupported site may be skipped only when the manifest names it and the caller explicitly accepts the skip set.

## Stable-value safety

The default protected categories are:

- wire field names and values;
- serialized names and discriminators;
- SQL table, column, index, and migration identifiers;
- persisted provider or event names;
- hash-domain separators and canonicalization labels;
- protocol method names and externally stable command/tool names;
- snapshots or golden data that encode a stable external contract; and
- arbitrary string or byte literals not proven to be source-symbol references.

Matching text in these categories is reported but not edited. A caller that intentionally changes one must select exact site identifiers, expected old bytes, and the protected category, and must set an explicit stable-value-change acknowledgement. Such a change is shown as a separate migration operation and requires its own verification gate. Broad `replace all strings` behavior is not accepted.

`assert_stable_value` is the normal operation for provider-neutral source promotion: it records the protected sites and causes apply to roll back if any of their bytes change.

## Atomic apply and stale-preview behavior

Apply performs these steps through one project-scoped edit transaction:

1. acquire the workspace edit lease;
2. verify project/worktree identity, repository and graph revisions, symbol identities, manifest digest, file digests, file modes, scope, and keep rules;
3. recompute collision, shadowing, and ambiguity checks against current state;
4. materialize every edit in memory and verify that ranges do not overlap;
5. capture all preimages, including files a scoped formatter may change;
6. write the complete candidate change through the edit transaction;
7. format only the planned files;
8. refresh required graph evidence and run diagnostics plus requested scoped verification;
9. commit the transaction and emit its receipt only if every required gate passes; otherwise restore every preimage; and
10. close or recover the transaction journal before another edit is accepted.

No apply operation returns success with a partial workspace. A write fault, formatter failure, diagnostics regression, verification failure, cancellation, or process restart must leave either the complete accepted change or the original preimages. Recovery is exercised before subsequent edit operations proceed.

Apply never silently rebases a stale preview. Any changed file digest, graph revision, symbol identity, scope, or keep rule rejects the apply before writes. The result identifies the invalidated preconditions and directs the caller to re-plan.

`tracedecay_api_migration_plan` accepts a prior logical operation name and manifest digest for revalidation. It may classify operations as already satisfied, still pending, or invalidated, but produces a new preview identifier and digest. This makes a deliberately sliced migration resumable after completed slices or concurrent edits without allowing a stale plan to mutate the workspace. Each apply remains atomic for its declared scope.

## Diagnostics, formatting, and verification

The planner records the pre-existing diagnostic baseline for affected files. Apply must:

- run the repository formatter for changed source files;
- refresh or invalidate graph state before post-edit graph-dependent checks;
- run language diagnostics for affected files and report introduced versus pre-existing findings;
- derive a scoped verification set from affected tests and public consumers;
- run caller-supplied verification only through the repository's existing command policy; and
- include formatter, diagnostics, and verification outcomes in the receipt.

Required gate failure rolls back. A caller may omit an optional expensive verification command during planning, but cannot downgrade a gate that the preview marked required without producing a new preview and digest.

## Result and receipt

Success returns the final manifest plus:

- changed, unchanged, skipped, and blocked-site records with reasons;
- final file digests and the transaction receipt;
- compatibility aliases and wrappers created or preserved;
- stable-value assertions checked;
- formatting and diagnostic summaries;
- scoped verification commands and outcomes; and
- graph refresh status.

Failure returns no success receipt. It reports whether no write was attempted or rollback completed, which precondition or gate failed, and any recovery action required. Human-readable output and JSON share one typed result model.

## Delivery slices

### Slice A: preview truth and contracts

- Extend `tracedecay_rename_preview` to require expected symbol identity for apply-grade previews.
- Add scope, keep/exclude rules, impact summaries, collision/shadowing analysis, stale-state evidence, site dispositions, stable-value classification, and the versioned manifest/digest.
- Keep the operation read-only and preserve explicit reporting of text-only matches.
- Add MCP/CLI rendering parity and fixture snapshots for the typed result.

Gate: every supported Rust reference site is classified exactly once; ambiguous or stale evidence produces `blocked`; JSON and markdown counts agree; no file changes during preview or dry-run.

### Slice B: atomic pure rename

- Add `tracedecay_rename_symbol` consuming the preview identifier and digest.
- Reuse the edit transaction, formatting, diagnostics, affected-test selection, and rollback facilities.
- Support Rust definitions, imports, re-exports, paths, annotations, constructors, patterns, trait and inherent methods, enum variants, tests, and bound documentation references.
- Return a transaction receipt and final change manifest.

Gate: the Rust fixture workspace compiles and its scoped tests pass after each successful rename; collision, stale-preview, injected write failure, formatter failure, and diagnostics failure leave all fixture file hashes unchanged.

### Slice C: compatibility-aware API migration

- Add plan/apply support for explicit operation families and dependencies.
- Promote provider-neutral primary APIs while generating requested compatibility type aliases and thin wrappers.
- Enforce that production consumers use primary names while approved compatibility boundaries retain old names.
- Coordinate selected error/documentation terminology changes and assert that persisted/wire/schema/hash-domain values do not change.
- Support revalidation and already-satisfied operations for deliberately sliced migrations.

Gate: the provider-neutral observation fixture promotes the complete related family in one plan, preserves old public entry points exactly where requested, removes unapproved production uses of old names, keeps protected values byte-identical, compiles, and passes old-name compatibility plus new-primary behavior tests.

### Slice D: capability expansion and adoption

- Publish per-language and per-symbol-kind capabilities from the canonical catalog.
- Add another already-supported language fixture without introducing a language-independent text-rewrite fallback.
- Add neutral tool-selection evals that distinguish rename intent from semantic migration intent.
- Update refactoring workflow bundles to compose graph impact, preview, apply, diagnostics, and verification only after each apply tool ships.

Gate: unsupported language/kind combinations fail closed with explicit reasons; the cross-language fixture uses the same manifest/result contract; agents select preview before apply and do not advertise unavailable tools.

## Acceptance matrix

The Rust fixture corpus must cover:

- local and public definitions;
- imports, renamed imports, glob-adjacent cases, and re-exports;
- type aliases and compatibility aliases that must remain;
- type annotations, generic arguments, constructors, and patterns;
- trait methods across declarations, implementations, and resolved calls;
- inherent methods and calls;
- enum variants and variant patterns;
- constants, including an identifier rename whose stable value is unchanged;
- unit tests, integration tests, examples, rustdoc links, and selected prose;
- supported macro definitions/invocations plus an unsupported macro case with an explicit reason;
- generated and unlinked files;
- compatibility wrapper generation and preservation;
- family migration with operation dependencies;
- selected error/documentation terminology updates;
- removal of a PR-numbered or other delivery-history production identifier;
- protected wire, serialized, schema, persisted, and hash-domain values;
- target-name collision and shadowing rejection;
- ambiguous-symbol and invalid-name rejection;
- stale graph, changed file, and changed symbol-identity rejection;
- concurrent edits followed by re-plan and resume;
- overlapping-edit rejection;
- cancellation and fault injection before, during, and after file replacement;
- formatter, diagnostics, and verification failure rollback; and
- changed/unchanged/skipped/blocked manifest and rendering parity.

### Analyzer-candidate merge fixtures

Where rename preview consumes analyzer candidates from Plan 35's
`prepareRename` or `rename`, the fixture corpus must additionally cover:

- graph-only planning: graph evidence alone produces the canonical preview
  manifest and site dispositions;
- analyzer-only candidates: analyzer evidence alone is reported with
  independent provenance and cannot become durable preview truth without graph
  confirmation or an explicit replan against clean content;
- disagreement: graph and analyzer candidates conflict and the manifest keeps
  both provenances explicit without rewriting graph identity, clean generations,
  canonical relations, bounded traversal, or test attribution;
- stale-analyzer: superseded document versions or cancelled analyzer work are
  rejected before preview truth is minted;
- overlay-vs-clean: dirty overlay candidates cannot become durable preview
  truth unless saved or replanned against clean content;
- provenance-preserving dedupe: equivalent sites from graph and analyzer
  sources collapse only when provenance and disposition remain inspectable; and
- cross-project merge: canonical historical and cross-project relations from
  graph evidence remain authoritative while analyzer candidates add only
  active-document, version-reference, or dispatch evidence with independent
  provenance.

End-to-end gates:

1. Preview and `dry_run` make zero file changes.
2. Apply consumes the exact preview digest and rejects any stale precondition before writing.
3. A successful pure rename leaves no in-scope bound references to the old symbol.
4. A successful API migration leaves old names only at manifest-approved compatibility sites.
5. Stable-value assertions prove protected bytes are unchanged unless exact sites received explicit acknowledgement.
6. Every failure path proves full rollback by comparing all scoped file bytes and modes with their preimages.
7. Formatting, diagnostics, affected tests, and requested scoped verification pass before the receipt is committed.
8. MCP and CLI return the same typed manifest and semantic success/failure status.
9. Natural-language evals select pure rename for identity-preserving changes and API migration for provider-neutral promotion or compatibility work.
10. The implementation reuses canonical graph, edit, diagnostics, formatting, and catalog owners; acceptance rejects a copied resolver, replacement engine, transaction path, or duplicate tool definition.
11. Analyzer-candidate merge fixtures prove graph-only, analyzer-only, disagreement, stale-analyzer, overlay-vs-clean, provenance-preserving dedupe, and cross-project merge behavior; dirty overlay candidates never become durable preview truth without save or replan against clean content.
