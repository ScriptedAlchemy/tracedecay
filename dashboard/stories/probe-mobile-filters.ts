/**
 * Functional probe: the Code workspace's symbol search must be reachable below
 * the `lg` breakpoint, where ExplorerSplit's filter rail is `max-lg:hidden`.
 * Drives the collapsible Query strip at 320 and 768 and asserts a real query
 * reaches the results column.
 *
 * Run:  npx tsx stories/probe-mobile-filters.ts
 */
import { spawn, type ChildProcess } from 'node:child_process';
import { chromium } from '@playwright/test';
import { installApiFixtures } from './fixtures/route.ts';

const PORT = Number(process.env['AUDIT_PORT'] ?? 5199);
const BASE = `http://localhost:${PORT}`;

async function waitForServer(timeoutMs = 90_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    try {
      const res = await fetch(BASE, { signal: AbortSignal.timeout(5_000) });
      if (res.ok || res.status === 304) return;
    } catch {
      /* not up yet */
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`server at ${BASE} not ready`);
}

async function main(): Promise<void> {
  const server: ChildProcess = spawn('npx', ['rsbuild', 'dev', '--port', String(PORT)], {
    cwd: process.cwd(),
    env: { ...process.env, NO_COLOR: '1' },
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: true,
  });
  server.stdout?.on('data', () => {});
  server.stderr?.on('data', () => {});

  let failures = 0;
  const browser = await chromium.launch({ headless: true });
  try {
    await waitForServer();
    const context = await browser.newContext({ deviceScaleFactor: 1 });
    const page = await context.newPage();
    await installApiFixtures(page);

    for (const width of [320, 768, 1440] as const) {
      await page.setViewportSize({ width, height: 900 });
      await page.goto(`${BASE}/code`, { waitUntil: 'domcontentloaded' });
      await page.waitForSelector('main#td-main', { timeout: 15_000 });
      await new Promise((r) => setTimeout(r, 700));

      const toggle = page.getByRole('button', { name: 'Query' });
      const narrow = width < 1024;
      const toggleVisible = await toggle.isVisible().catch(() => false);
      if (toggleVisible !== narrow) {
        console.error(
          `[probe] ${width}: Query disclosure visible=${toggleVisible}, expected ${narrow}`,
        );
        failures += 1;
      }
      if (narrow) {
        await toggle.click();
        if ((await toggle.getAttribute('aria-expanded')) !== 'true') {
          console.error(`[probe] ${width}: disclosure did not expand`);
          failures += 1;
        }
      }

      const input = page.getByLabel('Symbol search');
      const inputs = await input.count();
      const visibleInputs = [];
      for (let i = 0; i < inputs; i += 1) {
        if (await input.nth(i).isVisible()) visibleInputs.push(i);
      }
      if (visibleInputs.length !== 1) {
        console.error(
          `[probe] ${width}: expected exactly 1 visible symbol search, found ${visibleInputs.length}`,
        );
        failures += 1;
        continue;
      }
      const field = input.nth(visibleInputs[0]!);
      await field.fill('graph');
      await field.press('Enter');
      await page.waitForFunction(
        () => /matched|matches/.test(document.querySelector('main#td-main')?.textContent ?? ''),
        undefined,
        { timeout: 10_000 },
      );
      console.log(`[probe] ${width}: search reachable and submitted OK`);
    }
  } finally {
    await browser.close();
    if (server.pid) {
      try {
        process.kill(-server.pid, 'SIGTERM');
      } catch {
        /* already gone */
      }
    }
  }
  if (failures > 0) {
    console.error(`[probe] ${failures} assertion(s) failed`);
    process.exitCode = 1;
  } else {
    console.log('[probe] all widths OK');
  }
}

main().catch((err) => {
  console.error('[probe] fatal:', err);
  process.exit(1);
});
