"""Export frozen Git containment and Cargo path dependencies; never opens TD stores."""
import argparse, json, subprocess, tomllib, posixpath
from datetime import datetime, timezone
from pathlib import Path
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('repository', nargs='?', default='/fast/projects/tracedecay')
parser.add_argument('--revision', default='0b04bce19ab19ee724ca1e56900df25b97e74ae3', help='Git revision to capture; defaults to the checked-in Git-only snapshot pin')
parser.add_argument('--output', type=Path, default=Path('src/structure/snapshot.json'))
args=parser.parse_args()
repo=args.repository
before='d3b1a4ab505154a5e70d2595eb549aecc1a4e1ef'
after='e199f3e08f667a0b9f95e98a80ebb6dc371c4eec'
def git(*args): return subprocess.check_output(['git','-C',repo,*args]).decode()
revision=git('rev-parse','--verify',args.revision+'^{commit}').strip()
def tree(rev):
 out={}
 for row in git('ls-tree','-rl','-z',rev).split('\0'):
  if not row: continue
  meta,path=row.split('\t',1); mode,kind,oid,size=meta.split()
  if kind=='blob': out[path]={'bytes':int(size),'oid':oid,'mode':mode}
 return out
current=tree(revision); old=tree(before)
changes=[]
for line in git('diff','--name-status','-M',before,after).splitlines():
 cols=line.split('\t'); status={'A':'added','D':'removed','M':'modified','R':'relocated'}.get(cols[0][0],cols[0])
 changes.append({'path':cols[-1],'status':status,**({'previousPath':cols[1]} if len(cols)>2 else {})})
allfiles={**{c['path']:old[c['path']] for c in changes if c['status']=='removed'},**current}
nodes={'':{'id':'','path':'','name':'tracedecay','kind':'directory','parent':None,'files':0,'bytes':0}}
for path,details in allfiles.items():
 parts=path.split('/')
 for i in range(1,len(parts)):
  p='/'.join(parts[:i]);nodes.setdefault(p,{'id':p,'path':p,'name':parts[i-1],'kind':'directory','parent':'/'.join(parts[:i-1]),'files':0,'bytes':0})
 nodes[path]={'id':path,'path':path,'name':parts[-1],'kind':'file','parent':'/'.join(parts[:-1]),'files':1,'bytes':details['bytes'],'removed':path not in current}
 for i in range(len(parts)):
  node=nodes['/'.join(parts[:i])];node['files']+=1;node['bytes']+=details['bytes']
for path in allfiles.keys()-current.keys():
 parts=path.split('/')
 for i in range(len(parts)):
  n=nodes['/'.join(parts[:i])];n['files']-=1;n['bytes']-=allfiles[path]['bytes']
manifests={}
for p in current:
 if p.endswith('Cargo.toml'):
  try: manifests[p]=tomllib.loads(git('show',revision+':'+p))
  except tomllib.TOMLDecodeError as e: raise RuntimeError(p) from e
rootdeps=manifests.get('Cargo.toml',{}).get('workspace',{}).get('dependencies',{})
packages={}
for p,m in manifests.items():
 if 'package' in m:
  parent=posixpath.dirname(p);packages[parent]=m['package']['name'];nodes[parent]['kind']='crate';nodes[parent]['name']=m['package']['name']
edges=[];unresolved=[]
for p,m in manifests.items():
 source=posixpath.dirname(p)
 if source not in packages:continue
 tables=[(kind,m.get(kind,{})) for kind in ['dependencies','dev-dependencies','build-dependencies']]
 for target,t in m.get('target',{}).items():tables += [(target+':'+kind,t.get(kind,{})) for kind in ['dependencies','dev-dependencies','build-dependencies']]
 for scope,deps in tables:
  for name,spec in deps.items():
   if not isinstance(spec,dict):continue
   base=source
   if spec.get('workspace'):
    spec={**rootdeps.get(name,{}),**spec}
    base=''
   if not isinstance(spec,dict) or 'path' not in spec:continue
   target=posixpath.normpath(posixpath.join(base,spec['path']))
   if target in packages:edges.append({'source':source,'target':target,'kind':'manifest-dependency','scope':scope,'manifest':p,'dependency':name,'optional':bool(spec.get('optional'))})
   else:unresolved.append({'manifest':p,'dependency':name,'path':target})
# Count distinct non-merge commit/path touches in a fixed seven-day window.
# No rename following: these measurements describe paths present at the head.
anchor=int(git('show','-s','--format=%ct',revision).strip()); start=anchor-7*86400
commit_times={sha:int(at) for sha,at in (line.split() for line in git('log','--no-merges','--since-as-filter=@'+str(start),'--until=@'+str(anchor),'--format=%H %ct',revision).splitlines())}
raw=subprocess.check_output(['git','-C',repo,'diff-tree','--stdin','--root','--no-renames','--name-only','-r','-z'],input=('\n'.join(commit_times)+'\n').encode()).decode()
file_touches={}; excluded_paths=set(); active_commit=None
for token in raw.split('\0'):
 if not token: continue
 if token in commit_times:
  active_commit=token;continue
 if active_commit is None: raise RuntimeError('Git diff-tree emitted a path before its commit')
 if token not in current: excluded_paths.add(token);continue
 item=file_touches.setdefault(token,{'path':token,'touches':0,'lastTouchedAt':0,'lastCommit':''})
 item['touches']+=1
 if (commit_times[active_commit],active_commit)>(item['lastTouchedAt'],item['lastCommit']):
  item['lastTouchedAt']=commit_times[active_commit];item['lastCommit']=active_commit
churn={'start':datetime.fromtimestamp(start,timezone.utc).isoformat(),'end':datetime.fromtimestamp(anchor,timezone.utc).isoformat(),'days':7,'commitCount':len(commit_times),'pathTouches':sum(x['touches'] for x in file_touches.values()),'touchedFiles':len(file_touches),'excludedHistoricalPaths':len(excluded_paths),'basis':'Reachable non-merge commit/file touches, current paths only; renames not followed; additions/deletions are path touches, not line counts or defects','files':sorted(file_touches.values(),key=lambda x:x['path'])}
source_extensions={'.rs','.ts','.tsx','.js','.jsx','.mjs','.cjs','.py','.go','.java','.c','.h','.cc','.cpp','.hpp','.cs','.kt','.swift','.rb','.sh'}
source_files={p:d for p,d in current.items() if Path(p).suffix in source_extensions and d['mode'] in ('100644','100755')}
blob_paths={}
for path,details in source_files.items():
 if details['bytes']>0: blob_paths.setdefault(details['oid'],[]).append(path)
groups=[{'blob':oid,'bytes':current[paths[0]]['bytes'],'paths':sorted(paths)} for oid,paths in blob_paths.items() if len(paths)>1]
groups.sort(key=lambda g:(-len(g['paths']),g['blob']))
duplicate_files={'basis':'Exact same nonempty Git blob across source-code paths; tests, fixtures and generated sources included. Byte identity is a candidate for inspection, not semantic equivalence or a defect. Symlinks and empty files excluded.','sourceExtensions':sorted(source_extensions),'sourceFiles':len(source_files),'emptyFilesExcluded':sum(d['bytes']==0 for d in source_files.values()),'groups':groups}
result={'revision':revision,'baselineRevision':before,'changeRevision':after,'repository':'ScriptedAlchemy/tracedecay','nodes':list(nodes.values()),'edges':edges,'changes':changes,'churn':churn,'duplicateFiles':duplicate_files,'coverage':{'trackedFiles':len(current),'ghostFiles':len(allfiles)-len(current),'manifests':len(manifests),'packages':len(packages),'unresolvedPathDependencies':unresolved,'basis':'Git tracked blobs and Cargo declared path dependencies; not resolved calls or feature activation'},'forwarding':{'consumer':'TraceDecay aggregate','title':'Aggregate runtime forwarding layer removed','removed':['ProjectStoreRuntimeHandle','ProjectStoreRuntimeV1','boxed RuntimeFuture adapter'],'owner':'DaemonSessionRuntimeRegistryV1','sourcePaths':['crates/tracedecay-application/src/tracedecay/runtime_port.rs','crates/tracedecay-store-runtime/src/session_registry/project_store_runtime.rs','crates/tracedecay/src/project_store_runtime.rs'],'basis':'Pinned source diff and migrated lifecycle callers; no unreachable-code claim'}}
args.output.parent.mkdir(parents=True,exist_ok=True)
args.output.write_text(json.dumps(result,separators=(',',':'))+'\n')
print(json.dumps({'nodes':len(nodes),'files':len(current),'edges':len(edges),'changes':len(changes),'unresolved':len(unresolved),'churnTouches':churn['pathTouches'],'duplicateGroups':len(groups)}))
