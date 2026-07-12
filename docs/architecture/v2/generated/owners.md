<!-- Generated from architecture-boundaries.toml; do not edit. -->
# V2 Architecture Owners

| Owner | Kind | Target | Tier | Public facade | Normative plan |
|---|---|---|---:|---|---|
| domain | Rust Package | `crates/tracedecay-domain` | 1 | `private` | `docs/plans/tracedecay-v2/01-domain-crate.md` |
| store | Rust Package | `crates/tracedecay-store` | 2 | `private` | `docs/plans/tracedecay-v2/02-store-crate.md` |
| capture | Rust Package | `crates/tracedecay-capture` | 2 | `private` | `docs/plans/tracedecay-v2/03-capture-crate.md` |
| projectors | Rust Package | `crates/tracedecay-projectors` | 2 | `private` | `docs/plans/tracedecay-v2/04-projectors-crate.md` |
| code-index | Rust Package | `crates/tracedecay-code-index` | 2 | `private` | `docs/plans/tracedecay-v2/25-code-intelligence-indexing-crate.md` |
| query | Rust Package | `crates/tracedecay-query` | 3 | `private` | `docs/plans/tracedecay-v2/05-query-crate.md` |
| policy | Rust Package | `crates/tracedecay-policy` | 3 | `private` | `docs/plans/tracedecay-v2/06-policy-crate.md` |
| tool-catalog | Rust Package | `crates/tracedecay-tool-catalog` | 2 | `private` | `docs/plans/tracedecay-v2/08-tool-catalog-crate.md` |
| application | Rust Package | `crates/tracedecay-application` | 4 | `private` | `docs/plans/tracedecay-v2/09-application-crate.md` |
| client-rust | Rust Package | `crates/tracedecay-client` | 5 | `private` | `docs/plans/tracedecay-v2/17-official-public-api-and-sdks.md` |
| root | Rust Package | `.` | 5 | `private` | `docs/plans/tracedecay-v2/12-root-compatibility-migration.md` |
| public-contracts | Generated Contract | `contracts/api` | 4 | `private` | `docs/plans/tracedecay-v2/17-official-public-api-and-sdks.md` |
| internal-node-contracts | Generated Contract | `contracts/node` | 4 | `private` | `docs/plans/tracedecay-v2/10-api-crate.md` |
| api | Root Private Module | `src/v2/api` | 5 | `private` | `docs/plans/tracedecay-v2/10-api-crate.md` |
| hooks | Root Private Module | `src/v2/hooks` | 5 | `private` | `docs/plans/tracedecay-v2/07-hooks-crate.md` |
| presentation | Root Private Module | `src/v2/presentation` | 5 | `private` | `docs/plans/tracedecay-v2/21-cli-mcp-tool-surface-and-output-unification.md` |
| host-deploy | Root Private Module | `src/v2/host_deploy` | 5 | `private` | `docs/plans/tracedecay-v2/27-cross-host-agent-plugin-bundles.md` |
| native-semantic-runtime | Root Private Module | `src/v2/native_semantic_runtime` | 5 | `private` | `docs/plans/tracedecay-v2/31-native-fastembed-semantic-code-search.md` |
| remote-brain-transport | Root Private Module | `src/v2/remote_brain_transport` | 5 | `private` | `docs/plans/tracedecay-v2/28-remote-multi-machine-shared-brain.md` |
| client-typescript | Public Package | `packages/tracedecay-client` | 5 | `private` | `docs/plans/tracedecay-v2/17-official-public-api-and-sdks.md` |
| client-python | Public Package | `python/tracedecay-client` | 5 | `private` | `docs/plans/tracedecay-v2/17-official-public-api-and-sdks.md` |
| dashboard | Frontend | `dashboard` | 6 | `private` | `docs/plans/tracedecay-v2/11-dashboard-frontend.md` |

Rust packages are capped at 11. Root-private adapters remain module-lint boundaries, not package-admission precedents.
