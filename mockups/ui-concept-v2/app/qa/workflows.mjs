import assert from "node:assert/strict";
import { existsSync } from "node:fs";
import { chromium } from "playwright";

const base = process.env.BASE_URL ?? "http://127.0.0.1:5195";
const browser = await chromium.launch({
  ...(process.env.CHROMIUM_EXECUTABLE_PATH
    ? { executablePath: process.env.CHROMIUM_EXECUTABLE_PATH }
    : existsSync(chromium.executablePath()) ? {} : { channel: "chrome" }),
});

const px = (value) => Number.parseFloat(value);

try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 }, reducedMotion: "reduce" });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => sessionStorage.clear());
  await page.goto(`${base}/?surface=workflows&data=fixture`);
  await page.locator(".wf-reg-table").waitFor();

  const fontSizes = await page.locator(".wf-reg-table td, .wf-summary p, .wf-run-row, .wf-steps td, .wf-runsteps td, .wf-version-history td").evaluateAll((elements) =>
    elements.map((element) => getComputedStyle(element).fontSize),
  );
  assert(fontSizes.length > 20 && fontSizes.every((size) => px(size) >= 11), "dense registry, inspector and exact-table body text must remain at least 11px");

  for (const selector of [".wf-definition .wf-table-scroll", ".wf-version-scroll", ".wf-run .wf-table-scroll"]) {
    const dimensions = await page.locator(selector).evaluate((element) => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
      overflowX: getComputedStyle(element).overflowX,
    }));
    assert.match(dimensions.overflowX, /auto|scroll/, `${selector} must own horizontal overflow`);
    assert(dimensions.scrollWidth > dimensions.clientWidth, `${selector} must scroll its exact table instead of shrinking it`);
  }

  const selectedRow = page.locator(".wf-reg-table tr[aria-selected=true]");
  assert.match(await selectedRow.innerText(), /enrich-graph/);
  await selectedRow.focus();
  await page.keyboard.press("ArrowDown");
  assert.match(await page.locator(".wf-reg-table tr[aria-selected=true]").innerText(), /cluster-topology/);
  assert.match(await page.locator(":focus").innerText(), /cluster-topology/, "keyboard registry traversal must move focus with selection");
  await page.keyboard.press("ArrowUp");
  assert.match(await page.locator(".wf-reg-table tr[aria-selected=true]").innerText(), /enrich-graph/);

  await page.locator('.wf-node').filter({ hasText: "s3" }).click();
  assert.equal(await page.locator('.wf-node[aria-pressed=true]').getAttribute("aria-pressed"), "true");
  assert.match(await page.locator(".wf-node-inspector").innerText(), /graph\.enrich/);
  assert.equal(await page.locator(".wf-ghost").count(), 2);
  await page.getByRole("button", { name: /v3 GHOST ON/ }).click();
  assert.equal(await page.locator(".wf-ghost").count(), 0, "version comparison must toggle without replacing topology");

  await page.locator(".wf-run-choice").filter({ hasText: "run_wait_4d" }).click();
  assert.match(await page.locator(".wf-track.run").innerText(), /WAITING.*no terminal receipt/s);
  assert.match(await page.locator(".wf-node-inspector").innerText(), /graph\.enrich/, "operation selection must survive run projection changes");
  const runInput = page.getByRole("textbox", { name: "Run ID" });
  await runInput.fill("not-an-authored-run");
  await runInput.press("Enter");
  assert.match(await page.locator(".wf-run-missing").innerText(), /No authored fixture matches this exact run ID/);
  await runInput.fill("run_fail_1b");
  await runInput.press("Enter");
  assert.match(await page.locator(".wf-track.run").innerText(), /FAILED.*failed terminal receipt/s);
  assert.equal(await page.locator(".wf-controls button:disabled").count(), 3, "fixture lifecycle commands must remain unavailable without daemon authority");

  await page.setViewportSize({ width: 1263, height: 931 });
  const medium = await page.locator(".wf-col-run").evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
    overflowY: getComputedStyle(element).overflowY,
  }));
  assert.match(medium.overflowY, /auto|scroll/);
  assert(medium.scrollHeight > medium.clientHeight, "medium run column must scroll to its full ledger and controls");
  await page.locator(".wf-controls").scrollIntoViewIfNeeded();
  const controls = await page.locator(".wf-controls .wf-ctl").evaluateAll((rows) => rows.map((row) => {
    const button = row.querySelector("button").getBoundingClientRect();
    const note = row.querySelector(":scope > span").getBoundingClientRect();
    return { buttonBottom: button.bottom, noteTop: note.top, noteRight: note.right, rowRight: row.getBoundingClientRect().right };
  }));
  assert.equal(controls.length, 3);
  assert(controls.every((control) => control.noteTop >= control.buttonBottom - 1 && control.noteRight <= control.rowRight + 1), "medium lifecycle notes must stack inside their control rows without overlap");

  await page.setViewportSize({ width: 793, height: 931 });
  const narrow = await page.locator(".wf-root").evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
  }));
  assert(narrow.scrollWidth <= narrow.clientWidth + 1, "narrow Workflow surface must not overflow horizontally");
  assert(narrow.scrollHeight > narrow.clientHeight, "narrow Workflow surface must expose all three columns through vertical scrolling");
  await page.locator(".wf-controls").scrollIntoViewIfNeeded();
  assert(await page.locator(".wf-controls").isVisible(), "narrow lifecycle controls must remain reachable");
  await page.locator(".wf-runsteps").scrollIntoViewIfNeeded();
  assert(await page.locator(".wf-runsteps").isVisible(), "narrow exact run table must remain reachable");

  await page.goto(`${base}/?surface=workflows&data=snapshot`);
  await page.locator(".wf-snapshot").waitFor();
  assert.equal(await page.locator(".wf-reg-table, .wf-node, .wf-run-choice").count(), 0, "snapshot must not render authored fixture authorities");
  assert.match(await page.locator(".wf-snapshot").innerText(), /WORKFLOW AUTHORITIES UNAVAILABLE.*workflow registry not served/s);
  assert.deepEqual(errors, []);
  console.log("PASS Workflow readable exact records, local overflow, canonical selection, run states, unavailable lifecycle and narrow reachability");
} finally {
  await browser.close();
}
