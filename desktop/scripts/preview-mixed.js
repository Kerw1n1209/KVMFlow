// A loopback-only synthetic preview; it never launches the native runtime.
// node scripts/preview-mixed.js -> ?scenario=mixed&page=settings&lang=en
const http=require('node:http');
const fs=require('node:fs');
const path=require('node:path');
const renderer=path.resolve(__dirname,'../src/renderer');
const fixture=fs.readFileSync(path.resolve(__dirname,'../tests/fixtures/mixed-preview.js'),'utf8');
const bridge=`${fixture}
(() => {
  const query=new URLSearchParams(location.search), sample=scenario(query.get('scenario')||'normal');
  const handlers=new Map();
  let runtimeSequence=0;
  const snapshot=data=>{const copy=structuredClone(data);copy.runtime_seq=++runtimeSequence;if(copy.mixed)copy.mixed.runtime_seq=copy.runtime_seq;return copy;};
  const emit=(kind,data)=>{if(['state','mixed.changed','mixed.pending_remote'].includes(kind))data=snapshot(data);for(const cb of handlers.get('runtime-notification')||[])cb({payload:{kind,data}});};
  window.__preview={...sample,emit};
  window.__TAURI__={event:{listen:async(kind,cb)=>{if(!handlers.has(kind))handlers.set(kind,new Set());handlers.get(kind).add(cb);return()=>handlers.get(kind).delete(cb);}},core:{invoke:async(command,args={})=>{
    if(command==='host_info')return{version:'0.3.0',platform:'preview',computerName:'MacBook Pro',languagePreference:query.get('lang')||'zh-CN',initialPage:query.get('page')==='settings'?'settings#mixed':'status'};
    if(command==='runtime_request'){
      const {method,params}=args;
      if(method==='config.get')return sample.config;
      if(method==='state.get')return snapshot(sample.state);
      if(method==='mixed.set') {if(sample.state.mixed.enabled===params.enabled)return snapshot({saved:false,applied:false,mixed:sample.state.mixed});sample.state.mixed.enabled=params.enabled;sample.state.mixed.revision++;sample.state.mixed.transition={phase:'idle',outcome:'ok'};emit('mixed.changed',{mixed:sample.state.mixed});return snapshot({saved:true,mixed:sample.state.mixed});}
      if(method==='mixed.assign'){sample.state.mixed.assignment=params.assignment;sample.state.mixed.revision++;return snapshot({saved:true,mixed:sample.state.mixed});}
      if(method==='display.list')return{displays:[]};
      return{};
    }
    if(command==='startup_get')return false;
    if(command==='update_check')return null;
    return null;
  }}};
})();`;
const server=http.createServer((request,response)=>{
  const url=new URL(request.url,'http://localhost');
  let body,type;
  if(url.pathname==='/preview-tauri.js'){body=bridge;type='text/javascript';}
  else {
    const relative=decodeURIComponent(url.pathname==='/'?'/index.html':url.pathname);
    const file=path.resolve(renderer,'.'+relative);
    if(!file.startsWith(renderer+path.sep)){response.writeHead(403);return response.end();}
    try{body=fs.readFileSync(file);}catch{response.writeHead(404);return response.end();}
    type={'.html':'text/html','.js':'text/javascript','.css':'text/css','.json':'application/json','.png':'image/png','.svg':'image/svg+xml'}[path.extname(file)]||'application/octet-stream';
    if(file.endsWith('/index.html'))body=body.toString().replace('<script src="bootstrap.js">','<script src="preview-tauri.js"></script><script src="bootstrap.js">');
  }
  response.writeHead(200,{'Content-Type':type,'Cache-Control':'no-store'});response.end(body);
});
server.listen(Number(process.env.KVMFLOW_PREVIEW_PORT||4176),'127.0.0.1',()=>console.log('Synthetic mixed preview: http://127.0.0.1:'+server.address().port));
