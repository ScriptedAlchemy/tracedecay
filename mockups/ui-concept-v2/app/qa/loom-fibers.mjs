import assert from 'node:assert/strict';
import { chromium } from 'playwright';
import { mkdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
const browser=await chromium.launch();
const base=process.env.BASE_URL ?? 'http://127.0.0.1:5195';
const output=process.env.LOOM_SCREENSHOTS ?? join(tmpdir(),'td-loom-fibers');
await mkdir(output,{recursive:true});
try {
 const page=await browser.newPage({viewport:{width:1280,height:853},reducedMotion:'reduce'}),errors=[];
 page.on('pageerror',error=>errors.push(error.message));
 await page.goto(`${base}/?surface=loom&data=fixture&loom_source=design&state=04`);
 await page.locator('.journey-spine-label').waitFor();
 const painted=()=>page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
 await painted();
 assert.match(await page.locator('.journey-spine-label').innerText(),/agent-27/,'The center strand identifies its actual source owner');
 // Compare plot coordinates below the 22px ruler and above the 12px foot clearance.
 const readPoints=()=>page.locator('.journey-event[data-event-id]').evaluateAll(elements=>elements.map(element=>{const box=element.getBoundingClientRect(),field=element.closest('.journey-field').getBoundingClientRect();return{id:element.dataset.eventId,group:element.dataset.workstream,time:Number(element.dataset.time),x:box.x+box.width/2-field.x,y:(box.y+box.height/2-field.y-22)/(field.height-34)};}));
 const points=await readPoints();assert(points.length>20);
 const ordered=[...points].sort((a,b)=>a.time-b.time),first=ordered[0],last=ordered.at(-1);
 const scale=(last.x-first.x)/(last.time-first.time);
 for(const point of points)assert(Math.abs(point.x-first.x-(point.time-first.time)*scale)<.2,'Curved fiber ports never distort recorded X spacing');
 await page.screenshot({path:join(output,'04-rounded.png')});
 const fieldBounds=await page.locator('.journey-field').boundingBox();
 for(const port of await page.locator('.journey-boundary-port').all()){const box=await port.boundingBox();assert(box.x>fieldBounds.x+10&&box.x+box.width<fieldBounds.x+fieldBounds.width-10,'Default camera contains complete endpoint aggregates with gutters');}
 const startPort=page.locator('.journey-boundary-port[data-boundary="start"]');
 const startCount=Number((await startPort.getAttribute('aria-label')).match(/Inspect (\d+)/)[1]);
 await startPort.click();
 const recordPicker=page.getByLabel('Co-timed source records',{exact:true});
 assert.match(await recordPicker.innerText(),/not evidence of a shared parent or a rejoin/);
 assert.equal(await recordPicker.getByRole('button').count(),startCount,'Aggregate count equals inspectable source records');
 await recordPicker.getByRole('button').first().click();
 const portSource=JSON.parse(await page.getByTestId('loom-event-source').textContent());
 assert.equal(portSource.kind,'session');
 await page.getByRole('button',{name:'BACK TO WEAVE',exact:true}).click();
 const saved=new URL(page.url()).searchParams;
 await page.getByRole('button',{name:'Show full loaded page in minimap',exact:true}).click();
 assert(Number(await page.getByRole('slider',{name:'Seek loaded snapshot',exact:true}).getAttribute('min'))<Date.parse('2025-05-12T14:06:00Z')/1000,'Full page exposes earlier loaded history');
 await page.getByRole('button',{name:'Show current scene in minimap',exact:true}).click();
 for(const key of ['loom_event','loom_time','loom_from','loom_to'])assert.equal(new URL(page.url()).searchParams.get(key),saved.get(key),'Minimap scope does not change source or camera');

 const time=ordered[Math.floor(ordered.length/2)].time;
 await page.getByRole('slider',{name:'Seek loaded snapshot',exact:true}).fill(String(Math.floor(time)));
 await painted();
 const replay=await readPoints();
 assert.equal(await page.locator('.journey-boundary-port[data-boundary="tail"]').count(),0,'Future tail aggregate and its count are withheld');
 assert(replay.every(point=>point.time<=Math.floor(time)),'Replay withholds future native event targets');
 const original=points;
 let unchanged=0;
 for(const point of replay){const before=original.filter(before=>before.id===point.id&&before.group===point.group).sort((a,b)=>Math.abs(point.y-a.y)-Math.abs(point.y-b.y))[0];if(!before)continue;assert(Math.abs(point.x-before.x)<.2&&Math.abs(point.y-before.y)<.002,`Replay changed curved home lane: ${JSON.stringify({point,before})}`);unchanged++;}
 assert(unchanged>5);
 assert(await page.locator('.journey-map-event').evaluateAll((marks,cutoff)=>marks.every(mark=>Number(mark.dataset.time)<=cutoff),Math.floor(time)));
 const target=page.locator('.journey-event[data-event-id]').first(),id=await target.getAttribute('data-event-id');
 await target.click();
 const source=JSON.parse(await page.getByTestId('loom-event-source').textContent());
 assert.equal(source.id,id,'A fiber event opens its own source record');
 assert.deepEqual(errors,[]);
 console.log('PASS rounded fibers retain exact temporal X, stable replay lanes, explicit source spine, future withholding and exact provenance selection');
}finally{await browser.close();}
