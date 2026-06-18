#!/usr/bin/env node
/**
 * tracedecay dashboard — frontend dev server (Rsbuild + HMR).
 *
 * Run from the repo root or the dashboard/ dir:
 *
 *   node dashboard/dev/run.mjs
 *   TRACEDECAY_DEV_PORT=8080 node dashboard/dev/run.mjs
 *   TRACEDECAY_DEV_API=http://127.0.0.1:7341 node dashboard/dev/run.mjs
 *
 * Env:
 *   TRACEDECAY_DEV_API   backend `tracedecay dashboard` base URL to proxy
 *                        /api/* to. Default: http://127.0.0.1:7341
 *   TRACEDECAY_DEV_PORT  port for this dev server. Default: 7342
 *
 * On success prints a stable, parseable line on stdout (mirrors the prod
 * server's announcement so wrappers can scrape it):
 *
 *   tracedecay dev listening on http://127.0.0.1:7342/
 *
 * REACT EXTERNALIZATION (dev/prod divergence):
 * In prod, each plugin bundle aliases `react` → a window-SDK shim so separate
 * bundles share one React. The dev server does NOT set that alias: the dev
 * entry uses react-dom/client (createRoot), whose internals read private
 * symbols straight off the real `react` module; aliasing `react` to a shim
 * namespace breaks react-dom. A single Rsbuild bundle already shares one React
 * instance, so the shim is unnecessary. main.tsx instead puts real React +
 * hooks + components + utils + fetchJSON on window.__HERMES_PLUGIN_SDK__
 * before any plugin entry runs, so every SDK consumer behaves like prod.
 */

import { createRsbuild } from "@rsbuild/core";
import { pluginReact } from "@rsbuild/plugin-react";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const dashboardRoot = path.resolve(__dirname, "..");

const apiTarget = process.env.TRACEDECAY_DEV_API || "http://127.0.0.1:7341";
const port = Number(process.env.TRACEDECAY_DEV_PORT || 7342);
const host = "127.0.0.1";

const rsbuildConfig = {
  root: dashboardRoot,
  source: {
    entry: { index: "./dev/main.tsx" },
  },
  html: {
    template: "./dev/index.html",
    title: "tracedecay dashboard (dev)",
  },
  // NOTE (dev/prod divergence): holographic's `@import "tailwindcss"` (Tailwind
  // v4) is NOT compiled in this dev server. In THIS execution environment BOTH
  // Tailwind-v4 integrations Rsbuild documents make createRsbuild() segfault
  // (native crash, no stdout/stderr output): `@rsbuild/plugin-tailwindcss` AND
  // `@tailwindcss/postcss` wired through `tools.postcss`. The earlier
  // `pluginReact()`-only dev server started fine, and `@rspack/core` works here
  // (the prod build uses it), so the Tailwind native code path is the trigger.
  // To keep the dev server usable (HMR + every non-holographic plugin styled),
  // we ship NO Tailwind pipeline here. The holographic plugin will render
  // unstyled in dev. The prod build (`npm run build`) is the source of truth
  // for holographic's Tailwind styles — verify holographic styling there.
  server: {
    host,
    port,
    proxy: {
      // All dashboard data calls go to /api/* on a running `tracedecay
      // dashboard` (default 127.0.0.1:7341). Plugins are imported by the dev
      // bundle, so /dashboard-plugins is NOT proxied.
      "/api": {
        target: apiTarget,
        changeOrigin: true,
      },
    },
  },
  plugins: [pluginReact()],
};

const rsbuild = await createRsbuild({ cwd: dashboardRoot, rsbuildConfig });

const handle = await rsbuild.startDevServer();

// startDevServer returns { server, port, urls, close }. rsbuild keeps the
// requested port when free; fall back to it if the field is absent.
const actualPort = (handle && typeof handle.port === "number" && handle.port) || port;
const url = `http://${host}:${actualPort}/`;
console.log(`tracedecay dev listening on ${url}`);
console.log(`tracedecay dev proxying /api -> ${apiTarget}`);

function shutdown() {
  Promise.resolve(handle && typeof handle.close === "function" ? handle.close() : undefined)
    .catch(() => {})
    .finally(() => process.exit(0));
}
process.on("SIGINT", shutdown);
process.on("SIGTERM", shutdown);
