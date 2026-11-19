import {MapRenderer} from './renderer.mjs';

const $ = id => document.getElementById(id);
const folder = n => n.kind === 'dir' || n.kind === 'ghost_dir';
const ghost = n => n.kind === 'ghost_dir' || n.kind === 'ghost_file';
const defaults = () => ({filter:'',hideTests:false,showIgnored:false,color:'file_type',search:'',traceRoot:null,direction:'all',depth:1,evidence:'all'});
const state = {
  config:null,project:null,options:defaults(),view:null,nodes:new Map(),selected:null,targetLine:null,
  projectGeneration:0,optionsEpoch:0,sourceGeneration:0,viewEpoch:-1,syntax:true,
  sources:new Map(),sourceFailures:new Set(),sourceSuppressed:new Set(),wantedSources:new Set(),sourceQueue:[],sourcePending:new Map(),sourceActive:0,
  sourceBytes:0,sourceRequests:0,sourceAbort:new Set(),mainAbort:new Set(),mainTail:Promise.resolve(),
  busy:0,statusTimer:null,viewTimer:null,pendingFit:false,queryTarget:null,searchIndex:-1,error:null,
};

function status(message, error = false) {
  $('status').textContent = message || 'Ready';
  $('status').classList.toggle('error',error);
  state.error = error ? message : null;
  $('status').title = message || 'Ready';
}

async function request(endpoint, {method='GET',body,source=false,project=state.project} = {}) {
  const url = new URL(`api/${endpoint}`,document.baseURI);
  if (project) url.searchParams.set('project',project);
  const controller = new AbortController();
  const controllers = source ? state.sourceAbort : state.mainAbort;
  controllers.add(controller);
  const timeout = setTimeout(() => controller.abort(),120000);
  const headers = {'x-megamap-token':state.config.token};
  if (body !== undefined) headers['content-type']='application/json';
  try {
    const response = await fetch(url,{method,headers,body:body===undefined ? undefined : JSON.stringify(body),signal:controller.signal,credentials:'same-origin'});
    let result;
    try {result=await response.json();} catch {throw new Error(`Local service returned an invalid response (${response.status}).`);}
    if (!response.ok) throw new Error(typeof result.error==='string' ? result.error : `Local service error ${response.status}.`);
    return result;
  } finally { clearTimeout(timeout); controllers.delete(controller); }
}

function invalidateSources() {
  state.sourceGeneration++;
  for (const ctrl of state.sourceAbort) ctrl.abort();
  state.sources=new Map(); state.sourceFailures=new Set(); state.sourceSuppressed=new Set(); state.wantedSources=new Set(); state.sourceQueue=[]; state.sourcePending=new Map(); state.sourceBytes=0;
  renderer.setSources(state.sources);
  $('source-preview').replaceChildren();
  $('source-status').textContent=state.selected==null ? '' : 'Loading source…';
}

function loading(on, message='Analyzing project…') {
  $('loading').hidden=!on; $('loading-label').textContent=message;
  $('refresh').disabled=on;
  $('capture').disabled=on || !state.config?.capabilities?.capture || state.view?.capabilities?.capture===false;
  $('snapshot').disabled=on; $('static').disabled=on;
  if (on) {
    if (!state.statusTimer) state.statusTimer=setInterval(pollStatus,800);
  } else {clearInterval(state.statusTimer);state.statusTimer=null;}
}

async function pollStatus() {
  const gen=state.projectGeneration,project=state.project;
  if (!project || !state.busy) return;
  try {
    const result=await request('status',{project});
    if (gen!==state.projectGeneration || !state.busy) return;
    if (result.message) $('loading-label').textContent=result.message;
    if (result.error) status(String(result.error),true);
  } catch { /* The active operation reports service errors. */ }
}

function enqueue(action) {
  const gen=state.projectGeneration;
  const promise=state.mainTail.catch(()=>{}).then(async()=>{
    if(gen!==state.projectGeneration)return;
    state.busy++; loading(true);
    try {await action(gen);}
    catch(error){
      if(gen===state.projectGeneration){
        status(`${error.name==='AbortError' ? 'The local service request was interrupted or timed out.' : error.message} Use Refresh to retry or Static only to return to source analysis.`,true);
        $('source-status').textContent='Source unavailable while the view is out of date. Refresh to retry.';
      }
    } finally {
      if(gen===state.projectGeneration){state.busy--;loading(state.busy>0);renderer.requestDraw();if(state.selected!=null)needSources([state.selected],true);}
    }
  });
  state.mainTail=promise;
  return promise;
}

function scheduleView({fit=false,immediate=false}={}) {
  state.optionsEpoch++; state.pendingFit ||= fit;
  invalidateSources();
  clearTimeout(state.viewTimer);
  state.viewTimer=setTimeout(()=>{
    state.viewTimer=null;
    const epoch=state.optionsEpoch,project=state.project,options={...state.options};
    enqueue(async gen=>{
      if(epoch!==state.optionsEpoch)return;
      const view=await request('view',{method:'POST',body:options,project});
      if(gen!==state.projectGeneration || epoch!==state.optionsEpoch)return;
      applyView(view,epoch);
      if(state.pendingFit){renderer.fit();state.pendingFit=false;}
      applyQueryTarget();
      if(state.advanceSearch){state.advanceSearch=false;nextSearchMatch();}
    });
  },immediate ? 0 : 170);
}

function operation(endpoint,body) {
  clearTimeout(state.viewTimer); state.viewTimer=null;
  state.optionsEpoch++;
  invalidateSources();
  const project=state.project;
  return enqueue(async gen=>{
    await request(endpoint,{method:'POST',body,project});
    if(gen!==state.projectGeneration)return;
    const epoch=state.optionsEpoch,options={...state.options};
    const view=await request('view',{method:'POST',body:options,project});
    if(gen!==state.projectGeneration || epoch!==state.optionsEpoch)return;
    applyView(view,epoch);
    applyQueryTarget();
  });
}

function applyView(view,epoch) {
  if(!view || !Array.isArray(view.nodes))throw new Error('The local service returned no treemap nodes.');
  state.view=view; state.viewEpoch=epoch;
  $('depth').max=view.capabilities.maxDepth;
  state.nodes=new Map(view.nodes.map(n=>[n.id,n]));
  invalidateSources();
  if(state.selected!=null && !state.nodes.get(state.selected)?.visible){state.selected=null;state.targetLine=null;}
  renderer.setView({...view,searchActive:Boolean(state.options.search) && !(view.trace?.root!=null && state.options.direction!=='all')});
  renderer.select(state.selected,state.targetLine);
  tree.sync(); updateInspector();
  $('counts').textContent=`${(view.counts?.files||0).toLocaleString()} files · ${(view.counts?.lines||0).toLocaleString()} lines`;
  $('evidence-status').textContent=stringStatus(view.evidenceStatus) || 'Static source analysis';
  $('evidence-status').title=$('evidence-status').textContent;
  $('empty').hidden=view.nodes.some(n=>n.visible && !folder(n) && n.rect[2]>0 && n.rect[3]>0);
  status(stringStatus(view.status) || 'Ready');
  if(view.trace?.root!=null && state.options.direction!=='all'){
    const root=state.nodes.get(view.trace.root);
    const detail=`Trace ${root?.path||view.trace.root}: ${view.trace.direct||0} direct · ${view.trace.indirect||0} indirect · ${view.trace.reachedSymbols||0} symbols · ${view.trace.sites||0} call sites`;
    const diagnostics=`${view.trace.inferred||0} inferred · ${view.trace.loaded||0} loaded · ${view.trace.observed||0} observed · ${view.trace.analyzedFiles||0} analyzed files · ${view.trace.unresolvedSites||0} unresolved / ${view.trace.parseErrors||0} parse errors / ${view.trace.unreadFiles||0} unread`;
    status(`${$('status').textContent} · ${detail} · ${diagnostics}`);
  }
  $('capture').disabled=state.busy>0 || !state.config.capabilities?.capture || view.capabilities?.capture===false;
}

function stringStatus(value) {
  if(typeof value==='string')return value;
  if(Array.isArray(value))return value.filter(Boolean).join(' · ');
  return value && typeof value.message==='string' ? value.message : '';
}

function chooseProject(id) {
  if(!state.config.projects.some(p=>p.id===id))return;
  state.projectGeneration++; state.optionsEpoch++; state.project=id;
  for(const ctrl of [...state.mainAbort,...state.sourceAbort])ctrl.abort();
  clearTimeout(state.viewTimer); state.viewTimer=null;
  state.mainTail=Promise.resolve();state.busy=0;loading(false);
  state.options=defaults();state.view=null;state.viewEpoch=-1;state.nodes=new Map();state.selected=null;state.targetLine=null;state.pendingFit=true;
  state.searchIndex=-1;state.advanceSearch=false;renderer.clear();tree.reset();invalidateSources();syncControls();
  $('selection').textContent='Select a file in the map or file tree.';
  $('source-status').textContent='';$('source-tools').hidden=true;$('counts').textContent='';
  $('evidence-status').textContent='Static source analysis';$('empty').hidden=true;
  $('project').value=id;
  const url=new URL(location.href);url.searchParams.set('project',id);
  if(!state.queryTarget){url.searchParams.delete('file');url.searchParams.delete('line');}
  history.replaceState(null,'',url);
  status('Loading project…');scheduleView({fit:true,immediate:true});
}

function syncControls() {
  const o=state.options;
  $('search').value=o.search;$('filter').value=o.filter;$('ignored').checked=o.showIgnored;
  $('tests').checked=o.hideTests;$('color').value=o.color;$('direction').value=o.direction;
  $('depth').value=o.depth;$('evidence').value=o.evidence;$('trace').disabled=state.selected==null;
}

function applyQueryTarget() {
  const target=state.queryTarget;
  if(!target)return;
  state.queryTarget=null;
  const node=[...state.nodes.values()].find(n=>n.path===target.file);
  if(node?.visible){selectNode(node.id,{reveal:true,fly:true,line:target.line});}
  else status(`The requested file is ${node ? 'hidden by the current filters' : 'not indexed in this project'}: ${target.file}`,true);
}

function selectNode(id,{reveal=true,fly=false,line=null}={}) {
  const node=state.nodes.get(id);
  if(!node?.visible)return;
  state.selected=id;state.targetLine=line;
  renderer.select(id,line);
  if(fly)renderer.flyTo(node,line);
  if(reveal)tree.reveal(id); else tree.render();
  updateInspector();
  if(node.kind==='text')needSources([id],true);
}

function activateNode(id) {
  const n=state.nodes.get(id);
  if(!n)return;
  selectNode(id);
  if(ghost(n)){
    if(folder(n))tree.expanded.add(n.path);
    operation('expand',{node:id});
  }else renderer.flyTo(n);
}

function jumpLink(link) {
  const def=link.definition,node=state.nodes.get(def?.node);
  if(!node?.visible){status('The definition is no longer visible. Refresh the trace and try again.',true);return;}
  selectNode(node.id,{fly:true,line:Math.max(1,Number(def.line)||1)});
  status(`Definition: ${def.name} · ${def.file}:${def.line} · ${link.evidence||'source'}${link.reason ? ' · '+link.reason : ''}`);
}

const renderer=new MapRenderer($('map'),{
  onSelect:id=>selectNode(id),onActivate:activateNode,onLink:jumpLink,
  onSourcesNeeded:ids=>{state.wantedSources=new Set(ids);state.sourceQueue=state.sourceQueue.filter(id=>state.wantedSources.has(id)||id===state.selected);needSources(ids);},
  onGPU:available=>{$('renderer-status').textContent=available ? 'WebGL2 · GPU treemap' : 'GPU unavailable';},
});

class FileTree {
  constructor(){
    this.el=$('tree');this.spacer=$('tree-spacer');this.content=$('tree-rows');this.expanded=new Set(['']);this.rows=[];this.order=new Map();this.focus=null;
    this.el.addEventListener('scroll',()=>this.render(),{passive:true});
    this.el.addEventListener('keydown',e=>this.key(e));
    this.content.addEventListener('click',e=>{
      const row=e.target.closest('[data-node]');if(!row)return;
      const id=Number(row.dataset.node),n=state.nodes.get(id);this.focus=id;
      if(e.target.closest('.tree-disclosure') && folder(n))this.toggle(id);
      else selectNode(id,{reveal:false});
      this.el.focus({preventScroll:true});
    });
    this.content.addEventListener('dblclick',e=>{
      if(e.target.closest('.tree-disclosure'))return;
      const row=e.target.closest('[data-node]');if(!row)return;
      const id=Number(row.dataset.node),n=state.nodes.get(id);
      if(folder(n))this.toggle(id);else activateNode(id);
    });
    new ResizeObserver(()=>this.render()).observe(this.el);
  }
  reset(){this.expanded=new Set(['']);this.focus=null;this.rows=[];this.order.clear();this.el.scrollTop=0;this.el.scrollLeft=0;this.render();}
  sync(){
    this.order=new Map();
    for(const n of state.nodes.values()){
      const children=n.children.map(id=>state.nodes.get(id)).filter(n=>n?.visible);
      children.sort((a,b)=>Number(folder(b))-Number(folder(a)) || a.name.toLowerCase().localeCompare(b.name.toLowerCase()) || a.name.localeCompare(b.name));
      this.order.set(n.id,children.map(n=>n.id));
    }
    this.rebuild();
    if(!this.rows.some(r=>r.id===this.focus))this.focus=this.rows.some(r=>r.id===state.selected) ? state.selected : this.rows[0]?.id;
    this.render();
  }
  rebuild(){
    const stack=[...state.nodes.values()].filter(n=>n.visible && n.parent==null).reverse().map(n=>({id:n.id,depth:0}));
    this.rows=[];let width=180;
    while(stack.length){
      const row=stack.pop(),n=state.nodes.get(row.id);this.rows.push(row);
      width=Math.max(width,row.depth*16+Math.min(500,n.name.length)*8+90);
      if(this.expanded.has(n.path))for(const id of (this.order.get(n.id)||[]).slice().reverse())stack.push({id,depth:row.depth+1});
    }
    this.width=width;this.spacer.style.height=`${this.rows.length*24}px`;this.spacer.style.width=`${width}px`;
    $('tree-count').textContent=this.rows.length.toLocaleString();
  }
  toggle(id){
    const n=state.nodes.get(id);if(!n || !folder(n))return;
    if(this.expanded.has(n.path)){
      this.expanded.delete(n.path);this.focus=id;
    }else{
      this.expanded.add(n.path);
      if(ghost(n))operation('expand',{node:id});
    }
    this.rebuild();this.render();
  }
  reveal(id){
    let n=state.nodes.get(id),parent=n?.parent;
    const seen=new Set();
    while(parent!=null && !seen.has(parent)){
      seen.add(parent);const p=state.nodes.get(parent);if(!p)break;this.expanded.add(p.path);parent=p.parent;
    }
    this.focus=id;this.rebuild();this.scrollToFocus();this.render();
  }
  scrollToFocus(){
    const index=this.rows.findIndex(r=>r.id===this.focus);if(index<0)return;
    const top=index*24;
    if(top<this.el.scrollTop)this.el.scrollTop=top;
    else if(top+24>this.el.scrollTop+this.el.clientHeight)this.el.scrollTop=Math.max(0,top+24-this.el.clientHeight);
    const labelLeft=this.rows[index].depth*16+24;
    if(labelLeft<this.el.scrollLeft+16 || labelLeft+80>this.el.scrollLeft+this.el.clientWidth)this.el.scrollLeft=Math.max(0,(this.rows[index].depth-1)*16);
  }
  render(){
    const start=Math.max(0,Math.floor(this.el.scrollTop/24)-2),end=Math.min(this.rows.length,Math.ceil((this.el.scrollTop+this.el.clientHeight)/24)+2);
    const fragment=document.createDocumentFragment();
    for(let i=start;i<end;i++){
      const {id,depth}=this.rows[i],n=state.nodes.get(id);if(!n)continue;
      const row=document.createElement('div');row.className='tree-row';row.dataset.node=id;row.id=`tree-node-${id}`;
      row.style.top=`${i*24}px`;row.style.paddingLeft=`${depth*16+3}px`;row.style.width=`${this.width}px`;
      row.setAttribute('role','treeitem');row.setAttribute('aria-level',depth+1);row.setAttribute('aria-selected',String(id===state.selected));
      if(folder(n))row.setAttribute('aria-expanded',String(this.expanded.has(n.path)));
      row.classList.toggle('selected',id===state.selected);row.classList.toggle('focused',id===this.focus);
      row.classList.toggle('ignored',Boolean(n.ignored));row.classList.toggle('dim',Boolean(state.options.search) && !n.onPath);
      row.title=`${n.path||state.view?.root||n.name}${n.ignored ? ' (ignored; open to read)' : ''}`;
      const disclosure=document.createElement('button');disclosure.className='tree-disclosure';disclosure.tabIndex=-1;
      disclosure.textContent=folder(n) ? (this.expanded.has(n.path) ? '▾' : '▸') : '·';
      disclosure.disabled=!folder(n);disclosure.title=folder(n) ? 'Expand or collapse folder; ignored folders are read on demand.' : 'File';
      disclosure.setAttribute('aria-label',folder(n) ? `Toggle ${n.name}` : n.name);row.append(disclosure);
      const name=document.createElement('span');name.className='tree-name';name.textContent=n.name;row.append(name);
      const badge=document.createElement('span');badge.className='tree-badge';badge.textContent=ghost(n) ? 'ignored' : folder(n) ? `${n.totalFiles}` : `${n.lineCount||0}`;row.append(badge);
      fragment.append(row);
    }
    this.content.replaceChildren(fragment);
    if(this.focus!=null && document.getElementById(`tree-node-${this.focus}`))this.el.setAttribute('aria-activedescendant',`tree-node-${this.focus}`);
    else this.el.removeAttribute('aria-activedescendant');
  }
  key(e){
    const index=this.rows.findIndex(r=>r.id===this.focus);let target=null;
    if(e.key==='ArrowDown')target=this.rows[Math.min(this.rows.length-1,Math.max(0,index+1))]?.id;
    else if(e.key==='ArrowUp')target=this.rows[Math.max(0,index-1)]?.id;
    else if(e.key==='Home')target=this.rows[0]?.id;
    else if(e.key==='End')target=this.rows.at(-1)?.id;
    else if(e.key==='PageDown')target=this.rows[Math.min(this.rows.length-1,Math.max(0,index)+Math.floor(this.el.clientHeight/24))]?.id;
    else if(e.key==='PageUp')target=this.rows[Math.max(0,index-Math.floor(this.el.clientHeight/24))]?.id;
    else if(e.key==='ArrowRight'){
      const n=state.nodes.get(this.focus);
      if(n && folder(n)){
        if(!this.expanded.has(n.path))this.toggle(n.id);
        else target=this.order.get(n.id)?.[0];
      }
    }else if(e.key==='ArrowLeft'){
      const n=state.nodes.get(this.focus);
      if(n && folder(n) && this.expanded.has(n.path))this.toggle(n.id);
      else target=n?.parent;
    }else if(e.key==='Enter' || e.key===' '){
      const n=state.nodes.get(this.focus);
      if(n && folder(n))this.toggle(n.id);else if(n)activateNode(n.id);
    }else return;
    e.preventDefault();
    if(target!=null){this.focus=target;selectNode(target,{reveal:false});this.scrollToFocus();this.render();}
  }
}
const tree=new FileTree();

function needSources(ids, priority=false) {
  if(!state.view || state.busy || state.viewEpoch!==state.optionsEpoch)return;
  for(const id of ids){
    const node=state.nodes.get(id);
    if(priority)state.sourceSuppressed.delete(id);
    if(node?.kind!=='text' || state.sourceSuppressed.has(id) || state.sources.has(id) || state.sourceFailures.has(id) || state.sourcePending.has(id) || state.sourceQueue.includes(id))continue;
    if(priority)state.sourceQueue.unshift(id);else state.sourceQueue.push(id);
  }
  if(state.sourceQueue.length>16)state.sourceQueue.length=16;
  pumpSources();
}

function pumpSources() {
  while(state.sourceActive<2 && state.sourceQueue.length && !state.busy && state.viewEpoch===state.optionsEpoch){
    const id=state.sourceQueue.shift();
    if(!state.wantedSources.has(id) && id!==state.selected)continue;
    const gen=state.projectGeneration,sourceGen=state.sourceGeneration,project=state.project,syntax=state.syntax;
    state.sourceActive++;state.sourcePending.set(id,sourceGen);state.sourceRequests++;
    request(`source?node=${encodeURIComponent(id)}&syntax=${syntax?1:0}`,{source:true,project}).then(source=>{
      if(gen!==state.projectGeneration || sourceGen!==state.sourceGeneration)return;
      if(!source || !Array.isArray(source.lines))throw new Error('Invalid source response.');
      source._cacheBytes=JSON.stringify(source).length*2;
      state.sources.set(id,source);state.sourceBytes+=source._cacheBytes;
      while(state.sources.size>24 || state.sourceBytes>16*1024*1024){
        const remove=[...state.sources.keys()].find(key=>key!==state.selected && key!==id);
        if(remove==null)break;
        state.sourceBytes-=state.sources.get(remove)._cacheBytes;state.sources.delete(remove);state.sourceSuppressed.add(remove);
      }
      renderer.setSources(state.sources);
      if(state.selected===id)renderSource();
    }).catch(error=>{
      if(gen!==state.projectGeneration || sourceGen!==state.sourceGeneration)return;
      state.sourceFailures.add(id);
      if(state.selected===id){$('source-status').textContent=`Source unavailable: ${error.name==='AbortError'?'request interrupted':error.message}. Use Refresh to retry.`;$('source-preview').replaceChildren();}
    }).finally(()=>{
      state.sourceActive--;
      if(state.sourcePending.get(id)===sourceGen)state.sourcePending.delete(id);
      pumpSources();
    });
  }
}

function updateInspector() {
  const node=state.nodes.get(state.selected);
  $('source-preview').replaceChildren();$('source-tools').hidden=true;
  $('trace').disabled=!node || node.kind!=='text';
  if(!node){$('selection').textContent='Select a file in the map or file tree.';$('source-status').textContent='';return;}
  const fragment=document.createDocumentFragment();
  const title=document.createElement('h2');title.textContent=node.name;fragment.append(title);
  const path=document.createElement('div');path.textContent=node.path||state.view.root;fragment.append(path);
  const dl=document.createElement('dl');
  const pairs=[['Kind',node.kind],['Lines',(folder(node)?node.totalLines:node.lineCount).toLocaleString()],['Files',String(node.totalFiles)],['Bytes',Number(node.bytes||0).toLocaleString()],['Commits',String(node.commits||0)],['Changed',node.lastChange>0?new Date(node.lastChange*1000).toLocaleDateString():'No Git data']];
  for(const [key,value]of pairs){const dt=document.createElement('dt'),dd=document.createElement('dd');dt.textContent=key;dd.textContent=value;dl.append(dt,dd);}fragment.append(dl);
  if(ghost(node)){
    const button=document.createElement('button');button.textContent='Read ignored entry';button.title='Read this ignored entry on demand using the registered repository.';button.addEventListener('click',()=>activateNode(node.id));fragment.append(button);
  }
  $('selection').replaceChildren(fragment);
  if(node.kind==='text'){
    $('source-tools').hidden=false;$('line').max=Math.max(1,node.lineCount);$('line').value=state.targetLine||1;
    if(state.sources.has(node.id))renderSource();
    else $('source-status').textContent=state.sourceFailures.has(node.id)?'Source unavailable. Use Refresh to retry.':'Loading source…';
  }else $('source-status').textContent=folder(node)?'Open folders in the file tree; double-click the map to zoom.':ghost(node)?'Ignored content has not been read.':'Binary content is not displayed as source.';
}

function renderSource() {
  const source=state.sources.get(state.selected),node=state.nodes.get(state.selected);
  if(!source || !node)return;
  const fragment=document.createDocumentFragment();
  $('source-status').textContent=source.message||'';
  const focus=state.targetLine||1,start=Math.max(0,Math.min(source.lines.length-100,focus-31));
  const end=Math.min(source.lines.length,start+100);
  if(start>0){const note=document.createElement('div');note.className='preview-note';note.textContent=`Lines ${start+1}–${end} of ${source.lines.length}. Use Line / Go to navigate.`;fragment.append(note);}
  for(let i=start;i<end;i++){
    const row=document.createElement('div');row.className='source-line';row.classList.toggle('active',i+1===state.targetLine);
    const number=document.createElement('span');number.className='line-number';number.textContent=String(i+1);row.append(number);
    const code=document.createElement('span');code.className='source-code';appendColoredLine(code,source.lines[i]);row.append(code);fragment.append(row);
  }
  if(end<source.lines.length){const note=document.createElement('div');note.className='preview-note';note.textContent=`Preview bounded to ${end-start} lines. Zoom the map or use Line / Go for the rest.`;fragment.append(note);}
  $('source-preview').replaceChildren(fragment);
  if(state.targetLine)requestAnimationFrame(()=>{$('source-preview').querySelector('.active')?.scrollIntoView({block:'nearest'});});
}

function appendColoredLine(element,line) {
  const chars=Array.from(line.text).slice(0,500),bounds=new Set([0,chars.length]);
  const runs=[];let column=0;
  for(const run of line.runs||[]){const end=column+Array.from(run.text).length;runs.push({start:column,end,color:run.color});bounds.add(Math.min(chars.length,column));bounds.add(Math.min(chars.length,end));column=end;if(column>=chars.length)break;}
  const links=(line.links||[]).filter(l=>l.definition && state.nodes.get(l.definition.node)?.visible && l.start<chars.length);
  for(const link of links){bounds.add(Math.max(0,Math.min(chars.length,link.start)));bounds.add(Math.max(0,Math.min(chars.length,link.end)));}
  const positions=[...bounds].sort((a,b)=>a-b);
  for(let i=0;i<positions.length-1;i++){
    const start=positions[i],end=positions[i+1];if(start===end)continue;
    const run=runs.find(r=>r.start<=start && r.end>start),link=links.find(l=>l.start<=start && l.end>=end);
    const span=document.createElement(link?'button':'span');span.textContent=chars.slice(start,end).join('');
    if(run)span.style.color=`rgb(${run.color.map(v=>Math.round(v*255)).join(',')})`;
    if(link){span.className='source-link';span.title=`Jump to ${link.definition.name} · ${link.definition.file}:${link.definition.line} · ${link.evidence||'source'}${link.reason?' · '+link.reason:''}`;span.addEventListener('click',()=>jumpLink(link));}
    element.append(span);
  }
  if(Array.from(line.text).length>500)element.append('…');
}

function nextSearchMatch() {
  if(!state.view || state.viewEpoch!==state.optionsEpoch){scheduleView({immediate:true});state.advanceSearch=true;return;}
  const query=state.options.search.trim();if(!query)return;
  const matches=state.view.nodes.filter(n=>n.visible && n.lit && (!state.nodes.get(n.parent)?.lit || !folder(n)));
  if(!matches.length){status(`No search matches for “${query}”.`);return;}
  state.searchIndex=(state.searchIndex+1)%matches.length;
  selectNode(matches[state.searchIndex].id,{fly:true});
  status(`Search ${state.searchIndex+1}/${matches.length}: ${matches[state.searchIndex].path||matches[state.searchIndex].name}`);
}

$('project').addEventListener('change',()=>{state.queryTarget=null;chooseProject($('project').value);});
$('fit').addEventListener('click',()=>renderer.fit());
$('tree-toggle').addEventListener('change',()=>{$('sidebar').hidden=!$('tree-toggle').checked;});
$('search').addEventListener('input',()=>{state.options.search=$('search').value;state.searchIndex=-1;state.advanceSearch=false;scheduleView();});
$('search').addEventListener('keydown',e=>{
  if(e.key==='Enter'){e.preventDefault();nextSearchMatch();}
  if(e.key==='Escape'){state.options.search='';$('search').value='';state.searchIndex=-1;scheduleView({immediate:true});}
});
$('filter').addEventListener('input',()=>{state.options.filter=$('filter').value;scheduleView();});
$('filter').addEventListener('keydown',e=>{if(e.key==='Enter'){e.preventDefault();scheduleView({immediate:true});}});
for(const [id,key]of [['ignored','showIgnored'],['tests','hideTests']])$(id).addEventListener('change',()=>{state.options[key]=$(id).checked;scheduleView({immediate:true});});
for(const [id,key]of [['color','color'],['direction','direction'],['evidence','evidence']])$(id).addEventListener('change',()=>{state.options[key]=$(id).value;scheduleView({immediate:true});});
$('depth').addEventListener('change',()=>{state.options.depth=Math.max(1,Math.min(Number($('depth').max),Math.floor(Number($('depth').value)||1)));$('depth').value=state.options.depth;scheduleView({immediate:true});});
$('syntax').addEventListener('change',()=>{state.syntax=$('syntax').checked;invalidateSources();updateInspector();if(state.selected!=null)needSources([state.selected],true);});
$('trace').addEventListener('click',()=>{
  if(state.selected==null)return;
  state.options.traceRoot=state.selected;
  if(state.options.direction==='all'){state.options.direction='callees';$('direction').value='callees';}
  scheduleView({fit:true,immediate:true});
});
$('clear-trace').addEventListener('click',()=>{state.options.traceRoot=null;state.options.direction='all';$('direction').value='all';scheduleView({fit:true,immediate:true});});
$('refresh').addEventListener('click',()=>{
  clearTimeout(state.viewTimer);state.viewTimer=null;
  const path=state.nodes.get(state.selected)?.path,line=state.targetLine;
  state.options={...state.options,traceRoot:null,direction:'all',evidence:'all'};
  syncControls();state.optionsEpoch++;invalidateSources();
  const epoch=state.optionsEpoch,options={...state.options},project=state.project;
  enqueue(async gen=>{
    const view=await request('refresh',{method:'POST',body:options,project});
    if(gen!==state.projectGeneration || epoch!==state.optionsEpoch)return;
    applyView(view,epoch);
    if(path!=null){const node=view.nodes.find(n=>n.visible && n.path===path);if(node)selectNode(node.id,{line:line ? Math.min(line,node.lineCount||1) : null});}
  });
});
$('static').addEventListener('click',()=>{state.options.evidence='static';$('evidence').value='static';operation('evidence',{snapshot:null});});
$('capture').addEventListener('click',()=>operation('capture',{}));
$('snapshot').addEventListener('change',async()=>{
  const file=$('snapshot').files[0],gen=state.projectGeneration;if(!file)return;
  if(file.size>32*1024*1024){status('Snapshot exceeds the 32 MiB import limit.',true);$('snapshot').value='';return;}
  try{const snapshot=await file.text();if(gen===state.projectGeneration)await operation('evidence',{snapshot});}
  catch(error){if(gen===state.projectGeneration)status(`Could not read snapshot: ${error.message}`,true);}
  finally{$('snapshot').value='';}
});
function jumpToLine(){
  const node=state.nodes.get(state.selected);if(!node)return;
  const line=Math.max(1,Math.min(node.lineCount,Math.floor(Number($('line').value)||1)));
  selectNode(node.id,{fly:true,line});
}
$('jump-line').addEventListener('click',jumpToLine);
$('line').addEventListener('keydown',e=>{if(e.key==='Enter')jumpToLine();});
document.addEventListener('keydown',e=>{
  if((e.metaKey||e.ctrlKey) && e.key.toLowerCase()==='f'){e.preventDefault();(e.shiftKey?$('filter'):$('search')).focus();}
});
window.addEventListener('error',e=>status(`Viewer error: ${e.message}. Reload to recover.`,true));
Object.defineProperty(window,'megamapDebug',{configurable:false,get:()=>Object.freeze({
  project:state.project,revision:state.view?.revision,selected:state.selected,targetLine:state.targetLine,
  camera:Object.freeze({...renderer.camera}),options:Object.freeze({...state.options}),
  nodes:state.view?.nodes.length||0,visibleNodes:renderer.nodes.length,
  treeRows:tree.rows.length,treeDOMRows:$('tree-rows').children.length,
  sourceCached:state.sources.size,sourceBytes:state.sourceBytes,sourceRequests:state.sourceRequests,sourcePending:state.sourceActive,
  busy:state.busy,error:state.error,gpu:Object.freeze({...renderer.stats}),
})});

async function bootstrap(){
  loading(true,'Connecting to local service…');
  try{
    const response=await fetch(new URL('api/config',document.baseURI),{credentials:'same-origin'});
    if(!response.ok)throw new Error(`Configuration request failed (${response.status}).`);
    const config=await response.json();
    if(!config.token || !Array.isArray(config.projects) || !config.projects.length)throw new Error('No projects are registered with the local service.');
    state.config=config;
    for(const project of config.projects){const option=document.createElement('option');option.value=project.id;option.textContent=project.label;$('project').append(option);}
    const query=new URL(location.href).searchParams;
    if(query.has('file'))state.queryTarget={file:query.get('file'),line:Math.max(1,Math.floor(Number(query.get('line'))||1))};
    const project=config.projects.find(p=>p.id===query.get('project'))||config.projects[0];
    chooseProject(project.id);
  }catch(error){loading(false);status(error.message,true);$('selection').textContent='The local analysis service is unavailable. Reload this page after starting the service.';}
}
bootstrap();
