#!/usr/bin/env node
// Stages the per-platform addon packages of @tracedecay/sdk, following the
// napi-rs release layout: one `@tracedecay/sdk-<platform>` package per release
// target, each holding `tracedecay_sdk_node.node`, referenced from the main
// package's optionalDependencies at the same version. Release targets come
// from `.github/release-targets.json`, the authority the daemon archives use.
//
//   native-packages.mjs stage --target <rust-triple> --addon <file> --out <dir>
//   native-packages.mjs link
//   native-packages.mjs verify --packages <artifact-directory>
//
// `stage` writes one platform package directory. `link` writes the main
// package's optionalDependencies at its current version; the release job runs
// it after the version bump and before `npm pack`, because unpublished
// platform packages cannot sit in the workspace manifest. Both are
// idempotent: rerunning converges on the same files.
import { copyFileSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repositoryRoot = path.resolve(packageRoot, "../..");
const ADDON = "tracedecay_sdk_node.node";

/** npm platform naming for each Rust target triple we release. */
function platformOf(target) {
  const known = {
    "aarch64-apple-darwin": { suffix: "darwin-arm64", os: "darwin", cpu: "arm64" },
    "x86_64-apple-darwin": { suffix: "darwin-x64", os: "darwin", cpu: "x64" },
    "x86_64-unknown-linux-gnu": { suffix: "linux-x64-gnu", os: "linux", cpu: "x64", libc: "glibc" },
    "aarch64-unknown-linux-gnu": { suffix: "linux-arm64-gnu", os: "linux", cpu: "arm64", libc: "glibc" },
    "x86_64-pc-windows-msvc": { suffix: "win32-x64-msvc", os: "win32", cpu: "x64" },
  };
  const platform = known[target];
  if (platform === undefined) throw new Error(`no npm platform mapping for release target ${target}`);
  return platform;
}

function releaseTargets() {
  const file = path.join(repositoryRoot, ".github/release-targets.json");
  return JSON.parse(readFileSync(file, "utf8")).include.map((entry) => entry.target);
}

function readJson(file) {
  return JSON.parse(readFileSync(file, "utf8"));
}

function writeJson(file, value) {
  writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function mainPackage() {
  return readJson(path.join(packageRoot, "package.json"));
}

function optionalDependencies(version) {
  return Object.fromEntries(
    releaseTargets()
      .map((target) => [`@tracedecay/sdk-${platformOf(target).suffix}`, version])
      .sort(([a], [b]) => a.localeCompare(b)),
  );
}

function verifyPackages(directory) {
  const expected = new Map(releaseTargets().map((target) => {
    const platform = platformOf(target);
    return [`@tracedecay/sdk-${platform.suffix}`, platform];
  }));
  const version = mainPackage().version;
  const filenames = readdirSync(directory, { recursive: true })
    .filter((filename) => filename.endsWith(".tgz"));
  for (const filename of filenames) {
    const tarball = path.join(directory, filename);
    const manifest = JSON.parse(execFileSync("tar", ["-xOf", tarball, "package/package.json"], { encoding: "utf8" }));
    const platform = expected.get(manifest.name);
    if (platform === undefined || manifest.version !== version || manifest.main !== ADDON
        || JSON.stringify(manifest.os) !== JSON.stringify([platform.os])
        || JSON.stringify(manifest.cpu) !== JSON.stringify([platform.cpu])
        || JSON.stringify(manifest.libc) !== JSON.stringify(platform.libc === undefined ? undefined : [platform.libc])) {
      throw new Error(`unexpected native SDK package: ${filename}`);
    }
    const addon = readFileSync(path.join(path.dirname(tarball), ADDON));
    const packed = execFileSync("tar", ["-xOf", tarball, `package/${ADDON}`], { maxBuffer: addon.length + 1 });
    if (packed.length === 0 || !packed.equals(addon)) {
      throw new Error(`native SDK package differs from the tested addon: ${filename}`);
    }
    expected.delete(manifest.name);
  }
  if (expected.size !== 0) throw new Error(`missing native SDK packages: ${[...expected.keys()].join(", ")}`);
}

function argument(args, name) {
  const index = args.indexOf(name);
  const value = index === -1 ? undefined : args[index + 1];
  if (value === undefined) throw new Error(`missing ${name}`);
  return value;
}

const [command, ...args] = process.argv.slice(2);
if (command === "stage") {
  const target = argument(args, "--target");
  if (!releaseTargets().includes(target)) throw new Error(`${target} is not a release target`);
  const platform = platformOf(target);
  const main = mainPackage();
  const out = path.resolve(argument(args, "--out"));
  const addon = path.resolve(argument(args, "--addon"));
  if (!statSync(addon).isFile() || statSync(addon).size === 0) throw new Error(`native SDK addon is missing or empty: ${addon}`);
  mkdirSync(out, { recursive: true });
  copyFileSync(addon, path.join(out, ADDON));
  writeJson(path.join(out, "package.json"), {
    name: `@tracedecay/sdk-${platform.suffix}`,
    version: main.version,
    description: `${main.name} native binding for ${target}`,
    license: main.license,
    repository: main.repository,
    main: ADDON,
    files: [ADDON],
    os: [platform.os],
    cpu: [platform.cpu],
    ...(platform.libc === undefined ? {} : { libc: [platform.libc] }),
    engines: main.engines,
    publishConfig: main.publishConfig,
  });
} else if (command === "link") {
  const file = path.join(packageRoot, "package.json");
  const main = mainPackage();
  writeJson(file, { ...main, optionalDependencies: optionalDependencies(main.version) });
} else if (command === "verify") {
  verifyPackages(path.resolve(argument(args, "--packages")));
} else {
  throw new Error("usage: native-packages.mjs stage --target <triple> --addon <file> --out <dir> | link | verify --packages <dir>");
}
