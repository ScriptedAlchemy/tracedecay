// Run with node qa/code-geometry.mjs. Exercises the production view model without a browser.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import ts from 'typescript';
const snapshot = JSON.parse(readFileSync(new URL('../src/structure/snapshot.json', import.meta.url), 'utf8'));
function compile(path, dependencies) {
  const source = readFileSync(new URL(path, import.meta.url), 'utf8');
  const result = ts.transpileModule(source, {compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022},reportDiagnostics:true});
  assert.equal(result.diagnostics?.length,0);
  const module = {exports:{}};
  new Function('require','exports','module',result.outputText)(id => {assert.ok(id in dependencies,`unexpected dependency ${id}`);return dependencies[id];},module.exports,module);
  return module.exports;
}
const model = compile('../src/structure/model.ts', {'./snapshot.json':{default:snapshot}});
const {dependencyDepths,buildSnapshotDataset,fixtureDataset} = compile('../src/code/semanticData.ts', {'../structure':model});
const ids = Array.from({length:12},(_,i)=>String(i));
const edges = ids.slice(1).map((id,i)=>({source:id,target:String(i)}));
edges.push({source:'11',target:'0'}); // A shortcut must not replace the longest chain.
assert.equal(dependencyDepths(ids,edges,[]).get('11'),11);
const condensed = dependencyDepths(['a','b','c'],[{source:'a',target:'b'},{source:'b',target:'a'},{source:'b',target:'c'}],[['a','b']]);
assert.equal(condensed.get('a'),1);assert.equal(condensed.get('b'),1);assert.equal(condensed.get('c'),0);
assert.throws(()=>dependencyDepths(['a','b'],[{source:'a',target:'b'},{source:'b',target:'a'}],[]),/acyclic/);
const data=buildSnapshotDataset();
assert.deepEqual(data,buildSnapshotDataset());
assert.equal(data.edges.length,snapshot.edges.length);
assert.ok(data.edges.every(edge=>edge.kind==='manifest'));
for(const edge of snapshot.edges.filter(e=>!e.scope.includes('dev-dependencies'))) {
 const from=data.regions.find(r=>r.id===edge.source),to=data.regions.find(r=>r.id===edge.target);
 if(!model.cycleGroups.some(group=>group.includes(edge.source)&&group.includes(edge.target))) assert.ok(from.depth>to.depth);
}
for(const dataset of [data,fixtureDataset]) {
 const positive=dataset.regions.filter(r=>r.mass>0),ratio=positive[0].rx*positive[0].ry/positive[0].mass;
 for(const region of positive) assert.ok(Math.abs(region.rx*region.ry/region.mass-ratio)<1e-8);
 for(const a of dataset.regions) for(const b of dataset.regions) if(a.depth>b.depth) assert.ok(a.y<b.y);
}
const symbols=new Map(fixtureDataset.files.flatMap(file=>file.symbols.map(s=>[s.id,{...s,file}])));
for(const {file,start,end} of symbols.values()) assert.ok(start>0&&end>=start&&end<=file.lines&&end<=(file.indexedTo??file.lines));
for(const node of fixtureDataset.nodes) {
 const symbol=symbols.get(node.id);
 assert.equal(node.startLine,symbol?.start??null);assert.equal(node.endLine,symbol?.end??null);
 if(!symbol) assert.equal(node.file,null);
}
assert.equal(symbols.get('focus').complexity,14);
assert.equal(fixtureDataset.files.find(f=>f.indexedTo)?.path,'crates/tracedecay-hooks/src/session/vendor_bridge.rs');
assert.equal(fixtureDataset.external.length,7);
assert.ok(fixtureDataset.external.every(e=>symbols.has(e.from)&&symbols.has(e.to)));
assert.ok(fixtureDataset.regions.filter(r=>['api','policy','store + rusqlite'].includes(r.id)).every(r=>r.warmth===null));
console.log(`PASS: ${data.regions.length} measured crate strata, ${data.edges.length} declarations, ${symbols.size} authored Core spans, 7 cross-file calls; SCC/longest-chain and geometry invariants.`);
