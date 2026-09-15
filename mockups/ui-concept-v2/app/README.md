# TraceDecay concept application

This is the complete existing React/Rsbuild concept application imported from
`ScriptedAlchemy/td-brain-demo` at `727a9096db4c5053a67d04a03d1377e41f7470dd`.
Continue its implementation here. The original fourteen workspaces, navigation,
view states, recorded profile exports, fixture scenarios and checks are retained.

The [parent concept folder](../README.md) owns the reconciled briefs and design
plates. There is no second lookbook copy in this application. Build on the existing
components in `src/`; do not replace the app with independent HTML screen rebuilds.

```sh
npm ci
npm run dev
# http://127.0.0.1:5195/

npm run build
npm run typecheck
```

The build is emitted to ignored `dist/` with relative assets and can also be
served under this folder by a static HTTP server. Dependencies and build output
are not source artifacts.

## Existing screens

Use `?surface=brain|explorer|loom|sessions|agents|code|knowledge|delivery|automations|observatory|costs|settings|work|workflows`.
Loom and Delivery use `state=01`, `02`, etc. Brain retains its overview, hover,
repository zoom, scoped, synapse, firing-tree and neuron-lab views. The structural
atlas remains accessible in recorded Brain, including churn and exact duplicates.

- `data=snapshot` is the default: recorded exports with honest unavailable states.
- `data=fixture` opens labeled authored scenarios and local interactions.
- Dense Loom: `?surface=loom&state=04&data=fixture&loom_source=design&loom_page=full`.
- Proximity: add `&loom_lens=proximity` to the dense Loom URL.

`profile-pack/`, `src/structure/snapshot.json` and other imported JSON retain the
app's recorded data. They are not a live daemon connection. Delivery admits only
tracked/indexed evidence, not the broad offline GitHub audit. Original source
paths inside captures are provenance, not new filesystem dependencies.

## Images and checks

[Application screenshots](../screenshots/README.md) are separate from the
[design plates](../GALLERY.md). Refresh captures from this directory:

```sh
BASE_URL=http://127.0.0.1:5195 node scripts/capture-ui.mjs
BASE_URL=http://127.0.0.1:5195 node qa/surface-shell.mjs
BASE_URL=http://127.0.0.1:5195 node qa/workspace.mjs
BASE_URL=http://127.0.0.1:5195 node qa/loom-proximity.mjs
```

Set `OUTPUT_DIR` to keep temporary captures out of the final screenshot collection.
