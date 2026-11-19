import test from 'node:test';
import assert from 'node:assert/strict';
import http from 'node:http';
import {mkdtemp,mkdir,writeFile,readFile,readdir,access,rm} from 'node:fs/promises';
import {join} from 'node:path';
import {tmpdir} from 'node:os';
import {execFileSync} from 'node:child_process';
import {createMegamapHandler} from './server.mjs';
test('prepared sessions are bound to registered projects, immutable, and cleaned on close',async()=>{
 const dir=await mkdtemp(join(tmpdir(),'spamton-pr-service-http-'));let handler,server;
 const previous={PATH:process.env.PATH,TMPDIR:process.env.TMPDIR};
 try{
  const root=join(dir,'repo'),bin=join(dir,'bin'),temps=join(dir,'temps');for(const path of[root,bin,temps])await mkdir(path);
  const env={...process.env,GIT_AUTHOR_NAME:'Fixture',GIT_AUTHOR_EMAIL:'fixture@example.test',GIT_COMMITTER_NAME:'Fixture',GIT_COMMITTER_EMAIL:'fixture@example.test'};
  const git=(...args)=>execFileSync('/usr/bin/git',['-C',root,...args],{env,encoding:'utf8'}).trim();git('init','-q');await writeFile(join(root,'app.rb'),'class Before; end\n');git('add','.');git('-c','core.hooksPath=/dev/null','commit','-qm','Before');const base=git('rev-parse','HEAD');await writeFile(join(root,'app.rb'),'class After; end\n');git('add','.');git('-c','core.hooksPath=/dev/null','commit','-qm','After');const head=git('rev-parse','HEAD');
  const gh=`#!${process.execPath}\nconst path=process.argv[3];if(path.endsWith('/pulls/42')){await new Promise(done=>setTimeout(done,200));console.log(JSON.stringify(${JSON.stringify({number:42,title:'Fixture',url:'https://github.com/fixture/project/pull/42',repository:'fixture/project',baseSha:base,headSha:head})}));}else if(path.includes('/compare/'))console.log(${JSON.stringify(base)});else process.exit(1);\n`;
  await writeFile(join(bin,'gh'),gh,{mode:0o755});
  const binary=join(bin,'spamton-view.mjs');await writeFile(binary,`#!${process.execPath}\nimport{createInterface}from'node:readline';import{readFileSync}from'node:fs';const manifest=process.argv[process.argv.indexOf('--comparison')+1];createInterface({input:process.stdin}).on('line',line=>{const r=JSON.parse(line);process.stdout.write(JSON.stringify({id:r.id,result:{op:r.op,manifest,pr:JSON.parse(readFileSync(manifest,'utf8')).pr}})+'\\n');});\n`,{mode:0o755});
  process.env.PATH=bin+':'+previous.PATH;process.env.TMPDIR=temps;
  handler=await createMegamapHandler({binary,projects:['p0','p1','p2'].map(id=>({id,root,repository:'fixture/project'}))});
  server=http.createServer((req,res)=>handler.handle(req,res));await new Promise(done=>server.listen(0,'127.0.0.1',done));const url='http://127.0.0.1:'+server.address().port+'/megamap/';const config=await(await fetch(url+'api/config')).json();
  const request=(path,value)=>fetch(url+path,{method:value===undefined?'GET':'POST',headers:{'x-megamap-token':config.token,...(value===undefined?{}:{'content-type':'application/json'})},...(value===undefined?{}:{body:JSON.stringify(value)})});
  const pending=request('api/pr?project=p0',{number:42});let loading=false;for(let i=0;i<30;i++){loading=(await(await request('api/status?project=p0')).json()).loading;if(loading)break;await new Promise(done=>setTimeout(done,10));}assert.equal(loading,true);assert.equal((await request('api/pr?project=p0',{number:42})).status,503);
  const response=await pending;assert.equal(response.status,200);const result=await response.json();assert.match(result.comparison,/^[a-f0-9]{32}$/);assert.equal(result.view.pr.headSha,head);assert.equal(result.view.pr.mergeBaseSha,base);const target=result.view.manifest;
  const route='?project=p0&comparison='+result.comparison;assert.equal((await request('api/view'+route,{})).status,200);assert.equal((await request('api/source'+route+'&node=1')).status,200);assert.equal((await request('api/view?project=p1&comparison='+result.comparison,{})).status,404);
  for(const op of['refresh','expand','evidence','capture'])assert.equal((await request('api/'+op+route,{node:1,snapshot:null})).status,409);
  assert.equal((await request('api/pr?project=p1',{number:43})).status,400);assert.equal((await readdir(temps)).filter(name=>name.startsWith('spamton-megamap-pr-')).length,1);
  await handler.close();await assert.rejects(access(target));assert.equal((await readdir(temps)).filter(name=>name.startsWith('spamton-megamap-pr-')).length,0);assert.equal((await request('api/status?project=p0')).status,503);
 }finally{if(handler)await handler.close();if(server){server.closeAllConnections();await new Promise(done=>server.close(done));}process.env.PATH=previous.PATH;if(previous.TMPDIR===undefined)delete process.env.TMPDIR;else process.env.TMPDIR=previous.TMPDIR;await rm(dir,{recursive:true,force:true});}
});
