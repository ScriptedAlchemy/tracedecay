import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createRequire } from "node:module";
import { spawnSync } from "node:child_process";

const require = createRequire(import.meta.url);
const packageJson = require.resolve("@rsbuild/core/package.json");
const cli = path.resolve(path.dirname(packageJson), "bin/rsbuild.js");

// Independently rebuild twice: a cache hit cannot prove determinism.
const temporary = await mkdtemp(path.join(tmpdir(), "dashboard-determinism-"));
try {
  const outputs = [];
  const destination = path.join(temporary, "app-dist");
  for (let attempt = 0; attempt < 2; attempt++) {
    const result = spawnSync(process.execPath, [cli, "build"], {
      env: { ...process.env, TRACEDECAY_DASHBOARD_DIST_PATH: destination },
      stdio: "inherit",
    });
    if (result.error) throw result.error;
    assert.equal(result.status, 0, "dashboard build failed");
    const entries = await readdir(destination, { recursive: true, withFileTypes: true });
    const files = entries.filter((entry) => entry.isFile()).map((entry) =>
      path.relative(destination, path.join(entry.parentPath, entry.name))).sort();
    assert.ok(files.length > 0, "dashboard build emitted no files");
    outputs.push(await Promise.all(files.map(async (file) =>
      [file, createHash("sha256").update(await readFile(path.join(destination, file))).digest("hex")])));
  }
  assert.deepEqual(outputs[0], outputs[1], "dashboard builds must be byte-identical");
} finally {
  await rm(temporary, { recursive: true, force: true });
}
