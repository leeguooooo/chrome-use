import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, mkdir, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
if (process.env.CHROME_USE_ISOLATED_GUI_HOST !== '1') throw new Error('Isolated GUI host required');
const out = process.argv[2];
if (!out) throw new Error('Provide a fresh output directory');
await mkdir(out);
const profile = await mkdtemp(join(tmpdir(), 'cu721-cap-'));
const chrome = spawn(process.env.CHROME_BINARY || '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome', [`--user-data-dir=${profile}`, '--remote-debugging-port=0', '--no-first-run', '--no-default-browser-check', '--window-size=900,700', 'about:blank'], {stdio:'ignore'});
const sleep = ms => new Promise(r=>setTimeout(r,ms));
let ws;let seq=0;const pending=new Map();const report={isolatedProfile:true,mode:'independent capability and hinge-capture research; no product fallback; no Overlay.enable; no input sent',platform:process.platform,arch:process.arch,probeSha256:createHash('sha256').update(await readFile(new URL(import.meta.url))).digest('hex')};
try {
 let port;
 for(let i=0;i<100;i++){try{port=Number((await readFile(join(profile,'DevToolsActivePort'),'utf8')).split('\n')[0]);break;}catch{}await sleep(50);}
 if(!port)throw new Error('No private Chrome endpoint');
 const version=await(await fetch(`http://127.0.0.1:${port}/json/version`)).json();
 report.browser={product:version.Browser,protocol:version['Protocol-Version'],userAgent:version['User-Agent']};
 const protocol=await(await fetch(`http://127.0.0.1:${port}/json/protocol`)).json();
 report.declared=protocol.domains.find(d=>d.domain==='Page')?.commands?.find(c=>c.name==='setInputVisualization')??null;
 ws=new WebSocket(version.webSocketDebuggerUrl);await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j;});
 ws.onmessage=e=>{const m=JSON.parse(e.data);const p=pending.get(m.id);if(p){pending.delete(m.id);m.error?p.reject(m.error):p.resolve(m.result);}};
 const call=(method,params={},sessionId)=>new Promise((resolve,reject)=>{const id=++seq;pending.set(id,{resolve,reject});ws.send(JSON.stringify({id,method,params,...(sessionId?{sessionId}:{})}));setTimeout(()=>{if(pending.delete(id))reject({message:'timeout: '+method});},10000).unref();});
 const targets=await call('Target.getTargets');const target=targets.targetInfos.find(t=>t.type==='page');const sid=(await call('Target.attachToTarget',{targetId:target.targetId,flatten:true})).sessionId;
 try{report.reply=await call('Page.setInputVisualization',{clicks:true},sid);report.supported=true;}catch(error){report.supported=error.code===-32601?false:null;report.error=error;}
 if(report.supported===true){try{await call('Page.setInputVisualization',{clicks:false},sid);report.disabled=true;}catch(error){report.disabled=false;report.disableError=error;process.exitCode=2;}}
 await call('Page.enable',{},sid);
 await call('Page.bringToFront',{},sid);report.activatedBeforeBaseline=true;await sleep(150);
 report.geometry=(await call('Runtime.evaluate',{expression:'({dpr:devicePixelRatio,viewport:[innerWidth,innerHeight],visualScale:visualViewport.scale})',returnByValue:true},sid)).result.value;
 report.captures=[];
 const capture=async(label,fromSurface=true)=>{
   const start=performance.now();
   try{
     const shot=await call('Page.captureScreenshot',{format:'png',fromSurface,captureBeyondViewport:false},sid);
     await writeFile(join(out,label+'.png'),Buffer.from(shot.data,'base64'));
     report.captures.push({label,fromSurface,elapsedMs:performance.now()-start});
   }catch(error){report.captures.push({label,fromSurface,error});}
 };
 const show=()=>call('Overlay.setShowHinge',{hingeConfig:{rect:{x:100,y:100,width:24,height:24},contentColor:{r:255,g:0,b:255,a:1},outlineColor:{r:255,g:0,b:255,a:1}}},sid);
 const hide=()=>call('Overlay.setShowHinge',{},sid);
 await capture('baseline-surface');await capture('baseline-view',false);
 await show();await sleep(150);
 await capture('visible-surface');await capture('visible-view',false);await capture('surface-after-view');
 report.hideAcks=[];
 for(let i=0;i<5;i++){
   await show();await sleep(40);await capture('pre-hide-'+i);
   const start=performance.now();await hide();const ackMs=performance.now()-start;
   await capture('hidden-ack-'+i);report.hideAcks.push({i,ackMs,totalMs:performance.now()-start});
 }
 for(let i=0;i<5;i++){
   await show();await sleep(40);await capture('pre-parallel-'+i);
   const hiding=hide();await capture('hidden-parallel-'+i);await hiding;
 }
 await hide();
 await call('Browser.close');report.closed=true;
 if(report.captures.some(c=>c.error))process.exitCode=2;
}catch(error){report.failure=String(error?.message||error);report.closed=false;process.exitCode=1;}
finally{ws?.close();chrome.kill();await writeFile(join(out,'capture-matrix.json'),JSON.stringify(report,null,2));}
console.log(JSON.stringify(report));
