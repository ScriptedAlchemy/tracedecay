import { chromium } from "playwright";

const baseUrl = process.env.BASE_URL ?? "http://127.0.0.1:5173";
const ROUTES = [
  "/?view=overview&dim=2d",
  "/?view=hover&dim=2d",
  "/?view=repo-zoom&dim=2d",
  "/?view=scoped&dim=2d",
  "/?view=synapse&dim=2d",
  "/?view=overview&dim=3d",
  "/?view=scoped&dim=3d",
  "/?view=scoped&scope=proj_e19f6f383c982ea8&dim=3d",
  "/?view=synapse&dim=3d",
  "/?view=firing-tree",
  "/?view=neuron-lab",
];

const browser = await chromium.launch({
  args: ["--no-sandbox"],
});
const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
let failures = 0;
for (const route of ROUTES) {
  const errors = [];
  const onErr = (e) => errors.push(String(e));
  const onConsole = (m) => m.type() === "error" && errors.push(m.text());
  page.on("pageerror", onErr);
  page.on("console", onConsole);
  await page.goto(`${baseUrl}${route}&data=fixture`, { waitUntil: "networkidle" });
  await page.waitForTimeout(1200);
  page.off("pageerror", onErr);
  page.off("console", onConsole);
  if (errors.length) {
    failures++;
    console.log(`FAIL ${route}\n  ${errors.join("\n  ")}`);
  } else {
    console.log(`ok   ${route}`);
  }
}
await browser.close();
process.exit(failures ? 1 : 0);
