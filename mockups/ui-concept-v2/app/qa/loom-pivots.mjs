import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { chromium } from 'playwright';

const base = process.env.BASE_URL ?? 'http://127.0.0.1:5195';
const pack = JSON.parse(readFileSync(new URL('../src/data/pack-index.json', import.meta.url)));
const ubuntu = JSON.parse(readFileSync(new URL('../profile-pack/ubuntu-main/proj_a5b3d7e3ebe14ca7/sessions.json', import.meta.url)));
const browser = await chromium.launch();
try {
  const page = await browser.newPage({viewport:{width:1280,height:720},reducedMotion:'reduce'});
  const errors=[]; page.on('pageerror', error=>errors.push(error.message));
  const open = async url => { await page.goto(url); await page.locator('.loom-destination').waitFor(); };
  const record = async () => JSON.parse(await page.getByTestId('loom-destination-source').textContent());
  await page.goto(`${base}/?surface=loom&state=05&loom_source=design`);
  await page.getByTestId('loom-event-source').waitFor({state:'attached'});
  const event=JSON.parse(await page.getByTestId('loom-event-source').textContent());
  await page.getByRole('group',{name:'Evidence modes',exact:true}).getByRole('button',{name:'CODE & IMPACT',exact:true}).click();
  const returnTo=page.url();
  for (const surface of ['SESSIONS','AGENTS','WORK','CODE','DELIVERY']) {
    const bounds=await page.getByRole('link',{name:`OPEN ${surface} ↗`,exact:true}).boundingBox();
    assert(bounds && bounds.y+ bounds.height < 700, `${surface} pivot must fit the initial evidence workspace`);
  }

  for (const surface of ['SESSIONS','AGENTS','WORK','CODE','DELIVERY']) {
    await page.getByRole('link',{name:`OPEN ${surface} ↗`,exact:true}).click();
    assert.equal((await record()).id,event.id);
    assert.equal((await record()).sessionId,event.sessionId);
    assert.equal(new URL(page.url()).searchParams.get('loom_page'),'full');
    assert.equal(await page.locator('.loom-destination-native').count(),0,'Synthetic source must not mount Mac data');
    if(surface==='CODE') assert.match(await page.locator('.loom-destination-diff').innerText(),/event.forceRender/);
    if(surface==='DELIVERY') assert.match(await page.locator('.loom-destination-record').innerText(),/No exact PR/);
    await page.getByRole('link',{name:'RETURN TO LOOM',exact:true}).click();
    await page.getByTestId('loom-event-source').waitFor({state:'attached'});
    assert.equal(page.url(),returnTo,'Return must restore all original navigation parameters');
  }
  console.log('PASS all five destinations retain source identity and exact return view');

  const replay=new URL(returnTo); replay.searchParams.set('loom_replay','1'); replay.searchParams.set('loom_follow','0'); replay.searchParams.set('loom_time',String(event.ts));
  await page.goto(replay.href); await page.getByTestId('loom-event-source').waitFor({state:'attached'});
  const replayReturn=page.url();
  await page.getByRole('link',{name:'OPEN SESSIONS ↗',exact:true}).click();
  const pivotUrl=page.url();
  assert.match(await page.locator('.loom-destination-record').innerText(),/Not revealed at this replay cursor/);
  const times=await page.locator('.loom-destination aside small').allTextContents();
  assert(times.every(time=>Date.parse(time)/1000<=event.ts));
  await page.locator('.loom-destination aside a').first().click();
  assert.notEqual((await record()).id,event.id);
  await page.getByRole('navigation',{name:'Selected source destinations'}).getByRole('link',{name:'CODE',exact:true}).click();
  await page.getByRole('link',{name:'RETURN TO LOOM',exact:true}).click();
  await page.getByTestId('loom-event-source').waitFor({state:'attached'}); assert.equal(page.url(),replayReturn);
  const invalidCases=[['loom_target_event','design:event:timeline-test'],['loom_target_session','design:session:agent-001'],['loom_target_event','missing'],['loom_source','unknown'],['loom_page','invalid'],['loom_time','NaN']];
  for(const [key,value] of invalidCases) {const url=new URL(pivotUrl);url.searchParams.set(key,value);await open(url.href);assert.equal(await page.getByRole('alert').count(),1,`${key}=${value} must fail closed`);assert.equal(await page.getByTestId('loom-destination-source').count(),0);}
  const external=new URL(pivotUrl);external.searchParams.set('loom_return','https://example.org/?surface=loom');await open(external.href);
  const safe=new URL(await page.getByRole('link',{name:'RETURN TO LOOM'}).getAttribute('href'),base);assert.equal(safe.origin,new URL(base).origin);
  console.log('PASS replay withholds future records; invalid identity, page, cursor and external return cannot substitute sources');

  const route=(surface,source,session)=>`${base}/?${new URLSearchParams({surface,loom_source:source,loom_pivot:'1',loom_target_session:session})}`;
  const last=pack.sessions.at(-1);
  await open(route('sessions','mac',last.id));
  assert.equal(await page.locator(`tr[aria-selected=true] [title="${last.id}"]`).count(),1);
  await open(route('agents','mac',last.id));
  assert.equal(await page.locator(`.ag-row [title="${last.id}"]`).count(),1);
  const families=[...new Set(pack.sessions.filter(s=>s.parentId).map(s=>s.parentId))];
  const child=pack.sessions.find(s=>s.parentId===families.at(-1));
  await open(route('work','mac',child.id));
  assert.equal(await page.locator('.loom-destination-native').count(),1);
  assert.equal(await page.locator('.wk-thread-glyph[aria-pressed=true]').getAttribute('title'),child.id);
  const singleton=pack.sessions.find(s=>!s.parentId&&!families.includes(s.id));
  await open(route('work','mac',singleton.id));assert.equal(await page.locator('.loom-destination-native').count(),0);assert.equal((await record()).id,singleton.id);
  const row=ubuntu.at(-1);
  await open(route('sessions','ubuntu',row.session_id)); assert.equal((await record()).id,row.session_id);
  assert.equal(await page.locator('.loom-destination-native').count(),0);assert.match(await page.locator('.loom-destination-record').innerText(),/No event spine or transcript body/);
  await page.setViewportSize({width:640,height:426});
  assert(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'Source view must reflow without document horizontal overflow');
  await page.getByRole('link',{name:'RETURN TO LOOM'}).focus();assert.equal(await page.locator(':focus').textContent(),'RETURN TO LOOM');
  assert.deepEqual(errors,[]);
  console.log('PASS native exact Mac selections, isolated Ubuntu identity view and narrow keyboard return');
} finally { await browser.close(); }
