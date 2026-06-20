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
 * React: prod plugin bundles alias `react` to the host SDK shim. Dev keeps the
 * real React module so react-dom/client can create the root; one Rsbuild graph
 * already shares that instance across all imported plugins.
 */

import { createRsbuild } from "@rsbuild/core";
import path from "node:path";
import { fileURLToPath } from "node:url";
import {
  compileHolographicTailwindCss,
  createDashboardDevConfig,
} from "../build.shared.mjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const dashboardRoot = path.resolve(__dirname, "..");

const apiTarget = process.env.TRACEDECAY_DEV_API || "http://127.0.0.1:7341";
const port = Number(process.env.TRACEDECAY_DEV_PORT || 7342);
const host = "127.0.0.1";

await compileHolographicTailwindCss();

const rsbuildConfig = createDashboardDevConfig({ apiTarget, host, port });

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
