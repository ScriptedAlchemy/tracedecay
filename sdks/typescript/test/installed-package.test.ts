import { once } from "node:events";
import {
  existsSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import {
  type ChildProcess,
  execFileSync,
  spawn,
  spawnSync,
} from "node:child_process";
import { fileURLToPath } from "node:url";

import { expect, it } from "vitest";

const PACKAGE_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPOSITORY_ROOT = resolve(PACKAGE_ROOT, "../..");
const NPM = process.platform === "win32" ? "npm.cmd" : "npm";

interface DaemonAuthority {
  auth_token: string;
  http_application_endpoint: string;
}

interface ProjectContext {
  project: {
    project_id: string;
  };
}

interface PackedArtifact {
  filename: string;
  files: Array<{ path: string }>;
}

function isolatedEnvironment(home: string): NodeJS.ProcessEnv {
  const profile = join(home, ".tracedecay");
  return {
    ...process.env,
    HOME: home,
    USERPROFILE: home,
    XDG_CONFIG_HOME: join(home, ".config"),
    TRACEDECAY_DATA_DIR: profile,
    TRACEDECAY_GLOBAL_DB: join(profile, "global.db"),
    TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN: "1",
  };
}

function run(
  executable: string,
  args: string[],
  options: { cwd: string; env: NodeJS.ProcessEnv },
): string {
  return execFileSync(executable, args, {
    ...options,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    timeout: 120_000,
  });
}

async function waitForAuthority(
  child: ChildProcess,
  authorityPath: string,
): Promise<DaemonAuthority> {
  const deadline = Date.now() + 15_000;
  for (;;) {
    if (child.exitCode !== null) {
      throw new Error(`TraceDecay daemon exited during startup (${child.exitCode})`);
    }
    if (existsSync(authorityPath)) {
      try {
        const authority = JSON.parse(
          readFileSync(authorityPath, "utf8"),
        ) as Partial<DaemonAuthority>;
        if (
          typeof authority.auth_token === "string" &&
          authority.auth_token.length === 64 &&
          typeof authority.http_application_endpoint === "string"
        ) {
          return authority as DaemonAuthority;
        }
      } catch {
        // The daemon publishes the authority record atomically more than once.
      }
    }
    if (Date.now() >= deadline) {
      throw new Error(`timed out waiting for daemon authority at ${authorityPath}`);
    }
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 25));
  }
}

async function stopDaemon(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null) {
    return;
  }
  child.kill("SIGINT");
  const stopped = await Promise.race([
    once(child, "exit").then(() => true),
    new Promise<false>((resolveDelay) =>
      setTimeout(() => resolveDelay(false), 5_000),
    ),
  ]);
  if (!stopped && child.exitCode === null) {
    child.kill("SIGKILL");
    await once(child, "exit");
  }
}

it(
  "imports the packed SDK and conforms against an isolated production daemon",
  async () => {
    if (process.platform === "win32") {
      throw new Error("installed-package conformance requires Unix daemon sockets");
    }

    const scratch = mkdtempSync(join(tmpdir(), "tracedecay-sdk-installed-"));
    const home = join(scratch, "home");
    const profile = join(home, ".tracedecay");
    const project = join(scratch, "project");
    const consumer = join(scratch, "consumer");
    const socket = join(profile, "daemon.sock");
    const authorityPath = join(profile, "daemon-authority.json");
    const binary = resolve(
      process.env.TRACEDECAY_TEST_BIN ?? join(REPOSITORY_ROOT, "target/debug/tracedecay"),
    );
    const env = isolatedEnvironment(home);
    let daemon: ChildProcess | undefined;
    let tarball: string | undefined;
    let ownsTarball = false;
    let daemonStderr = "";

    try {
      expect(existsSync(binary), `missing production daemon binary: ${binary}`).toBe(
        true,
      );
      mkdirSync(home, { recursive: true });
      mkdirSync(project, { recursive: true });
      mkdirSync(consumer, { recursive: true });
      writeFileSync(
        join(project, "package.json"),
        '{"name":"installed-sdk-daemon-fixture","private":true}\n',
      );
      writeFileSync(
        join(project, "index.ts"),
        "export const installedPackageFixture = true;\n",
      );
      run("git", ["init", "--quiet"], { cwd: project, env });

      run(binary, ["init"], { cwd: project, env });
      daemon = spawn(binary, ["daemon", "run", "--socket", socket], {
        cwd: project,
        env,
        stdio: ["ignore", "ignore", "pipe"],
      });
      daemon.stderr?.setEncoding("utf8");
      daemon.stderr?.on("data", (chunk: string) => {
        daemonStderr += chunk;
      });
      const authority = await waitForAuthority(daemon, authorityPath);
      const context = JSON.parse(
        run(binary, ["projects", "context", project, "--json"], {
          cwd: project,
          env,
        }),
      ) as ProjectContext;
      expect(context.project.project_id).toMatch(/\S/);

      // Publish-lane callers build and pack exactly once and pass the resulting
      // tarball in here so conformance exercises the identical bytes that
      // later get published, rather than a fresh rebuild/repack.
      const externalTarball = process.env.TRACEDECAY_SDK_TARBALL;
      if (externalTarball !== undefined) {
        tarball = resolve(externalTarball);
        ownsTarball = false;
        expect(existsSync(tarball), `missing prebuilt tarball: ${tarball}`).toBe(true);
        const contents = run("tar", ["-tzf", tarball], { cwd: PACKAGE_ROOT, env: process.env });
        expect(contents).toEqual(
          expect.stringContaining("package/dist/index.js"),
        );
        expect(contents).toEqual(
          expect.stringContaining("package/dist/client.js"),
        );
      } else {
        ownsTarball = true;
        run(NPM, ["run", "build"], { cwd: PACKAGE_ROOT, env: process.env });
        const packed = JSON.parse(
          run(NPM, ["pack", "--json", "--ignore-scripts"], {
            cwd: PACKAGE_ROOT,
            env: process.env,
          }),
        ) as PackedArtifact[];
        expect(packed).toHaveLength(1);
        const artifact = packed[0];
        expect(artifact).toBeDefined();
        tarball = join(PACKAGE_ROOT, artifact!.filename);
        expect(artifact!.files.map((file) => file.path)).toEqual(
          expect.arrayContaining([
            "dist/index.js",
            "dist/index.d.ts",
            "dist/client.js",
            "dist/client.d.ts",
          ]),
        );
      }

      writeFileSync(
        join(consumer, "package.json"),
        '{"name":"tracedecay-sdk-installed-consumer","private":true,"type":"module"}\n',
      );
      run(
        NPM,
        [
          "install",
          "--ignore-scripts",
          "--no-audit",
          "--no-fund",
          tarball,
        ],
        { cwd: consumer, env: process.env },
      );
      writeFileSync(
        join(consumer, "conformance.mjs"),
        `
import * as sdk from "@tracedecay/sdk";
import * as clientEntry from "@tracedecay/sdk/client";

const callableOperations = new Set(sdk.OPERATIONS.map((operation) => operation.operation));
if (typeof sdk.createClient !== "function" ||
    typeof clientEntry.createClient !== "function" ||
    !Array.isArray(sdk.SERVER_OPERATIONS) ||
    sdk.SERVER_OPERATIONS.length === 0 ||
    !sdk.SERVER_OPERATIONS.every(
      (operation) => operation.sdkAvailability === "unavailable" &&
        operation.disposition === "schema_unavailable" &&
        !callableOperations.has(operation.operation),
    ) ||
    !Array.isArray(sdk.UNAVAILABLE_OPERATIONS)) {
  throw new Error("installed package exports are incomplete");
}

const attemptFinishDescriptor = sdk.OPERATIONS.find(
  (operation) => operation.operation === "work_attempt_finish",
);
if (!attemptFinishDescriptor ||
    attemptFinishDescriptor.operationId !== "operation.work.attempt_finish" ||
    attemptFinishDescriptor.route !== "/application/work/attempt/finish" ||
    attemptFinishDescriptor.effect !== "administrative" ||
    attemptFinishDescriptor.idempotency !== "required" ||
    attemptFinishDescriptor.bindingId !== "binding.http.work.attempt_finish" ||
    attemptFinishDescriptor.requestSchema.schemaId !== "schema.work.attempt_finish.request" ||
    attemptFinishDescriptor.requestSchema.revision !== 1 ||
    attemptFinishDescriptor.resultSchema.schemaId !== "schema.work.attempt_finish.result" ||
    attemptFinishDescriptor.resultSchema.revision !== 1) {
  throw new Error("installed package work_attempt_finish descriptor identity drifted");
}

console.log(JSON.stringify({
  evidence: "package-import",
  root: typeof sdk.createClient,
  client: typeof clientEntry.createClient,
}));

const availabilityClient = sdk.createClient({
  baseUrl: process.env.TRACEDECAY_SDK_BASE_URL,
  projectId: process.env.TRACEDECAY_SDK_PROJECT_ID,
  token: process.env.TRACEDECAY_SDK_TOKEN,
});
if (!("work_snapshot" in availabilityClient.operations) ||
    !("work_attempt_finish" in availabilityClient.operations) ||
    !("workflow_register_definition" in availabilityClient.operations) ||
    "test_results" in availabilityClient.operations ||
    "invoke" in availabilityClient ||
    "requestOperation" in availabilityClient) {
  throw new Error("only schema-authorized Work and Workflow operations may be callable");
}

const baseUrl = process.env.TRACEDECAY_SDK_BASE_URL;
for (const mode of ["local", "remote"]) {
  const client = sdk.createClient({
    baseUrl,
    projectId: process.env.TRACEDECAY_SDK_PROJECT_ID,
    token: process.env.TRACEDECAY_SDK_TOKEN,
    ...(mode === "remote" ? { origin: new URL(baseUrl).origin } : {}),
  });
  let operationId;
  try {
    const result = await client.operations.work_snapshot(
      { page_size: 1 },
      { page: { size: 1 } },
    );
    operationId = result.request_id;
  } catch (error) {
    if (!(error instanceof sdk.TraceDecayProblemError)) {
      throw error;
    }
    operationId = error.envelope.request_id;
  }
  let terminal;
  try {
    const initial = client.streamOperation(operationId);
    const opened = await initial.next();
    if (opened.done || opened.value.event !== "open") {
      throw new Error("operation stream did not expose an open frontier");
    }
    const frontier = opened.value.data.data.frontier;
    await initial.return();
    const resumed = [];
    for await (const event of client.streamOperation(operationId, {
      resume: {
        resumeToken: frontier.resume_token,
        nextSequence: frontier.next_sequence,
      },
    })) {
      resumed.push(event);
    }
    terminal = resumed.at(-1)?.event;
  } catch (error) {
    if (!(error instanceof sdk.TraceDecayUnavailableError) ||
        error.problem.code !== "operation_event.unavailable") {
      throw error;
    }
    try {
      await client.streamOperation(operationId, {
        resume: { resumeToken: "resume.unavailable", nextSequence: 1 },
      }).next();
      throw new Error("unavailable resume unexpectedly opened");
    } catch (resumeError) {
      if (!(resumeError instanceof sdk.TraceDecayStaleError) ||
          resumeError.problem.code !== "operation_event.resume_expired") {
        throw resumeError;
      }
    }
    terminal = "unavailable";
  }
  let cancellation;
  try {
    cancellation = (await client.cancelOperation(operationId)).status;
  } catch (error) {
    if (!(error instanceof sdk.TraceDecayUnavailableError) ||
        error.problem.code !== "operation_event.unavailable") {
      throw error;
    }
    cancellation = "unavailable";
  }
  console.log(JSON.stringify({
    evidence: "production-lifecycle",
    mode,
    terminal,
    cancellation,
  }));
}
`,
      );

      const consumerRun = spawnSync(process.execPath, ["conformance.mjs"], {
        cwd: consumer,
        env: {
          ...process.env,
          TRACEDECAY_SDK_BASE_URL: `http://${authority.http_application_endpoint}`,
          TRACEDECAY_SDK_PROJECT_ID: context.project.project_id,
          TRACEDECAY_SDK_TOKEN: authority.auth_token,
        },
        encoding: "utf8",
        timeout: 60_000,
      });
      expect(
        consumerRun.status,
        `installed consumer failed\nstdout:\n${consumerRun.stdout}\nstderr:\n${consumerRun.stderr}`,
      ).toBe(0);
    } catch (error) {
      if (daemonStderr.trim() !== "") {
        console.error(`isolated daemon stderr:\n${daemonStderr}`);
      }
      throw error;
    } finally {
      if (daemon !== undefined) {
        await stopDaemon(daemon);
      }
      if (tarball !== undefined && ownsTarball) {
        rmSync(tarball, { force: true });
      }
      rmSync(scratch, { recursive: true, force: true });
    }
  },
  180_000,
);
