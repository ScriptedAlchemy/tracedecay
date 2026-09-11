import type audit from "./github-workload.json";

type AuditWorkload = typeof audit;
type PullRequestContext = Omit<AuditWorkload["prs"][number], "headSha" | "headObservedAt" | "headSourceUrl"> & {
  headSha: string | null;
  headObservedAt: string | null;
  headSourceUrl: string | null;
};
// A manual branch enrollment is the primary entity. A matching GitHub head
// supplies PR context; it does not establish PR-autotrack enrollment.
export type TrackedBranch = {
  kind: "branch";
  id: string;
  trackedRef: string;
  repository: string;
  observedAt: string;
  sourceEvidence: string[];
  footprint?: {
    comparison: "base-to-head";
    baseOid: string;
    headOid: string;
    observedAt: string;
    sourceEvidence: string[];
    files: {
      path: string;
      previousPath?: string;
      status: string;
      additions: number | null;
      deletions: number | null;
    }[];
  };
  // Field names retain the canonical BranchEntry.graph_source provenance.
  graphSource: {
    publication_epoch: number;
    project_id: string;
    repository_id: string;
    worktree_id: string;
    worktree_root: string;
    reference: string;
    source_oid: string;
  };
  prContext: {
    enrollment: "head-matched-context";
    id: string;
    repo: string;
    number: number;
    url: string;
    headRef: string;
    headSha: string;
    observedAt: string;
  } | null;
};
export type TrackingCoverage = {
  state: "available" | "partial" | "unavailable";
  observedAt: string | null;
  reason: string;
  evidence: { source: string; finding: string }[];
};

// Registration, GitHub authorship, and a local Git ref are not admission.
// A served record needs TraceDecay project + tracked ref/PR identity bound to
// an indexed head. Partial indexing may qualify when that binding is served.
export const trackingCoverage: TrackingCoverage = {
  "state": "partial",
  "observedAt": "2026-09-09T03:55:04.878190Z",
  "reason": "One indexed Rspack branch is captured with exact-head-matched PR context. Other projects and tracked references are not yet covered. Manual branch enrollment does not establish PR-autotrack enrollment.",
  "evidence": [
    {
      "source": "/fast/projects/td-visual-review/implementation/runtime-refresh/rspack-branch-receipt.json",
      "finding": "All seven graph_source fields are present; publication epoch 1 and indexed source_oid match the provider PR head."
    },
    {
      "source": "/fast/projects/td-visual-review/implementation/runtime-refresh/rspack-status-current.json",
      "finding": "Index current, complete, graph-ready and fresh; no rebuild in flight. Observation time is the status capture file time, not a provider event."
    },
    {
      "source": "/fast/projects/td-visual-review/implementation/runtime-refresh/sample-pr-context-refresh.json",
      "finding": "Separately observed PR #12977 repository, branch and head match the indexed source."
    }
  ]
};

// Type-only audit import keeps the broad account dataset out of this payload.
// Relationships require admitted endpoints; unindexed context cannot seed a graph.
export const externalRelations: AuditWorkload["relations"] = [];
export const trackedRepositories: string[] = ["web-infra-dev/rspack"];
export const trackedBranches: TrackedBranch[] = [
  {
    "kind": "branch",
    "id": "proj_175ebc47e48f0ec9:refs/remotes/origin/feat/mf-layers",
    "trackedRef": "refs/remotes/origin/feat/mf-layers",
    "repository": "web-infra-dev/rspack",
    "observedAt": "2026-09-09T03:55:04.878190Z",
    "sourceEvidence": [
      "/fast/projects/td-visual-review/implementation/runtime-refresh/rspack-branch-receipt.json",
      "/fast/projects/td-visual-review/implementation/runtime-refresh/rspack-status-current.json",
      "/fast/projects/td-visual-review/implementation/runtime-refresh/sample-pr-context-refresh.json"
    ],
    footprint: {
      "comparison": "base-to-head",
      "baseOid": "ba36b0ad179a7874d142b20a43c87f268df7d652",
      "headOid": "ecd6feb8cec8a3f936947d35f041271691e735b5",
      "observedAt": "2026-09-09T04:19:26.994950Z",
      "sourceEvidence": [
        "/fast/projects/rspack: git diff --no-ext-diff --no-textconv --find-renames --name-status/--numstat -z ba36b0ad179a7874d142b20a43c87f268df7d652 ecd6feb8cec8a3f936947d35f041271691e735b5",
        "/fast/projects/td-visual-review/implementation/runtime-refresh/sample-pr-context-refresh.json",
        "/fast/projects/td-visual-review/implementation/runtime-refresh/rspack-branch-receipt.json"
      ],
      "files": [
        {
          "path": "Cargo.lock",
          "status": "M",
          "additions": 9,
          "deletions": 0
        },
        {
          "path": "crates/node_binding/napi-binding.d.ts",
          "status": "M",
          "additions": 12,
          "deletions": 0
        },
        {
          "path": "crates/rspack_binding_api/src/raw_options/raw_builtins/raw_mf.rs",
          "status": "M",
          "additions": 33,
          "deletions": 0
        },
        {
          "path": "crates/rspack_plugin_mf/src/container/container_entry_dependency.rs",
          "status": "M",
          "additions": 27,
          "deletions": 4
        },
        {
          "path": "crates/rspack_plugin_mf/src/container/container_entry_module.rs",
          "status": "M",
          "additions": 28,
          "deletions": 7
        },
        {
          "path": "crates/rspack_plugin_mf/src/container/container_entry_module_factory.rs",
          "status": "M",
          "additions": 6,
          "deletions": 0
        },
        {
          "path": "crates/rspack_plugin_mf/src/lib.rs",
          "status": "M",
          "additions": 70,
          "deletions": 4
        },
        {
          "path": "crates/rspack_plugin_mf/src/manifest/mod.rs",
          "status": "M",
          "additions": 22,
          "deletions": 7
        },
        {
          "path": "crates/rspack_plugin_mf/src/manifest/utils.rs",
          "status": "M",
          "additions": 0,
          "deletions": 22
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/collect_shared_entry_plugin.rs",
          "status": "M",
          "additions": 155,
          "deletions": 71
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/consume_shared_fallback_dependency.rs",
          "status": "M",
          "additions": 8,
          "deletions": 2
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/consume_shared_module.rs",
          "status": "M",
          "additions": 74,
          "deletions": 18
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/consume_shared_plugin.rs",
          "status": "M",
          "additions": 71,
          "deletions": 43
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/consume_shared_runtime_module.rs",
          "status": "M",
          "additions": 26,
          "deletions": 3
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/mod.rs",
          "status": "M",
          "additions": 49,
          "deletions": 1
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/provide_shared_dependency.rs",
          "status": "M",
          "additions": 27,
          "deletions": 11
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/provide_shared_module.rs",
          "status": "M",
          "additions": 42,
          "deletions": 7
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/provide_shared_module_factory.rs",
          "status": "M",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/provide_shared_plugin.rs",
          "status": "M",
          "additions": 175,
          "deletions": 81
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/share_runtime_module.rs",
          "status": "M",
          "additions": 25,
          "deletions": 1
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/shared_container_plugin.rs",
          "status": "M",
          "additions": 8,
          "deletions": 0
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/shared_used_exports_optimizer_plugin.rs",
          "status": "M",
          "additions": 216,
          "deletions": 147
        },
        {
          "path": "crates/rspack_plugin_mf/src/sharing/shared_used_exports_optimizer_runtime_module.rs",
          "status": "M",
          "additions": 34,
          "deletions": 24
        },
        {
          "path": "crates/rspack_plugin_mf_test/Cargo.toml",
          "status": "A",
          "additions": 16,
          "deletions": 0
        },
        {
          "path": "crates/rspack_plugin_mf_test/tests/identifiers.rs",
          "status": "A",
          "additions": 105,
          "deletions": 0
        },
        {
          "path": "packages/rspack/src/container/ModuleFederationPlugin.ts",
          "status": "M",
          "additions": 26,
          "deletions": 3
        },
        {
          "path": "packages/rspack/src/exports.ts",
          "status": "M",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "packages/rspack/src/runtime/moduleFederationDefaultRuntime.js",
          "status": "M",
          "additions": 60,
          "deletions": 11
        },
        {
          "path": "packages/rspack/src/sharing/CollectSharedEntryPlugin.ts",
          "status": "M",
          "additions": 29,
          "deletions": 8
        },
        {
          "path": "packages/rspack/src/sharing/ConsumeSharedPlugin.ts",
          "status": "M",
          "additions": 105,
          "deletions": 32
        },
        {
          "path": "packages/rspack/src/sharing/IndependentSharedPlugin.ts",
          "status": "M",
          "additions": 378,
          "deletions": 141
        },
        {
          "path": "packages/rspack/src/sharing/ProvideSharedPlugin.ts",
          "status": "M",
          "additions": 29,
          "deletions": 6
        },
        {
          "path": "packages/rspack/src/sharing/SharePlugin.ts",
          "status": "M",
          "additions": 90,
          "deletions": 22
        },
        {
          "path": "packages/rspack/src/sharing/SharedContainerPlugin.ts",
          "status": "M",
          "additions": 19,
          "deletions": 3
        },
        {
          "path": "packages/rspack/src/sharing/SharedUsedExportsOptimizerPlugin.ts",
          "status": "M",
          "additions": 27,
          "deletions": 3
        },
        {
          "path": "packages/rspack/src/sharing/TreeShakingSharedPlugin.ts",
          "status": "M",
          "additions": 11,
          "deletions": 2
        },
        {
          "path": "packages/rspack/src/sharing/utils.ts",
          "status": "M",
          "additions": 21,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-direct-consume-validation/errors.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-direct-consume-validation/index.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-direct-consume-validation/rspack.config.js",
          "status": "A",
          "additions": 15,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-direct-provide-validation/errors.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-direct-provide-validation/index.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-direct-provide-validation/rspack.config.js",
          "status": "A",
          "additions": 15,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-share-plugin-validation/errors.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-share-plugin-validation/index.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/enhanced-option-share-plugin-validation/rspack.config.js",
          "status": "A",
          "additions": 15,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/share-scope-array-direct-plugin-validation/errors.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/share-scope-array-direct-plugin-validation/index.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/share-scope-array-direct-plugin-validation/rspack.config.js",
          "status": "A",
          "additions": 13,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/share-scope-array-direct-provide-validation/errors.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/share-scope-array-direct-provide-validation/index.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-0/share-scope-array-direct-provide-validation/rspack.config.js",
          "status": "A",
          "additions": 13,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/manifest/index.js",
          "status": "M",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/manifest/rspack.config.js",
          "status": "M",
          "additions": 3,
          "deletions": 1
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/relative-layered-consume-issuer-context/index.js",
          "status": "A",
          "additions": 16,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/relative-layered-consume-issuer-context/nested/consumer.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/relative-layered-consume-issuer-context/nested/shared.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/relative-layered-consume-issuer-context/rspack.config.js",
          "status": "A",
          "additions": 35,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-module-id-collisions/index.js",
          "status": "A",
          "additions": 15,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-module-id-collisions/rspack.config.js",
          "status": "A",
          "additions": 47,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-module-id-collisions/shared.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-request-defaults/index.js",
          "status": "A",
          "additions": 28,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-request-defaults/node_modules/lib/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-request-defaults/node_modules/lib/package.json",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-request-defaults/node_modules/other-impl/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-request-defaults/node_modules/other-impl/package.json",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/shared-request-defaults/rspack.config.js",
          "status": "A",
          "additions": 39,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/consumer.js",
          "status": "A",
          "additions": 9,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/index.js",
          "status": "A",
          "additions": 29,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/overlap/deep/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/overlap/deep/long-sub.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/overlap/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/overlap/package.json",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/deep/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/deep/sub.js",
          "status": "A",
          "additions": 2,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/disabled.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/disabled/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/disabled/sub.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/exact.js",
          "status": "A",
          "additions": 2,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/index.js",
          "status": "A",
          "additions": 2,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/package.json",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/node_modules/prefix/sub.js",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/rspack.config.js",
          "status": "A",
          "additions": 58,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-prefix-exports/server.js",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/index.js",
          "status": "A",
          "additions": 32,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg-a/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg-a/package.json",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg-b/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg-b/package.json",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg/package.json",
          "status": "A",
          "additions": 5,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/node_modules/pkg/sub.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/query-loader.js",
          "status": "A",
          "additions": 3,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/container-1-5/tree-shaking-shared-request-origins/rspack.config.js",
          "status": "A",
          "additions": 77,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/duplicate-identity-exports/first.js",
          "status": "A",
          "additions": 2,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/duplicate-identity-exports/index.js",
          "status": "A",
          "additions": 6,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/duplicate-identity-exports/rspack.config.js",
          "status": "A",
          "additions": 23,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/duplicate-identity-exports/second.js",
          "status": "A",
          "additions": 2,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/first.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/index.js",
          "status": "A",
          "additions": 31,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/node_modules/b)c.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/node_modules/c.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/node_modules/package/button.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/node_modules/package/feature/button.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/node_modules/package/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/node_modules/package/package.json",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/rspack.config.js",
          "status": "A",
          "additions": 46,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/layered-provider-matching/second.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/provide-discovered-during-add-include/index.js",
          "status": "A",
          "additions": 9,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/provide-discovered-during-add-include/node_modules/package/index.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/provide-discovered-during-add-include/node_modules/package/package.json",
          "status": "A",
          "additions": 4,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/provide-discovered-during-add-include/rspack.config.js",
          "status": "A",
          "additions": 41,
          "deletions": 0
        },
        {
          "path": "tests/rspack-test/configCases/sharing/provide-discovered-during-add-include/unused-provider.js",
          "status": "A",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/type-tests/resolution-bundler/index.ts",
          "status": "M",
          "additions": 1,
          "deletions": 0
        },
        {
          "path": "tests/type-tests/resolution-bundler/module-federation.ts",
          "status": "A",
          "additions": 66,
          "deletions": 0
        },
        {
          "path": "website/docs/en/plugins/module-federation-plugin.mdx",
          "status": "M",
          "additions": 6,
          "deletions": 0
        },
        {
          "path": "website/docs/zh/plugins/module-federation-plugin.mdx",
          "status": "M",
          "additions": 6,
          "deletions": 0
        }
      ]
    },
    "graphSource": {
      "publication_epoch": 1,
      "project_id": "proj_175ebc47e48f0ec9",
      "repository_id": "repository.daemon.d2764e12cacc0181dfd30b1ab938ac48533ccd71e4a8dcbdd939edbe3af3f59a",
      "worktree_id": "worktree.daemon.81d660b5d945cb3fa1e3a74fc7658b0378d69e9ffb3ee641cfc8a655fa700581",
      "worktree_root": "/home/zack/.tracedecay/projects/proj_175ebc47e48f0ec9/branch-worktrees/ee165d07bf0b5b6ffdc379c800b5fb1a957cff459233a0cbcf2752ee654dd14f",
      "reference": "refs/heads/tracedecay/track/refs/remotes/origin/feat/mf-layers",
      "source_oid": "ecd6feb8cec8a3f936947d35f041271691e735b5"
    },
    "prContext": {
      "enrollment": "head-matched-context",
      "id": "web-infra-dev/rspack#12977",
      "repo": "web-infra-dev/rspack",
      "number": 12977,
      "url": "https://github.com/web-infra-dev/rspack/pull/12977",
      "headRef": "feat/mf-layers",
      "headSha": "ecd6feb8cec8a3f936947d35f041271691e735b5",
      "observedAt": "2026-09-09T03:37:33.985860Z"
    }
  }
];

const workload: Omit<AuditWorkload, "counts" | "prs"> & {
  prs: PullRequestContext[];
  counts: { [Key in keyof AuditWorkload["counts"]]: number | null };
  trackingCoverage: TrackingCoverage;
  branches: TrackedBranch[];
  trackedBranchCount: number | null;
  prContextCount: number | null;
} = {
  schemaVersion: 1,
  capturedAt: "2026-09-09T03:55:04.878190Z",
  window: { start: "2026-09-01T21:18:05Z", end: "2026-09-08T21:18:05Z" },
  source: {
    author: "ScriptedAlchemy",
    queries: [],
    limitations: [
      "This is a bounded indexed-branch sample, not the full tracked workload. Overall PR and weekly totals remain unknown.",
      "The window retains the earlier requested audit interval; capturedAt is the current TraceDecay status export observation.",
      "PR metadata is separately timestamped, exact-head-matched context. It does not establish canonical PR-autotrack enrollment.",
      "Review activity and check outcomes are not included in this UI payload; their absence does not establish healthy or unreviewed work.",
      "The broad GitHub author-search audit remains offline and cannot seed delivery nodes, attention candidates, or relationships."
    ],
  },
  counts: { created: null, merged: null, open: null, weeklyUnique: null, openRepos: null, cohortUnique: null },
  prs: [
    {
      "id": "web-infra-dev/rspack#12977",
      "repo": "web-infra-dev/rspack",
      "number": 12977,
      "url": "https://github.com/web-infra-dev/rspack/pull/12977",
      "title": "feat(mf): add layer-aware shared module core",
      "state": "open",
      "createdAt": "2026-02-07T04:39:02Z",
      "updatedAt": "2026-09-09T03:32:09Z",
      "closedAt": null,
      "mergedAt": null,
      "draft": false,
      "cohorts": [
        "open"
      ],
      "headSha": "ecd6feb8cec8a3f936947d35f041271691e735b5",
      "headObservedAt": "2026-09-09T03:37:33.985860Z",
      "headSourceUrl": "https://github.com/web-infra-dev/rspack/pull/12977"
    }
  ],
  relations: [],
  reviewActivity: [],
  relationDirections: {
    prerequisite: "from depends on the explicit previous stack slice or upstream support to",
    companion: "from explicitly pairs with to; no merge ordering implied",
    consumer: "from is upstream of the named consumer to",
    precedent: "from names to as an upstream precedent",
    withdrawn: "from names to as a withdrawn workaround",
    consolidation: "from points to the consolidated upstream version to",
  },
  trackingCoverage,
  branches: trackedBranches,
  trackedBranchCount: trackedBranches.length,
  prContextCount: trackedBranches.filter((branch) => branch.prContext !== null).length,
};

export default workload;
