import { spawn } from "node:child_process";
import process from "node:process";
import readline from "node:readline";
import { chromium } from "playwright";

const VIEWPORTS = [
  { name: "desktop", width: 1280, height: 900 },
  { name: "narrow", width: 420, height: 900 },
];

const DASHBOARD_URL_RE = /(http:\/\/127\.0\.0\.1:\d+\/)/;

function workspaceRoot() {
  return new URL("..", import.meta.url).pathname;
}

function withTrailingSlash(url) {
  return url.endsWith("/") ? url : `${url}/`;
}

async function startDashboardServer() {
  return new Promise((resolve, reject) => {
    const child = spawn("cargo", ["run", "--", "dashboard", "--port", "0"], {
      cwd: workspaceRoot(),
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let settled = false;
    let stderrBuffer = "";
    const complete = (handler, value) => {
      if (settled) return;
      settled = true;
      handler(value);
    };

    const stdoutLines = readline.createInterface({ input: child.stdout });
    stdoutLines.on("line", (line) => {
      process.stdout.write(`[dashboard] ${line}\n`);
      const match = line.match(DASHBOARD_URL_RE);
      if (match) {
        complete(resolve, {
          baseUrl: withTrailingSlash(match[1]),
          child,
          stop: async () => {
            child.kill("SIGTERM");
            await new Promise((done) => child.once("exit", done));
          },
        });
      }
    });

    const stderrLines = readline.createInterface({ input: child.stderr });
    stderrLines.on("line", (line) => {
      stderrBuffer = `${stderrBuffer}${line}\n`;
      process.stderr.write(`[dashboard:stderr] ${line}\n`);
    });

    child.once("error", (err) => {
      complete(reject, err);
    });
    child.once("exit", (code) => {
      if (settled) return;
      complete(
        reject,
        new Error(`dashboard server exited before startup (code ${code})\n${stderrBuffer}`),
      );
    });
  });
}

async function waitForAny(page, locators, timeoutMs) {
  const timeout = new Promise((_, reject) => {
    setTimeout(() => reject(new Error(`timed out after ${timeoutMs}ms`)), timeoutMs);
  });
  const checks = locators.map((locator) =>
    locator.waitFor({ state: "visible", timeout: timeoutMs }).then(() => locator),
  );
  return Promise.race([timeout, ...checks]);
}

async function runViewportSmoke(browser, baseUrl, viewport, expectLcmMode) {
  const context = await browser.newContext({
    viewport: { width: viewport.width, height: viewport.height },
  });
  const page = await context.newPage();
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

  const similarityViewButton = page.getByRole("button", { name: "Similarity" });
  await similarityViewButton.waitFor({ state: "visible" });
  await similarityViewButton.click();
  await page.getByText("Similar Pairs").waitFor({ state: "visible" });

  // --- Curation tab: check the panel renders and Preview button is present ---
  const curationViewButton = page.getByRole("button", { name: "Curation" });
  await curationViewButton.waitFor({ state: "visible" });
  await curationViewButton.click();
  await page.getByText("Curation").first().waitFor({ state: "visible" });
  const previewButton = page.getByRole("button", { name: "Preview" });
  await previewButton.waitFor({ state: "visible" });

  // Click Preview — triggers dry-run curation; wait for a delete plan or the
  // "no changes" empty state (the plan proposes permanent deletions now).
  await previewButton.click();
  await page.waitForFunction(
    () => {
      const text = document.body.innerText;
      return (
        text.includes("delete") ||
        text.includes("no changes") ||
        text.includes("proposed actions")
      );
    },
    undefined,
    { timeout: 10000 },
  );

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

  await context.close();
}

async function main() {
  const urlArg = process.argv.find((arg) => arg.startsWith("--url="));
  const explicitUrl = urlArg ? withTrailingSlash(urlArg.replace("--url=", "")) : null;
  const lcmModeArg = process.argv.find((arg) => arg.startsWith("--expect-lcm="));
  const expectLcmMode = lcmModeArg ? lcmModeArg.replace("--expect-lcm=", "") : "either";
  if (!["either", "empty", "non-empty"].includes(expectLcmMode)) {
    throw new Error("--expect-lcm must be one of: either, empty, non-empty");
  }
  let server = null;

  try {
    if (explicitUrl) {
      server = { baseUrl: explicitUrl, stop: async () => {} };
      console.log(`Using existing dashboard URL: ${explicitUrl}`);
    } else {
      console.log("Starting `tokensave dashboard --port 0` for smoke test...");
      server = await startDashboardServer();
      console.log(`Dashboard URL: ${server.baseUrl}`);
    }

    const browser = await chromium.launch({ headless: true });
    try {
      for (const viewport of VIEWPORTS) {
        console.log(`Running ${viewport.name} smoke (${viewport.width}x${viewport.height})...`);
        await runViewportSmoke(browser, server.baseUrl, viewport, expectLcmMode);
      }
    } finally {
      await browser.close();
    }
    console.log("Dashboard smoke checks passed.");
  } finally {
    if (server) {
      await server.stop();
    }
  }
}

main().catch((err) => {
  console.error(err instanceof Error ? err.stack ?? err.message : String(err));
  process.exitCode = 1;
});
