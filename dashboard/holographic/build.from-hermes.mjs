/**
 * Build the holographic-memory dashboard plugin bundle for Hermes.
 *
 * This file remains as a compatibility entry point for Hermes workflows, but
 * the implementation delegates to the canonical dashboard Rsbuild/Tailwind
 * path in `../build.shared.mjs` so standalone, dev, and Hermes builds share
 * JSX, alias, CSS, and output behavior.
 */

import { dashboardRoot, buildHolographicPlugin } from "../build.shared.mjs";
import fs from "node:fs/promises";
import path from "node:path";

async function main() {
  await buildHolographicPlugin();
  const distDir = path.join(dashboardRoot, "holographic/dist");
  const [jsStat, cssStat] = await Promise.all([
    fs.stat(path.join(distDir, "index.js")),
    fs.stat(path.join(distDir, "style.css")),
  ]);
  console.log(`✓ dist/index.js  ${(jsStat.size / 1024).toFixed(1)} KB`);
  console.log(`✓ dist/style.css ${(cssStat.size / 1024).toFixed(1)} KB`);
}

main().catch((err) => {
  console.error(err);
  process.exitCode = 1;
});
