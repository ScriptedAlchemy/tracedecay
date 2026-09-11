import assert from 'node:assert/strict';
import { chromium } from 'playwright';

const browser = await chromium.launch();
try {
  const page = await browser.newPage({viewport:{width:1280,height:853},reducedMotion:'reduce'});
  const base = process.env.BASE_URL ?? 'http://127.0.0.1:5195';
  const errors=[]; page.on('pageerror', error=>errors.push(error.message));
  const time = clock => Date.parse(`2025-05-12T${clock}Z`)/1000;
  await page.goto(`${base}/?surface=loom&loom_source=design&state=01`);
  await page.getByRole('button',{name:'Previous event',exact:true}).click();
  assert.equal(new URL(page.url()).searchParams.get('loom_replay'),'1','stepping away from the tail enters replay');
  assert.equal(new URL(page.url()).searchParams.get('loom_follow'),'0');
  assert.match(await page.getByRole('status').innerText(),/Paused at/);
  const field = await page.locator('.journey-field').boundingBox();
  const moment = await page.locator('.journey-moment').boundingBox();
  assert.ok(field.x + field.width <= moment.x, 'the moment panel must not obscure the replay canvas');

  for (const label of await page.locator('.journey-event > span').all()) assert(await label.evaluate(el=>el.scrollWidth<=el.clientWidth+1),'Close-view captions must wrap within their event spacing');

  const url = new URL(`${base}/?surface=loom&loom_source=design&state=04&loom_replay=1&loom_follow=1&loom_event=design:event:timeline-file-edited`);
  url.searchParams.set('loom_time',String(time('14:30:00')));
  url.searchParams.set('loom_query','TimelineCanvas.tsx');
  await page.goto(url.href);
  await page.locator('.journey-field canvas').waitFor();
  assert.equal(new URL(page.url()).searchParams.get('loom_follow'),'0','replay and follow cannot coexist after restoring a URL');
  assert.equal(new URL(page.url()).searchParams.get('loom_event'),'','restoring replay cannot select an unrevealed event');
  assert.equal(await page.locator('.journey-group').count(),0,'a future file reference must not make its agent searchable');
  assert.equal(await page.locator('.loom-fallback').first().locator('tbody tr').count(),0);
  url.searchParams.set('loom_time',String(time('14:32:17.803')));
  await page.goto(url.href);
  await page.locator('.journey-group').first().waitFor();
  assert.match(await page.locator('.loom-head').innerText(),/1 UNIQUE AGENTS.*2 WORKSTREAM PARTICIPATIONS/);
  assert.equal(await page.locator('.journey-group').count(),2,'the revealed identity retains both workstream memberships');

  url.searchParams.delete('loom_query'); url.searchParams.delete('loom_event');
  url.searchParams.set('loom_time',String(time('14:30:00')));
  url.searchParams.set('loom_unresolved_only','1');
  await page.goto(url.href);
  await page.locator('.journey-group').first().waitFor();
  assert.match(await page.locator('.loom-head').innerText(),/103 UNIQUE AGENTS.*123 WORKSTREAM PARTICIPATIONS/,'a future session end cannot resolve a session during replay');
  await page.getByRole('button',{name:'Play replay',exact:true}).click();
  const announcement = await page.getByRole('status').innerText();
  const start = Number(new URL(page.url()).searchParams.get('loom_time'));
  await page.waitForFunction(before => Number(new URLSearchParams(location.search).get('loom_time')) > before, start);
  assert.equal(await page.getByRole('status').innerText(),announcement,'screen readers must not announce every playback tick');
  await page.getByRole('button',{name:'Pause replay',exact:true}).click();
  url.searchParams.set('loom_time',String(time('14:42:00')));
  await page.goto(url.href);
  await page.locator('.journey-field canvas').waitFor();
  assert.equal(await page.locator('.journey-group').count(),0,'recorded ends resolve sessions once revealed');
  const gapUrl = new URL(`${base}/?surface=loom&loom_source=design&state=07&loom_replay=1`);
  gapUrl.searchParams.set('loom_time',String(time('14:24:10')));
  await page.goto(gapUrl.href);
  await page.locator('.journey-field canvas').waitFor();
  assert.doesNotMatch(await page.locator('.journey-ruler').textContent(),/TRANSCRIPT/,'coverage finding stays withheld before its boundary record');
  gapUrl.searchParams.set('loom_time',String(time('14:24:11')));
  gapUrl.searchParams.set('loom_event','design:event:transcript-capture-gap');
  await page.goto(gapUrl.href);
  await page.locator('.journey-field canvas').waitFor();
  assert.match(await page.locator('.journey-ruler').textContent(),/TRANSCRIPT NOT INGESTED/);
  await page.getByRole('button',{name:'OPEN SOURCE DETAILS',exact:true}).click();
  assert.match(await page.locator('.loom-gap-evidence').innerText(),/14:12:29 to 14:24:11/,'hatched range resolves to its authored coverage record');
  assert.deepEqual(errors,[]);
  console.log('PASS: replay transitions, non-overlapping moment panel, cutoff-safe search and unresolved filters, URL restoration and quiet playback announcements.');
} finally { await browser.close(); }
