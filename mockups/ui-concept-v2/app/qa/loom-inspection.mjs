import assert from 'node:assert/strict';
import fs from 'node:fs';
import {chromium} from 'playwright';

const base=process.env.BASE_URL ?? 'http://127.0.0.1:5195';
const browser=await chromium.launch();
try {
  const page=await browser.newPage({viewport:{width:1280,height:720},reducedMotion:'reduce'});
  const errors=[]; page.on('pageerror',error=>errors.push(error.message));
  const params=()=>new URL(page.url()).searchParams;
  const pack=JSON.parse(fs.readFileSync(new URL('../src/data/pack-index.json',import.meta.url)));
  const [first,second,third]=pack.loomEvents;
  // These exported messages form one two-record episode followed by a later message.
  assert.equal(first.kind,second.kind); assert.equal(first.sessionId,second.sessionId);
  assert(second.ts-first.ts<60 && third.ts-first.ts>=60);
  const route=new URL(`${base}/?surface=loom&state=02&loom_zoom=event&loom_replay=1&loom_follow=0`);
  route.searchParams.set('loom_session',first.sessionId);
  route.searchParams.set('loom_from',String(first.ts)); route.searchParams.set('loom_to',String(third.ts));
  route.searchParams.set('loom_event',first.id); route.searchParams.set('loom_time',String(second.ts+1));
  await page.goto(route.href);
  await page.getByRole('button',{name:'Next event',exact:true}).click();
  assert.equal(params().get('loom_event'),third.id,'Next advances from the cursor after playback has passed a selected event');
  route.searchParams.set('loom_time',String(first.ts)); route.searchParams.set('loom_zoom','episode');
  await page.goto(route.href);
  await page.getByRole('button',{name:'Next event',exact:true}).click();
  assert.equal(params().get('loom_event'),third.id,'Episode stepping skips the remaining messages in the current episode');

  for(const resolved of [true,false]) {
    const session=pack.sessions.find(s=>s.parentId && pack.sessions.some(parent=>parent.id===s.parentId)===resolved);
    assert(session);
    await page.goto(`${base}/?surface=loom&state=03&loom_query=${session.id}`);
    const tree=page.locator('.loom-fallback').last();
    await tree.locator('summary').click();
    const row=tree.locator('tbody tr').filter({hasText:session.id});
    assert.equal(await row.locator('.loom-grade').innerText(),resolved?'EXACT':'UNAVAILABLE');
    await row.getByRole('button',{name:session.id,exact:true}).click();
    assert.equal(await row.getAttribute('aria-selected'),'true');
  }
  await page.goto(`${base}/?surface=loom&loom_source=design&state=03&loom_replay=1&loom_time=${Date.parse('2025-05-12T09:50:00Z')/1000}`);
  await page.getByRole('button',{name:'COLLAPSE CHILDREN',exact:true}).click();
  assert.match(await page.getByRole('button',{name:/^Collapsed children/}).innerText(),/future not revealed/);
  assert.match(await page.locator('.journey-session-evidence').first().innerText(),/revealed · relations:/);

  await page.goto(`${base}/?surface=loom&loom_source=design&state=05`);
  await page.getByRole('slider',{name:'Evidence pane width',exact:true}).fill('60');
  await page.getByRole('tab',{name:'CONTEXT',exact:true}).click();
  await page.getByRole('button',{name:'FOCUS EXACT SOURCE',exact:true}).click();
  await page.getByRole('button',{name:'FEEDBACK',exact:true}).click();
  await page.reload();
  await page.getByRole('button',{name:'BACK TO EVIDENCE',exact:true}).click();
  assert.equal(await page.getByRole('button',{name:'RESTORE SPLIT',exact:true}).count(),1);
  await page.getByRole('button',{name:'RESTORE SPLIT',exact:true}).click();
  assert.equal(await page.getByRole('slider',{name:'Evidence pane width',exact:true}).inputValue(),'60');
  assert.equal(await page.getByRole('tab',{name:'CONTEXT',exact:true}).getAttribute('aria-selected'),'true');
  await page.getByRole('slider',{name:'Evidence pane width',exact:true}).fill('48');
  await page.getByRole('button',{name:'Review illustrated diff line 5',exact:true}).click();
  assert.equal(params().get('loom_feedback_line'),'4');
  await page.reload();
  assert.equal(await page.getByLabel('Feedback target',{exact:true}).inputValue(),'4');
  assert.match(await page.getByTestId('loom-feedback-target-line').innerText(),/!event.forceRender/);
  const workspace=await page.locator('.loom-fb').boundingBox();
  const save=await page.getByRole('button',{name:'SAVE LOCALLY',exact:true}).boundingBox();
  assert(save.y+save.height<=workspace.y+workspace.height+1,'Line-targeted feedback remains reachable in the initial workspace');
  await page.getByLabel('Comment / evidence rationale').fill('Review this exact illustrated condition.');
  await page.getByRole('button',{name:'SAVE LOCALLY',exact:true}).click();
  const records=()=>page.evaluate(()=>JSON.parse(localStorage.getItem('td-loom-local-review')));
  const original=(await records())[0]; assert.equal(original.diffLine,4);
  await page.getByLabel('Feedback target',{exact:true}).selectOption('1');
  await page.getByRole('combobox',{name:/^Lifecycle/}).selectOption('acknowledged');
  await page.getByLabel('Feedback to update').selectOption(original.id);
  await page.getByLabel('Comment / evidence rationale').fill('Reviewed the original target.');
  await page.getByRole('button',{name:'SAVE LOCALLY',exact:true}).click();
  assert.deepEqual((await records())[0],original);
  assert.equal((await records())[1].diffLine,4,'Lifecycle inherits the original hunk instead of the composer selection');
  const invalid=JSON.stringify([{...original,diffLine:999}]);
  await page.evaluate(value=>localStorage.setItem('td-loom-local-review',value),invalid);
  await page.reload();
  await page.getByLabel('Comment / evidence rationale').fill('Must not overwrite unreadable notes.');
  await page.getByRole('button',{name:'SAVE LOCALLY',exact:true}).click();
  assert.match(await page.getByRole('alert').innerText(),/Could not save locally/);
  assert.equal(await page.evaluate(()=>localStorage.getItem('td-loom-local-review')),invalid);
  await page.evaluate(()=>localStorage.removeItem('td-loom-local-review'));
  await page.goto(`${base}/?surface=loom&state=06&loom_feedback_line=4`);
  await page.getByLabel('Comment / evidence rationale').waitFor();
  assert.equal(await page.getByLabel('Feedback target',{exact:true}).count(),0,'Metadata-only sources have no invented diff target');
  assert.equal(params().get('loom_feedback_line'),'');
  assert.deepEqual(errors,[]);
  console.log('PASS cursor and episode stepping, branch evidence and selection, withheld counts, restored evidence layout and durable diff-line feedback');
} finally {await browser.close();}
