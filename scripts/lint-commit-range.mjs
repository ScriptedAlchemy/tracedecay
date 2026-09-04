#!/usr/bin/env node

import { execFileSync, spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.dirname(scriptDirectory);
const require = createRequire(import.meta.url);
const commitlintPackage = require.resolve("@commitlint/cli/package.json");
const requireFromCommitlint = createRequire(commitlintPackage);
const loadModule = await import(
  pathToFileURL(requireFromCommitlint.resolve("@commitlint/load")).href
);
const lintModule = await import(
  pathToFileURL(requireFromCommitlint.resolve("@commitlint/lint")).href
);
const load = loadModule.default;
const lint = lintModule.default;

function usage() {
  console.error("usage: scripts/lint-commit-range.sh <base> <head>");
  process.exit(2);
}

function parseArguments(arguments_) {
  if (
    arguments_.length !== 4 ||
    arguments_[0] !== "--repository" ||
    !arguments_[1] ||
    !arguments_[2] ||
    !arguments_[3]
  ) {
    usage();
  }
  return {
    repository: path.resolve(arguments_[1]),
    base: arguments_[2],
    head: arguments_[3],
  };
}

function readCommitMessages(repository, commitShas) {
  if (commitShas.length === 0) {
    return [];
  }

  const batch = spawnSync(
    "git",
    ["-C", repository, "cat-file", "--batch"],
    {
      input: `${commitShas.join("\n")}\n`,
      maxBuffer: 1024 * 1024 * 256,
    },
  );
  if (batch.error) {
    throw batch.error;
  }
  if (batch.status !== 0) {
    throw new Error(batch.stderr.toString("utf8").trim() || "git cat-file failed");
  }

  const commits = [];
  let offset = 0;
  for (const expectedSha of commitShas) {
    const headerEnd = batch.stdout.indexOf(0x0a, offset);
    if (headerEnd < 0) {
      throw new Error(`missing cat-file header for ${expectedSha}`);
    }
    const header = batch.stdout.subarray(offset, headerEnd).toString("utf8");
    const [actualSha, objectType, sizeText] = header.split(" ");
    const size = Number.parseInt(sizeText, 10);
    if (
      actualSha !== expectedSha ||
      objectType !== "commit" ||
      !Number.isSafeInteger(size) ||
      size < 0
    ) {
      throw new Error(`invalid cat-file header for ${expectedSha}: ${header}`);
    }

    const contentStart = headerEnd + 1;
    const contentEnd = contentStart + size;
    if (contentEnd >= batch.stdout.length || batch.stdout[contentEnd] !== 0x0a) {
      throw new Error(`truncated commit object for ${expectedSha}`);
    }
    const content = batch.stdout.subarray(contentStart, contentEnd);
    const messageStart = content.indexOf(Buffer.from("\n\n"));
    if (messageStart < 0) {
      throw new Error(`commit ${expectedSha} has no message boundary`);
    }
    commits.push({
      sha: expectedSha,
      message: content.subarray(messageStart + 2).toString("utf8").trimEnd(),
    });
    offset = contentEnd + 1;
  }
  if (offset !== batch.stdout.length) {
    throw new Error("cat-file returned unexpected trailing data");
  }
  return commits;
}

function renderProblems(commit, result) {
  const problems = [...result.errors, ...result.warnings];
  if (problems.length === 0) {
    return;
  }
  console.error(`commit ${commit.sha}`);
  console.error(commit.message);
  for (const problem of problems) {
    const severity = problem.level === 2 ? "error" : "warning";
    console.error(`${severity}: ${problem.message} [${problem.name}]`);
  }
}

async function main() {
  const { repository, base, head } = parseArguments(process.argv.slice(2));
  const range = `${base}..${head}`;
  const output = execFileSync(
    "git",
    ["-C", repository, "rev-list", "--no-merges", range],
    { encoding: "utf8" },
  ).trim();
  const commitShas = output === "" ? [] : output.split("\n");
  const commits = readCommitMessages(repository, commitShas);
  const loaded = await load(
    {},
    {
      cwd: projectRoot,
      file: path.join(projectRoot, "commitlint.config.cjs"),
    },
  );
  const lintOptions = {
    parserOpts: loaded.parserPreset?.parserOpts ?? {},
    plugins: loaded.plugins ?? {},
    ignores: loaded.ignores ?? [],
    defaultIgnores: loaded.defaultIgnores,
  };
  const results = await Promise.all(
    commits.map((commit) => lint(commit.message, loaded.rules, lintOptions)),
  );

  let valid = true;
  for (const [index, result] of results.entries()) {
    renderProblems(commits[index], result);
    valid &&= result.valid;
  }
  if (!valid) {
    process.exitCode = 1;
  }
}

try {
  await main();
} catch (error) {
  const message = error instanceof Error ? error.message : String(error);
  console.error(`commit-range lint failed: ${message}`);
  process.exitCode = 2;
}
