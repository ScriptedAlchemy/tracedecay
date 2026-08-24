import { readdirSync, statSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const root = dirname(fileURLToPath(import.meta.url));
const testRoot = join(root, "test");

function collectTests(dir) {
  const out = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    const stat = statSync(full);
    if (stat.isDirectory()) {
      out.push(...collectTests(full));
      continue;
    }
    if (entry.endsWith(".test.mjs")) {
      out.push(relative(root, full));
    }
  }
  return out.sort();
}

const tests = collectTests(testRoot);
if (!tests.length) {
  console.error("No unit tests found under dashboard/test");
  process.exit(1);
}

const passthrough = process.argv.slice(2);
const result = spawnSync(process.execPath, ["--test", ...tests, ...passthrough], {
  cwd: root,
  stdio: "inherit",
});
process.exit(result.status ?? 1);
