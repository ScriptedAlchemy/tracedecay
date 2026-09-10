import assert from "node:assert/strict";
import { chromium } from "playwright";

const base = process.env.BASE_URL ?? "http://127.0.0.1:5195";
const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1586, height: 992 }, reducedMotion: "reduce" });
  const errors = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.addInitScript(() => {
    for (const state of ["08", "09", "10"]) {
      sessionStorage.setItem(`td:view:fixture:delivery:${state}:feedback`, JSON.stringify(
        { id: `current-${state}`, kind: "challenge", body: `current ${state}`, anchor: `fixture:${state}:current`, revision: "def456", lifecycle: "open" },
      ));
      sessionStorage.setItem(`td:view:fixture:delivery:${state}:feedback-history`, JSON.stringify([
        {},
        { id: `valid-${state}`, kind: "comment", body: `retained ${state}`, anchor: `fixture:${state}`, revision: "abc123", lifecycle: "open" },
      ]));
    }
  });

  for (const state of ["08", "09", "10"]) {
    await page.goto(`${base}/?surface=delivery&data=fixture&state=${state}`);
    if (state !== "08") await page.getByRole("button", { name: "FEEDBACK", exact: true }).click();
    const history = page.getByText("EARLIER LOCAL FEEDBACK (1)", { exact: true });
    await history.waitFor();
    assert.equal(await page.getByText(`comment: retained ${state}`, { exact: true }).count(), 1);
    assert.doesNotMatch(await history.locator("..").innerText(), /undefined/);
    await page.waitForFunction((scope) => {
      const current = JSON.parse(sessionStorage.getItem(`td:view:fixture:delivery:${scope}:feedback`) ?? "null");
      const saved = JSON.parse(sessionStorage.getItem(`td:view:fixture:delivery:${scope}:feedback-history`) ?? "[]");
      return current?.id === `current-${scope}` && saved.length === 1 && saved[0]?.id === `valid-${scope}`;
    }, state);
  }
  assert.deepEqual(errors, []);
  console.log("PASS malformed local feedback and history are rejected across Delivery review states 08, 09 and 10");
} finally {
  await browser.close();
}
