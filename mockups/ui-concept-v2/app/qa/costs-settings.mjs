import assert from "node:assert/strict";
import { chromium } from "playwright";

const baseUrl = process.env.BASE_URL ?? "http://127.0.0.1:5195";
const browser = await chromium.launch({ headless: true });

async function open(path, viewport = { width: 1586, height: 992 }) {
  const context = await browser.newContext({ viewport });
  const page = await context.newPage();
  const errors = [];
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto(`${baseUrl}/${path}`, { waitUntil: "networkidle" });
  return { context, page, errors };
}

async function costsFlow() {
  const { context, page, errors } = await open("?surface=costs&data=fixture");
  try {
    assert.equal(await page.locator(".cs-flow-row.is-priced").count(), 1);
    assert.equal(await page.locator(".cs-flow-row.is-unpriced").count(), 1);
    assert.equal(await page.locator(".cs-flow-row.is-gap").count(), 1);
    assert.match(
      await page.locator(".cs-budget-boundary").innerText(),
      /\$18\.72 attributed priced.*59% project-token pricing coverage/s,
    );
    assert.equal(await page.locator(".cs-fixture-series").count(), 1);
    assert.match(await page.locator(".cs-plot").getAttribute("aria-label"), /Cumulative priced spend.*Anthropic event.*unpriced/);
    assert.match(await page.locator(".cs-hint").innerText(), /covers 65% of measured tokens/);
    const range = page.getByRole("button", { name: "RANGE · 15 MIN" });
    await range.click();
    assert.match(await page.getByRole("status").innerText(), /ONLY SERVED WINDOW[\s\S]*Alternate windows are not present/);
    await range.press("Escape");
    assert.equal(await page.getByRole("status").count(), 0);

    const unpriced = page.locator(".cs-flow-row.is-unpriced");
    assert.match(await unpriced.innerText(), /1\.96M tokens/);
    assert.match(await unpriced.innerText(), /spend unpriced/);
    await unpriced.click();
    assert.match(page.url(), /flow=evt-a044/);
    assert.match(page.url(), /provider=Anthropic/);
    assert.equal(await page.locator(".cs-flow-row.is-dim").count(), 2);
    assert.match(
      await page.locator(".cs-flow-inspect").innerText(),
      /evt-a044[\s\S]*run-707-tests[\s\S]*fixture\/usage-ledger\/evt-a044/,
    );
    const main = await page.locator(".cs-main").evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
    }));
    const detail = await page.locator(".cs-detail").boundingBox();
    const root = await page.locator(".cs-root").boundingBox();
    const topology = await page.locator(".cs-mid-well").nth(2).boundingBox();
    const topologyTotal = await page.locator(".cs-mid-well").nth(2).locator("tr.total").boundingBox();
    assert.equal(main.scrollHeight, main.clientHeight);
    assert.ok(detail && root && detail.y + detail.height <= root.y + root.height);
    assert.ok(topology && topologyTotal && topologyTotal.y + topologyTotal.height <= topology.y + topology.height);
    assert.deepEqual(errors, []);
  } finally {
    await context.close();
  }
}

async function costsRouteAuthority() {
  const { context, page, errors } = await open("?surface=costs&data=fixture");
  try {
    await page.locator(".cs-flow-row").first().click();
    await page.locator('.cs-cov-table tr[data-provider="Anthropic"]').click();
    assert.equal(await page.locator(".cs-flow-row.is-selected").count(), 0);
    assert.match(page.url(), /provider=Anthropic/);
    assert.equal(new URL(page.url()).searchParams.has("flow"), false);
    await page.locator('.cs-cov-table tr[data-provider="OpenAI"]').click();
    assert.equal(await page.locator(".cs-flow-row.is-selected").count(), 0);
    assert.match(page.url(), /provider=OpenAI/);
    assert.equal(new URL(page.url()).searchParams.has("flow"), false);

    await page.locator(".cs-flow-row").first().click();
    await page.goto(`${baseUrl}/?surface=costs&data=fixture&provider=Anthropic`, { waitUntil: "networkidle" });
    assert.equal(await page.locator(".cs-flow-row.is-selected").count(), 0);
    assert.equal(await page.locator(".cs-flow-row.is-dim").count(), 2);
    assert.match(await page.locator(".cs-flow-inspect").innerText(), /Select a flow/);
    assert.match(await page.locator(".cs-cov-table tr.is-selected").innerText(), /Anthropic/);

    await page.locator(".cs-flow-row.is-unpriced").click();
    await page.reload({ waitUntil: "networkidle" });
    assert.match(await page.locator(".cs-flow-inspect").innerText(), /evt-a044[\s\S]*run-707-tests/);
    assert.match(await page.locator(".cs-cov-table tr.is-selected").innerText(), /Anthropic/);

    await page.goto(`${baseUrl}/?surface=costs&data=fixture&provider=Anthropic&flow=evt-91c2`, { waitUntil: "networkidle" });
    assert.equal(await page.locator(".cs-flow-row.is-selected").count(), 0);
    assert.match(await page.locator(".cs-flow-inspect").innerText(), /Select a flow/);
    assert.match(await page.locator(".cs-cov-table tr.is-selected").innerText(), /Anthropic/);
    await page.locator(".cs-flow-row").first().focus();
    await page.keyboard.press("Escape");
    assert.equal(new URL(page.url()).searchParams.has("flow"), false);
    assert.match(page.url(), /provider=Anthropic/);
    await page.keyboard.press("Escape");
    assert.equal(new URL(page.url()).searchParams.has("provider"), false);
    assert.equal(await page.locator(".cs-flow-row.is-dim").count(), 0);

    await page.goto(`${baseUrl}/?surface=costs&data=fixture&flow=unknown-flow`, { waitUntil: "networkidle" });
    assert.equal(await page.locator(".cs-flow-row.is-selected").count(), 0);
    assert.equal(await page.locator(".cs-flow-row.is-dim").count(), 0);
    assert.equal(await page.locator(".cs-cov-table tr.is-selected").count(), 0);
    await page.locator(".cs-flow-row").first().focus();
    await page.keyboard.press("Escape");
    assert.equal(new URL(page.url()).searchParams.has("flow"), false);
    assert.deepEqual(errors, []);
  } finally {
    await context.close();
  }
}

async function settingsLifecycle() {
  const { context, page, errors } = await open("?surface=settings&data=fixture");
  try {
    assert.match(await page.locator(".st-layer.is-winner").innerText(), /0\.82[\s\S]*winning path/);
    assert.ok((await page.locator(".st-layer.is-overridden").count()) >= 1);
    assert.match(await page.locator(".st-row").first().innerText(), /local only[\s\S]*immediate/);

    const input = page.getByRole("textbox", { name: /Proposed value/ });
    await input.fill("0.84");
    assert.equal(await page.locator(".st-stations .is-preview").count(), 1);
    await page.getByRole("button", { name: "STALE REVISION" }).click();
    await page.getByRole("button", { name: "APPLY LOCAL FIXTURE" }).click();
    assert.match(
      await page.locator(".st-impact").innerText(),
      /VALIDATION\s+valid[\s\S]*PERSISTED\s+conflicted · stale revision[\s\S]*READ-BACK\s+not run/,
    );
    await page.waitForFunction(() => document.activeElement?.classList.contains("st-row"));
    assert.match(await page.locator(".st-row.is-sel").innerText(), /0\.82/);
    assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("st-row")), true);

    await page.getByRole("button", { name: "STALE REVISION" }).click();
    await page.getByRole("button", { name: "APPLY LOCAL FIXTURE" }).click();
    assert.match(
      await page.locator(".st-impact").innerText(),
      /PERSISTED\s+persisted[\s\S]*READ-BACK\s+confirmed[\s\S]*RUNTIME\s+adopted/,
    );
    await page.waitForFunction(() => document.activeElement?.classList.contains("st-row"));
    assert.match(await page.locator(".st-card").first().innerText(), /fixture-r13/);
    assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("st-row")), true);

    await page.getByRole("button", { name: /code\.index\.refresh_interval/ }).click();
    await page.getByRole("textbox", { name: /Proposed value/ }).fill("20m");
    await page.getByRole("button", { name: "APPLY LOCAL FIXTURE" }).click();
    assert.match(await page.locator(".st-impact").innerText(), /RUNTIME\s+reindex required/);
    await page.getByRole("button", { name: "MARK ADOPTED" }).click();
    assert.match(await page.locator(".st-impact").innerText(), /RUNTIME\s+adopted/);

    await page.getByRole("button", { name: /security\.redact\.secrets/ }).click();
    await page.getByRole("textbox", { name: /Proposed value/ }).fill("banana");
    assert.match(await page.locator(".st-impact").innerText(), /VALIDATION\s+invalid/);
    assert.ok(await page.getByRole("button", { name: "APPLY LOCAL FIXTURE" }).isDisabled());
    assert.match(await page.locator(".st-row.is-sel").innerText(), /true/);
    assert.deepEqual(errors, []);
  } finally {
    await context.close();
  }
}

async function snapshotTruth() {
  const costs = await open("?surface=costs&data=snapshot");
  try {
    assert.equal(await costs.page.locator(".cs-flow-row.is-unavailable").count(), 3);
    assert.equal(await costs.page.locator(".cs-fixture-series").count(), 0);
    assert.equal((await costs.page.locator(".cs-flow-list").innerText()).includes("$"), false);
    assert.match(await costs.page.locator(".cs-flow-list").innerText(), /spend unavailable/);
    assert.equal((await costs.page.locator(".cs-flow-list").innerText()).includes("spend unpriced"), false);
    assert.match(await costs.page.locator(".cs-hint").innerText(), /Pricing coverage is unavailable/);
    const firstSeries = costs.page.locator(".cs-tracks .ln").first();
    const hitTarget = await firstSeries.evaluate((element) => ({ height: element.clientHeight, pointerEvents: getComputedStyle(element).pointerEvents }));
    assert.ok(hitTarget.height >= 10);
    assert.equal(hitTarget.pointerEvents, "auto");
    await firstSeries.focus();
    await firstSeries.press("ArrowDown");
    assert.equal(await costs.page.locator(".cs-tracks .ln").nth(1).getAttribute("aria-pressed"), "true");
    assert.equal(await costs.page.evaluate(() => document.activeElement?.dataset?.provider), "cursor");
    assert.deepEqual(costs.errors, []);
  } finally {
    await costs.context.close();
  }

  const settings = await open("?surface=settings&data=snapshot");
  try {
    assert.match(await settings.page.locator(".st-row").first().innerText(), /unserved\s+unserved$/);
    assert.ok(await settings.page.getByRole("button", { name: /APPLY UNAVAILABLE/ }).isDisabled());
    assert.deepEqual(settings.errors, []);
  } finally {
    await settings.context.close();
  }
}

async function compactLayout() {
  const costs = await open("?surface=costs&data=fixture", { width: 793, height: 700 });
  try {
    const canvas = costs.page.locator(".cs-flow");
    const dimensions = await canvas.evaluate((element) => ({
      width: element.clientWidth,
      scrollWidth: element.scrollWidth,
      overflowX: getComputedStyle(element).overflowX,
    }));
    assert.ok(dimensions.scrollWidth > dimensions.width);
    assert.equal(dimensions.overflowX, "auto");
    await canvas.evaluate((element) => { element.scrollLeft = element.scrollWidth; });
    assert.ok(await canvas.evaluate((element) => element.scrollLeft > 0));
    assert.deepEqual(costs.errors, []);
  } finally {
    await costs.context.close();
  }

  const settings = await open("?surface=settings&data=fixture", { width: 793, height: 700 });
  try {
    const selector = await settings.page.locator(".st-section-menu select").boundingBox();
    const search = await settings.page.locator(".st-search").boundingBox();
    assert.ok(selector && search && selector.y + selector.height <= search.y);
    assert.equal(await settings.page.locator(".st-stations > span").count(), 5);
    assert.deepEqual(settings.errors, []);
  } finally {
    await settings.context.close();
  }
}

try {
  await costsFlow();
  await costsRouteAuthority();
  await settingsLifecycle();
  await snapshotTruth();
  await compactLayout();
  console.log("Costs and Settings QA passed");
} finally {
  await browser.close();
}
