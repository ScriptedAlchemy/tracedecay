# TraceDecay V2 rewrite

Status: active product rewrite.

## What exists

- `tracedecay-domain` contains the first executable V2 foundation: versioned domain and research contracts.
- The root integration test keeps a small, direct research-anchor contract.
- Existing runtime Doctor, daemon, storage, hooks, MCP, and CLI behavior remain product code. They are not replaced by inventories or plan metadata.

## What was removed

- The compatibility-inventory binary and production module.
- Generated architecture views, policy generators, source/YAML parsers, snapshot envelopes, and receipt catalogs.
- Abandoned evidence/privacy-corpus infrastructure and scanner-specific CI lanes.
- Agent skills and large Markdown checklists for executing the rewrite plan.
- Plan parsers, workflow executors, and incremental-PR orchestration artifacts.

Those systems modeled the rewrite instead of delivering it. They are intentionally not part of V2.

## Delivery rule

Each rewrite change must ship executable product behavior and direct tests of that behavior. Do not add a second metadata model of the product, generated plan views, or CI that validates planning artifacts.

See [NEXT.md](NEXT.md) for the next implementation slice.
