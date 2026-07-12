<!-- Generated from architecture-boundaries.toml; do not edit. -->
# V2 Dependency DAG

```mermaid
flowchart TD
  D[domain]
  S[store] --> D
  C[capture] --> D
  P[projectors] --> D
  CI[code-index] --> D
  Q[query] --> D
  POL[policy] --> D
  CAT[tool-catalog] --> D
  APP[application] --> D
  APP --> S
  APP --> C
  APP --> P
  APP --> Q
  APP --> POL
  APP --> CAT
  CONTRACT[public-contracts]
  API[api] --> APP
  API --> CONTRACT
  HOOKS[hooks] --> APP
  PRESENT[presentation] --> APP
  DEPLOY[host-deploy] --> CAT
  REMOTE[remote-brain-transport] --> APP
  RUSTCLIENT[client-rust] --> CONTRACT
  TS[client-typescript] --> CONTRACT
  PY[client-python] --> CONTRACT
  UI[dashboard] --> TS
  ROOT[root] --> APP
  ROOT --> API
  ROOT --> HOOKS
  ROOT --> PRESENT
  ROOT --> DEPLOY
  ROOT --> REMOTE
```

An arrow means “imports/depends on.” Transport nodes may cross only generated contracts and the application/domain/catalog boundary declared in the manifest. They may not import concrete store, query, policy, capture, projector, or code-index implementations.