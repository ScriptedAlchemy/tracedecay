/**
 * Build every dashboard artifact served by `tracedecay dashboard`.
 *
 *   npm install && npm run build      (from dashboard/)
 *
 * Outputs:
 *   shell/dist/shell.js + shell.css   Standalone host shell.
 *   holographic/dist/index.js         Holographic-memory plugin bundle.
 *   graph/dist/index.js               Code graph explorer plugin bundle.
 *   savings/dist/index.js             Savings plugin bundle.
 *   lcm/dist/index.js + style.css     Copied from lcm/src.
 *   hermes-wrapper/dist/*             Combined Hermes dashboard plugin.
 *
 * The Rust binary embeds these dist files at compile time
 * (src/dashboard/assets.rs), so run this before `cargo build` when the UI
 * changed.
 */

import {
  buildHermesWrapper,
  buildHolographicPlugin,
  buildPlugin,
  buildShell,
  EMBEDDED_DIST_FILES,
  HERMES_WRAPPER_DIST_FILES,
  logBuiltFiles,
} from "./build.shared.mjs";

async function main() {
  await Promise.all([
    buildShell(),
    buildHolographicPlugin(),
    buildPlugin("graph", "code graph", { primitives: true }),
    buildPlugin("savings", "savings & cost", { primitives: true }),
    buildPlugin("lcm", "LCM", { primitives: true }),
  ]);
  await buildHermesWrapper();
  await logBuiltFiles([...EMBEDDED_DIST_FILES, ...HERMES_WRAPPER_DIST_FILES]);
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
