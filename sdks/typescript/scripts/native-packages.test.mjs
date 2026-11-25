import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");

test("native packages cover the release roster and retain the exact addon bytes", () => {
  const root = mkdtempSync(path.join(tmpdir(), "tracedecay-native-packages-"));
  try {
    const packageRoot = path.join(root, "sdks/typescript");
    mkdirSync(path.join(packageRoot, "scripts"), { recursive: true });
    mkdirSync(path.join(root, ".github"));
    for (const relative of ["sdks/typescript/package.json", ".github/release-targets.json", "sdks/typescript/scripts/native-packages.mjs"]) {
      copyFileSync(path.join(repository, relative), path.join(root, relative));
    }
    const script = path.join(packageRoot, "scripts/native-packages.mjs");
    const run = (...args) => execFileSync(process.execPath, [script, ...args], { stdio: "pipe" });
    const addon = path.join(root, "addon.node");
    writeFileSync(addon, Buffer.alloc(2 * 1024 * 1024, 0x61));
    const artifacts = path.join(root, "artifacts");
    const targets = JSON.parse(readFileSync(path.join(root, ".github/release-targets.json"))).include;
    const archives = [];
    for (const { target, name } of targets) {
      const staged = path.join(root, "stage", name, "package");
      run("stage", "--target", target, "--addon", addon, "--out", staged);
      const manifest = JSON.parse(readFileSync(path.join(staged, "package.json")));
      assert.equal(manifest.main, "tracedecay_sdk_node.node");
      assert.deepEqual(manifest.files, ["tracedecay_sdk_node.node"]);
      const artifact = path.join(artifacts, name);
      mkdirSync(artifact, { recursive: true });
      copyFileSync(addon, path.join(artifact, "tracedecay_sdk_node.node"));
      const archive = path.join(artifact, "platform.tgz");
      execFileSync("tar", ["-czf", archive, "-C", path.dirname(staged), "package"]);
      archives.push(archive);
    }
    run("verify", "--packages", artifacts);
    run("link");
    const linked = JSON.parse(readFileSync(path.join(packageRoot, "package.json")));
    assert.equal(Object.keys(linked.optionalDependencies).length, targets.length);
    assert.ok(Object.values(linked.optionalDependencies).every((version) => version === linked.version));

    writeFileSync(path.join(path.dirname(archives[0]), "tracedecay_sdk_node.node"), "different binary");
    assert.throws(() => run("verify", "--packages", artifacts));
    copyFileSync(addon, path.join(path.dirname(archives[0]), "tracedecay_sdk_node.node"));
    rmSync(archives[0]);
    assert.throws(() => run("verify", "--packages", artifacts), /missing native SDK packages/);
    writeFileSync(addon, "");
    assert.throws(() => run("stage", "--target", targets[0].target, "--addon", addon, "--out", path.join(root, "empty")), /missing or empty/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
