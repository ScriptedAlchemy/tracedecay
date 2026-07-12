<!-- Generated from architecture-boundaries.toml; do not edit. -->
# V2 Architecture Owners

| Owner | Kind | Target | Tier | Normative plan |
|---|---|---|---:|---|
| domain | Rust package | `crates/tracedecay-domain` | 1 | plan 01 |
| store | Rust package | `crates/tracedecay-store` | 2 | plan 02 |
| capture | Rust package | `crates/tracedecay-capture` | 2 | plan 03 |
| projectors | Rust package | `crates/tracedecay-projectors` | 2 | plan 04 |
| code-index | Rust package | `crates/tracedecay-code-index` | 2 | plan 25 |
| query | Rust package | `crates/tracedecay-query` | 3 | plan 05 |
| policy | Rust package | `crates/tracedecay-policy` | 3 | plan 06 |
| tool-catalog | Rust package | `crates/tracedecay-tool-catalog` | 2 | plan 08 |
| application | Rust package | `crates/tracedecay-application` | 4 | plan 09 |
| client-rust | Rust package | `crates/tracedecay-client` | 5 | plan 17 |
| root | Rust package | `.` | 5 | plan 12 |
| public-contracts | Generated contract | `contracts/api` | 4 | plan 17 |
| api / hooks / presentation / host-deploy / remote-brain-transport | Root-private modules | `src/v2/*` | 5 | plans 07/10/21/27/28 |
| client-typescript / client-python | Public packages | `packages/`, `python/` | 5 | plan 17 |
| dashboard | Frontend | `dashboard` | 6 | plan 11 |

The Rust package count is capped at eleven. Root-private adapters are linted boundaries and are not package-admission precedents.