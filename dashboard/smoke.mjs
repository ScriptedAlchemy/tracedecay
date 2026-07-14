import { spawn, spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import readline from "node:readline";
import { fileURLToPath } from "node:url";
import { chromium, devices } from "playwright";

const VIEWPORT_PROFILES = {
  desktop: {
    name: "desktop",
    contextOptions: { viewport: { width: 1280, height: 900 } },
  },
  narrow: {
    name: "narrow",
    contextOptions: { viewport: { width: 420, height: 900 } },
  },
  iphone12: {
    name: "iphone12",
    contextOptions: { ...devices["iPhone 12"] },
  },
  pixel5: {
    name: "pixel5",
    contextOptions: { ...devices["Pixel 5"] },
  },
  ipadmini: {
    name: "ipadmini",
    contextOptions: { ...devices["iPad Mini"] },
  },
};

const DASHBOARD_URL_RE = /(http:\/\/127\.0\.0\.1:\d+\/)/;
const DAEMON_HARNESS_ACTIVE = "TRACEDECAY_DAEMON_HARNESS_ACTIVE";
const DASHBOARD_STARTUP_TIMEOUT_MS = 30_000;
const DASHBOARD_STOP_TIMEOUT_MS = 5_000;
const IS_UNIX = process.platform !== "win32";

function workspaceRoot() {
  return fileURLToPath(new URL("..", import.meta.url));
}

function withTrailingSlash(url) {
  return url.endsWith("/") ? url : `${url}/`;
}

function runnableFile(candidate) {
  if (!candidate) return null;
  const resolved = path.resolve(candidate);
  try {
    if (!fs.statSync(resolved).isFile()) return null;
    if (IS_UNIX) fs.accessSync(resolved, fs.constants.X_OK);
    return resolved;
  } catch {
    return null;
  }
}

function resolveTracedecayBinary() {
  const cargoBinary = runnableFile(process.env.CARGO_BIN_EXE_tracedecay);
  if (cargoBinary) return cargoBinary;

  const suffix = process.platform === "win32" ? ".exe" : "";
  const workspaceBinary = path.join(workspaceRoot(), "target", "debug", `tracedecay${suffix}`);
  const builtBinary = runnableFile(workspaceBinary);
  if (builtBinary) return builtBinary;

  throw new Error(`built TraceDecay binary not found at ${workspaceBinary}`);
}

function runUnderIsolatedDaemon() {
  const harness = path.join(workspaceRoot(), "scripts", "with-isolated-tracedecay-daemon.sh");
  const tracedecayBinary = resolveTracedecayBinary();
  const result = spawnSync(
    harness,
    ["--bin", tracedecayBinary, "--", process.execPath, ...process.argv.slice(1)],
    {
      cwd: workspaceRoot(),
      env: process.env,
      stdio: "inherit",
    },
  );
  if (result.error) throw result.error;
  if (result.signal) {
    throw new Error(`isolated daemon harness terminated by ${result.signal}`);
  }
  return result.status ?? 1;
}

// The dashboard refuses to start without a TraceDecay index, and CI checkouts
// (unlike dev workspaces) have no `.tracedecay/`. Build a tiny throwaway
// project and index it so the smoke run is hermetic everywhere.
function createSmokeWorkspace() {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "tracedecay-dashboard-smoke-"));
  fs.writeFileSync(
    path.join(dir, "sample.rs"),
    "/// Fixture indexed by `tracedecay init` for the dashboard smoke test.\npub fn smoke_sample() -> u32 {\n    42\n}\n",
  );
  // stdin is closed so init's interactive `.gitignore` prompt reads EOF and
  // proceeds with the default instead of blocking.
  const result = spawnSync(resolveTracedecayBinary(), ["init", dir], {
    cwd: workspaceRoot(),
    env: process.env,
    stdio: ["ignore", "inherit", "inherit"],
  });
  if (result.error || result.status !== 0) {
    fs.rmSync(dir, { recursive: true, force: true });
    if (result.error) throw result.error;
    throw new Error(`tracedecay init failed for smoke workspace (code ${result.status})`);
  }
  return dir;
}

async function startDashboardServer(projectPath) {
  const child = spawn(
    resolveTracedecayBinary(),
    ["dashboard", "--port", "0", "--path", projectPath],
    {
      cwd: workspaceRoot(),
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
      detached: IS_UNIX,
    },
  );

  let closed = false;
  let stderrBuffer = "";
  let stopPromise = null;
  const exitPromise = new Promise((resolve) => {
    child.once("error", (error) => resolve({ error }));
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  child.once("close", () => {
    closed = true;
  });
  child.stderr.on("data", (chunk) => {
    stderrBuffer += chunk.toString();
  });

  const stdoutLines = readline.createInterface({ input: child.stdout });
  const stderrLines = readline.createInterface({ input: child.stderr });
  stderrLines.on("line", (line) => process.stderr.write(`[dashboard:stderr] ${line}\n`));

  const processGroupAlive = () => {
    if (child.pid === undefined) return false;
    if (!IS_UNIX) return child.exitCode === null && child.signalCode === null;
    try {
      process.kill(-child.pid, 0);
      return true;
    } catch (error) {
      if (error.code === "ESRCH") return false;
      if (error.code === "EPERM") return true;
      throw error;
    }
  };
  const sendSignal = (signal) => {
    if (child.pid === undefined) return;
    try {
      if (IS_UNIX) process.kill(-child.pid, signal);
      else child.kill(signal);
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  };
  const waitForStop = async () => {
    const deadline = Date.now() + DASHBOARD_STOP_TIMEOUT_MS;
    while (Date.now() < deadline) {
      if (closed && !processGroupAlive()) return true;
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    return closed && !processGroupAlive();
  };
  const stop = () => {
    if (stopPromise) return stopPromise;
    stopPromise = (async () => {
      try {
        if (!closed || processGroupAlive()) sendSignal("SIGTERM");
        if (!(await waitForStop())) {
          sendSignal("SIGKILL");
          if (!(await waitForStop())) {
            throw new Error("dashboard server did not stop after SIGKILL");
          }
        }
      } finally {
        stdoutLines.close();
        stderrLines.close();
      }
    })();
    return stopPromise;
  };

  try {
    const baseUrl = await new Promise((resolve, reject) => {
      let settled = false;
      const complete = (handler, value) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        handler(value);
      };
      const timer = setTimeout(
        () => complete(reject, new Error(
          `dashboard server startup timed out after ${DASHBOARD_STARTUP_TIMEOUT_MS}ms`,
        )),
        DASHBOARD_STARTUP_TIMEOUT_MS,
      );
      stdoutLines.on("line", (line) => {
        process.stdout.write(`[dashboard] ${line}\n`);
        const match = line.match(DASHBOARD_URL_RE);
        if (match) complete(resolve, withTrailingSlash(match[1]));
      });
      exitPromise.then(({ code, signal, error }) => {
        const detail = error?.message ?? `code ${code}${signal ? `, signal ${signal}` : ""}`;
        complete(reject, new Error(`dashboard server exited before startup (${detail})`));
      });
    });
    return { baseUrl, child, stop };
  } catch (error) {
    let stopError = null;
    try {
      await stop();
    } catch (caught) {
      stopError = caught;
    }
    const diagnostics = stderrBuffer.trim();
    const message = error instanceof Error ? error.message : String(error);
    const stopMessage = stopError ? `\nshutdown error: ${stopError.message}` : "";
    throw new Error(`${message}${diagnostics ? `\n${diagnostics}` : ""}${stopMessage}`, {
      cause: error,
    });
  }
}

async function waitForAny(page, locators, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    for (const locator of locators) {
      if (await locator.isVisible().catch(() => false)) {
        return locator;
      }
    }
    await page.waitForTimeout(Math.min(100, Math.max(0, deadline - Date.now())));
  }
  throw new Error(`timed out after ${timeoutMs}ms`);
}

async function runViewportSmoke(browser, baseUrl, profile, expectLcmMode) {
  const context = await browser.newContext({
    ...profile.contextOptions,
  });
  const page = await context.newPage();

  const runtimeErrors = [];
  page.on("pageerror", (err) => {
    runtimeErrors.push(`pageerror: ${err.message}`);
  });
  page.on("console", (msg) => {
    if (msg.type() === "error") {
      runtimeErrors.push(`console.error: ${msg.text()}`);
    }
  });
  const serverErrors = [];
  page.on("response", (response) => {
    if (response.status() >= 500) {
      serverErrors.push(`${response.status()} ${response.url()}`);
    }
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });

  // Shell tabs render with role="tab" (older shells used buttons).
  const memoryTab = page
    .getByRole("tab", { name: "Holographic Memory", exact: true })
    .or(page.getByRole("button", { name: "Holographic Memory", exact: true }));
  const lcmTab = page
    .getByRole("tab", { name: "LCM", exact: true })
    .or(page.getByRole("button", { name: "LCM", exact: true }));
  await memoryTab.waitFor({ state: "visible" });
  await lcmTab.waitFor({ state: "visible" });

  await memoryTab.click();
  const search = page.getByPlaceholder("Search holographic facts");
  await search.waitFor({ state: "visible" });
  await search.fill("cache");
  await page.waitForTimeout(500);

  // The holographic view switcher renders ARIA tabs (older builds used plain
  // buttons), so match either role.
  const similarityViewButton = page
    .getByRole("tab", { name: "Similarity", exact: true })
    .or(page.getByRole("button", { name: "Similarity", exact: true }));
  await similarityViewButton.waitFor({ state: "visible" });
  await assertNoHorizontalOverflow(page);
  await assertSpacingTokenResolves(page);
  await assertViewSwitcherLayout(page, profile.name);
  await similarityViewButton.click();
  await page.getByText("Similar Pairs").waitFor({ state: "visible" });

  // --- Curation tab: check the panel renders and autonomous run controls are present ---
  const curationViewButton = page
    .getByRole("tab", { name: "Curation", exact: true })
    .or(page.getByRole("button", { name: "Curation", exact: true }));
  await curationViewButton.waitFor({ state: "visible" });
  await curationViewButton.click();
  await page.getByText("Curation").first().waitFor({ state: "visible" });
  const runMemoryCuratorButton = page.getByRole("button", { name: "Run Memory curator" });
  await runMemoryCuratorButton.waitFor({ state: "visible" });
  await page.getByText("Activity").first().waitFor({ state: "visible" });
  await page.getByText("codex_app_server").first().waitFor({ state: "visible" });

  // --- Code Graph tab: the canvas self-populates with the seedless default
  // slice (no search required); the empty state must not be visible.
  const graphTab = page
    .getByRole("tab", { name: "Code Graph", exact: true })
    .or(page.getByRole("button", { name: "Code Graph", exact: true }));
  await graphTab.click();
  await page.locator(".tsg-canvas").waitFor({ state: "visible", timeout: 8000 });
  await page.waitForFunction(
    () => {
      const footer = document.querySelector(".tsg-canvas-count");
      const match = footer?.textContent?.match(/^\s*([\d,]+)\s*\/\s*([\d,]+)\s*nodes/);
      return Boolean(match && Number(match[1].replace(/,/g, "")) > 0);
    },
    undefined,
    { timeout: 8000 },
  );
  if (await page.locator(".tsg-graph-empty").isVisible().catch(() => false)) {
    throw new Error("Code Graph canvas should auto-populate, but the empty state is visible");
  }

  await lcmTab.click();
  const recentSessionsHeader = page.getByRole("heading", { name: "Recent Sessions" });
  const emptyStateHeader = page.getByRole("heading", { name: "No LCM sessions indexed yet" });
  if (expectLcmMode === "empty") {
    await emptyStateHeader.waitFor({ state: "visible", timeout: 8000 });
  } else if (expectLcmMode === "non-empty") {
    await recentSessionsHeader.waitFor({ state: "visible", timeout: 8000 });
    if (await emptyStateHeader.isVisible().catch(() => false)) {
      throw new Error("Expected non-empty LCM state, but empty-state panel is visible");
    }
  } else {
    await waitForAny(page, [recentSessionsHeader, emptyStateHeader], 8000);
  }

  if (profile.name === "desktop") {
    await runSecondaryTabsSmoke(page);
  }

  if (runtimeErrors.length > 0) {
    throw new Error(
      `dashboard raised ${runtimeErrors.length} runtime error(s):\n  ${runtimeErrors.join("\n  ")}`,
    );
  }
  if (serverErrors.length > 0) {
    throw new Error(
      `dashboard returned ${serverErrors.length} server error response(s):\n  ${serverErrors.join("\n  ")}`,
    );
  }

  await context.close();
}

async function runSecondaryTabsSmoke(page) {
  const tab = (name) =>
    page
      .getByRole("tab", { name, exact: true })
      .or(page.getByRole("button", { name, exact: true }));

  await tab("Savings & Cost").click();
  await page.waitForFunction(
    () => {
      const text = document.body.innerText;
      return !text.includes("Loading savings analytics") && /saved/i.test(text);
    },
    undefined,
    { timeout: 12000 },
  );
  for (const subTab of ["Sessions", "Models & Pricing"]) {
    await tab(subTab).click();
    await page.waitForTimeout(200);
  }

  await tab("Code Diagnostics").click();
  await page.getByText("ENGINES", { exact: false }).first().waitFor({ state: "visible", timeout: 8000 });
  await assertEngineIdsDoNotCharStack(page);

  await tab("Settings").click();
  await page.getByText("Project config", { exact: false }).first().waitFor({ state: "visible", timeout: 8000 });
  await page.getByRole("button", { name: "Save", exact: true }).first().waitFor({ state: "visible", timeout: 8000 });
}

async function assertEngineIdsDoNotCharStack(page) {
  const whiteSpaces = await page.$$eval(".tdcd-engine-row > code", (nodes) =>
    nodes.map((node) => getComputedStyle(node).whiteSpace),
  );
  for (const whiteSpace of whiteSpaces) {
    if (whiteSpace !== "nowrap") {
      throw new Error(
        `engine id <code> must not per-character wrap; expected white-space: nowrap, got "${whiteSpace}"`,
      );
    }
  }
}

async function assertNoHorizontalOverflow(page) {
  const overflow = await page.evaluate(() => {
    const doc = document.documentElement;
    return {
      clientWidth: doc.clientWidth,
      scrollWidth: doc.scrollWidth,
      bodyScrollWidth: document.body.scrollWidth,
    };
  });
  if (overflow.scrollWidth > overflow.clientWidth + 1) {
    throw new Error(
      `dashboard has horizontal overflow: ${JSON.stringify(overflow)}`,
    );
  }
}

// Defense in depth against the `@layer theme` regression: if Tailwind's
// structural `--spacing` token is missing, every spacing utility collapses.
// Assert it resolves to a non-empty value in the real, fully-loaded page.
async function assertSpacingTokenResolves(page) {
  const spacing = await page.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--spacing").trim(),
  );
  if (!spacing) {
    throw new Error(
      "dashboard is missing the --spacing design token; spacing utilities will collapse",
    );
  }
}

async function assertViewSwitcherLayout(page, profileName) {
  if (profileName !== "narrow") return;
  const layout = await page.locator(".hv-viewswitch").first().evaluate((el) => {
    const style = window.getComputedStyle(el);
    return {
      flexWrap: style.flexWrap,
      clientWidth: el.clientWidth,
      scrollWidth: el.scrollWidth,
    };
  });
  if (layout.flexWrap !== "nowrap") {
    throw new Error(`narrow Holographic view switcher should not wrap: ${JSON.stringify(layout)}`);
  }
  if (layout.scrollWidth < layout.clientWidth) {
    throw new Error(`narrow Holographic view switcher should remain scrollable: ${JSON.stringify(layout)}`);
  }
}

async function main() {
  const urlArg = process.argv.find((arg) => arg.startsWith("--url="));
  const explicitUrl = urlArg ? withTrailingSlash(urlArg.replace("--url=", "")) : null;
  const lcmModeArg = process.argv.find((arg) => arg.startsWith("--expect-lcm="));
  const expectLcmMode = lcmModeArg ? lcmModeArg.replace("--expect-lcm=", "") : "either";
  if (!["either", "empty", "non-empty"].includes(expectLcmMode)) {
    throw new Error("--expect-lcm must be one of: either, empty, non-empty");
  }
  const profilesArg = process.argv.find((arg) => arg.startsWith("--profiles="));
  const profileKeys = (profilesArg ? profilesArg.replace("--profiles=", "") : "desktop,narrow")
    .split(",")
    .map((value) => value.trim().toLowerCase())
    .filter(Boolean);
  const profiles = profileKeys.map((key) => {
    const profile = VIEWPORT_PROFILES[key];
    if (!profile) {
      throw new Error(`Unknown --profiles entry: ${key}. Expected one of ${Object.keys(VIEWPORT_PROFILES).join(", ")}`);
    }
    return profile;
  });

  if (!explicitUrl && process.env[DAEMON_HARNESS_ACTIVE] !== "1") {
    console.log("Starting hermetic foreground TraceDecay daemon for smoke test...");
    process.exitCode = runUnderIsolatedDaemon();
    return;
  }

  let server = null;
  let workspace = null;

  try {
    if (explicitUrl) {
      server = { baseUrl: explicitUrl, stop: async () => {} };
      console.log(`Using existing dashboard URL: ${explicitUrl}`);
    } else {
      console.log("Smoke daemon is ready.");
      console.log("Creating hermetic smoke workspace (tracedecay init)...");
      workspace = createSmokeWorkspace();
      console.log(`Starting \`tracedecay dashboard --port 0 --path ${workspace}\` for smoke test...`);
      server = await startDashboardServer(workspace);
      console.log(`Dashboard URL: ${server.baseUrl}`);
    }

    const browser = await chromium.launch({ headless: true });
    try {
      for (const profile of profiles) {
        const viewport = profile.contextOptions.viewport;
        const size = viewport ? `${viewport.width}x${viewport.height}` : "device-default";
        console.log(`Running ${profile.name} smoke (${size})...`);
        await runViewportSmoke(browser, server.baseUrl, profile, expectLcmMode);
      }
    } finally {
      await browser.close();
    }
    console.log("Dashboard smoke checks passed.");
  } finally {
    if (server) {
      await server.stop();
    }
    if (workspace) {
      fs.rmSync(workspace, { recursive: true, force: true });
    }
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.stack ?? err.message : String(err));
  process.exitCode = 1;
});
