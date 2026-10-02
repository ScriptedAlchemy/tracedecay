import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DaemonAuthorityRecordSchema } from "../src/server/authority.js";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");

export function tracedecayBinary(): string {
  const candidate = process.env.TRACEDECAY_BIN ?? path.join(REPO_ROOT, "target/debug/tracedecay");
  if (!existsSync(candidate)) {
    throw new Error(`tracedecay binary not found at ${candidate}; run cargo build -p tracedecay-cli or set TRACEDECAY_BIN`);
  }
  return candidate;
}

export const BILLING_SOURCE = `export interface Invoice { id: string; cents: number; paid: boolean }

export function computeTotal(invoices: Invoice[]): number {
  return invoices.reduce((sum, invoice) => sum + invoice.cents, 0);
}

export function outstandingTotal(invoices: Invoice[]): number {
  return computeTotal(invoices.filter((invoice) => !invoice.paid));
}

export function formatCents(cents: number): string {
  return \`$\${(cents / 100).toFixed(2)}\`;
}
`;

export const REPORT_SOURCE = `import { formatCents, outstandingTotal, type Invoice } from "./billing";

export function renderBillingReport(invoices: Invoice[]): string {
  const outstanding = outstandingTotal(invoices);
  return \`Outstanding: \${formatCents(outstanding)}\`;
}

export function renderEmptyReport(): string {
  return renderBillingReport([]);
}
`;

export const INDEX_SOURCE = `import { renderBillingReport } from "./report";

export function main(): void {
  const output = renderBillingReport([{ id: "inv-1", cents: 1250, paid: false }]);
  process.stdout.write(\`\${output}\\n\`);
}
`;

export const SECOND_PROJECT_SOURCE = `export function shippingQuote(weightGrams: number): number {
  return Math.ceil(weightGrams / 100) * 45;
}

export function describeQuote(weightGrams: number): string {
  return \`Shipping: \${shippingQuote(weightGrams)}\`;
}
`;

export type FixtureRepo = { readonly root: string; head(): string };

export class DaemonFixture {
  readonly binary = tracedecayBinary();
  readonly root: string;
  readonly home: string;
  readonly profileRoot: string;
  readonly env: NodeJS.ProcessEnv;
  #daemon: ChildProcess | null = null;
  #stderr = "";

  private constructor(root: string) {
    this.root = root;
    this.home = path.join(root, "home");
    this.profileRoot = path.join(this.home, ".tracedecay");
    this.env = {
      PATH: process.env.PATH ?? "",
      HOME: this.home,
      USERPROFILE: this.home,
      XDG_CONFIG_HOME: path.join(this.home, ".config"),
      XDG_DATA_HOME: path.join(this.home, ".local/share"),
      XDG_STATE_HOME: path.join(this.home, ".local/state"),
      XDG_CACHE_HOME: path.join(this.home, ".cache"),
      XDG_RUNTIME_DIR: path.join(this.home, ".runtime"),
      TRACEDECAY_DATA_DIR: this.profileRoot,
      TRACEDECAY_GLOBAL_DB: path.join(this.profileRoot, "global.db"),
      TRACEDECAY_DAEMON_SOCKET: path.join(this.profileRoot, "daemon.sock"),
      TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN: "1",
      TRACEDECAY_BIN: this.binary,
    };
  }

  static async create(): Promise<DaemonFixture> {
    const root = await mkdtemp(path.join(os.tmpdir(), "tracedecay-chatgpt-ext-"));
    const fixture = new DaemonFixture(root);
    await mkdir(fixture.profileRoot, { recursive: true });
    await mkdir(path.join(fixture.home, ".config"), { recursive: true });
    return fixture;
  }

  async startDaemon(): Promise<void> {
    if (this.#daemon !== null) throw new Error("daemon already running");
    const child = spawn(this.binary, ["daemon", "run"], {
      cwd: this.home,
      env: this.env,
      stdio: ["ignore", "ignore", "pipe"],
    });
    this.#stderr = "";
    child.stderr?.on("data", (chunk: Buffer) => {
      this.#stderr += chunk.toString("utf8");
    });
    this.#daemon = child;
    const deadline = Date.now() + 120_000;
    while (Date.now() < deadline) {
      if (child.exitCode !== null) throw new Error(`daemon exited early (${child.exitCode}):\n${this.#stderr}`);
      const record = await this.readAuthority();
      if (record !== null && record.http_application_endpoint) return;
      await sleep(100);
    }
    throw new Error(`daemon did not publish its authority record in time:\n${this.#stderr}`);
  }

  async readAuthority() {
    try {
      const raw = await readFile(path.join(this.profileRoot, "daemon-authority.json"), "utf8");
      const parsed = DaemonAuthorityRecordSchema.safeParse(JSON.parse(raw));
      return parsed.success ? parsed.data : null;
    } catch {
      return null;
    }
  }

  async stopDaemon(): Promise<void> {
    const child = this.#daemon;
    if (child === null) return;
    this.#daemon = null;
    if (child.exitCode === null) {
      child.kill("SIGTERM");
      await new Promise<void>((resolve) => {
        const timer = setTimeout(() => {
          child.kill("SIGKILL");
          resolve();
        }, 15_000);
        child.once("exit", () => {
          clearTimeout(timer);
          resolve();
        });
      });
    }
  }

  async createRepo(name: string, files: Record<string, string>): Promise<FixtureRepo> {
    const root = path.join(this.root, name);
    await mkdir(path.join(root, "src"), { recursive: true });
    await writeFile(path.join(root, "package.json"), JSON.stringify({ name, version: "0.0.0", type: "module" }, null, 2));
    for (const [relative, content] of Object.entries(files)) await writeFile(path.join(root, relative), content);
    this.git(root, ["init", "-q", "-b", "main"]);
    this.commit(root, "initial fixture");
    return { root, head: () => this.git(root, ["rev-parse", "HEAD"]).trim() };
  }

  commit(root: string, message: string): string {
    this.git(root, ["add", "-A"]);
    this.git(root, ["-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-q", "-m", message]);
    return this.git(root, ["rev-parse", "HEAD"]).trim();
  }

  git(root: string, args: readonly string[]): string {
    const result = spawnSync("git", [...args], { cwd: root, env: { ...this.env, GIT_CONFIG_NOSYSTEM: "1" }, encoding: "utf8" });
    if (result.status !== 0) throw new Error(`git ${args.join(" ")} failed: ${result.stderr}`);
    return result.stdout;
  }

  initProject(root: string): void {
    const result = spawnSync(this.binary, ["init"], { cwd: root, env: this.env, encoding: "utf8" });
    if (result.status !== 0) throw new Error(`tracedecay init failed in ${root}:\n${result.stdout}\n${result.stderr}`);
  }

  sync(root: string): void {
    const result = spawnSync(this.binary, ["sync", root], { cwd: root, env: this.env, encoding: "utf8" });
    if (result.status !== 0) throw new Error(`tracedecay sync failed in ${root}:\n${result.stdout}\n${result.stderr}`);
  }

  async destroy(): Promise<void> {
    await this.stopDaemon();
    await rm(this.root, { recursive: true, force: true });
  }
}

export function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function waitFor<T>(label: string, probe: () => Promise<T | null>, timeoutMs = 120_000): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let last: unknown = null;
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value !== null) return value;
    } catch (error) {
      last = error;
    }
    await sleep(250);
  }
  throw new Error(`timed out waiting for ${label}${last === null ? "" : `: ${String(last)}`}`);
}
