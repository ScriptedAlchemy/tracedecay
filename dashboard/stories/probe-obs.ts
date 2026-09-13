/** Throwaway probe: what inflates the Observatory document height? */
import { chromium } from '@playwright/test';
import { startStaticServer, STILLNESS_INIT } from '../e2e/static-server.ts';
import { installApiFixtures } from './fixtures/route.ts';

async function main(): Promise<void> {
  const { server, baseURL } = startStaticServer();
  const browser = await chromium.launch({ headless: true });
  const page = await browser.newPage({ viewport: { width: 768, height: 900 } });
  await installApiFixtures(page);
  await page.addInitScript({ content: STILLNESS_INIT });
  await page.goto(`${baseURL}/observatory`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('main#td-main', { timeout: 30_000 });
  await page.waitForTimeout(2000);
  const shrink = await page.evaluate(() => {
    const doc = document.documentElement;
    const main = document.querySelector('main#td-main');
    const page1 = main?.firstElementChild;
    const sections = page1 ? Array.from(page1.children) : [];
    const readings: string[] = [];
    readings.push(`initial scrollHeight=${doc.scrollHeight}`);
    for (const section of sections) {
      const label =
        section.getAttribute('aria-label') ??
        section.querySelector('h2, h1')?.textContent?.slice(0, 40) ??
        section.tagName;
      (section as HTMLElement).style.display = 'none';
      readings.push(`hid ${label} -> ${doc.scrollHeight}`);
    }
    return readings;
  });
  console.log(shrink.join('\n'));

  const report = await page.evaluate(() => {
    const doc = document.documentElement;
    const rows: { desc: string; bottom: number; h: number }[] = [];
    for (const el of Array.from(document.querySelectorAll('*'))) {
      const rect = el.getBoundingClientRect();
      const tag = el.tagName.toLowerCase();
      const cls = (el.getAttribute('class') ?? '').slice(0, 90);
      rows.push({ desc: `${tag}.${cls}`, bottom: Math.round(rect.bottom), h: Math.round(rect.height) });
    }
    rows.sort((a, b) => b.bottom - a.bottom);
    // Which elements are NOT inside a scroll container (i.e. extend the
    // document itself)? Walk up from each element; if no ancestor scrolls,
    // its bottom contributes to documentElement.scrollHeight.
    const documentLevel: { desc: string; bottom: number; h: number }[] = [];
    for (const el of Array.from(document.querySelectorAll('*'))) {
      const rect = el.getBoundingClientRect();
      if (rect.bottom < 1000) continue;
      let clipped = false;
      let node = el.parentElement;
      while (node) {
        const style = getComputedStyle(node);
        if (/(auto|scroll|hidden|clip)/.test(style.overflowY)) {
          clipped = true;
          break;
        }
        node = node.parentElement;
      }
      if (!clipped) {
        documentLevel.push({
          desc: `${el.tagName.toLowerCase()}.${(el.getAttribute('class') ?? '').slice(0, 90)}`,
          bottom: Math.round(rect.bottom),
          h: Math.round(rect.height),
        });
      }
    }
    documentLevel.sort((a, b) => b.bottom - a.bottom);
    return {
      scrollHeight: doc.scrollHeight,
      bodyScrollHeight: document.body.scrollHeight,
      documentLevel: documentLevel.slice(0, 10),
      bodyChildren: Array.from(document.body.children).map((child) => {
        const rect = child.getBoundingClientRect();
        return `${child.tagName.toLowerCase()}#${child.id}.${(child.getAttribute('class') ?? '').slice(0, 60)} bottom=${Math.round(rect.bottom)}`;
      }),
    };
  });
  console.log(JSON.stringify(report, null, 2));
  await browser.close();
  server.close();
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
