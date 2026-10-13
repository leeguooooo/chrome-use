import test from 'node:test';
import assert from 'node:assert/strict';
import {createUserTabOrganizer} from './user-tab-organizer.js';
function fixture() {
  let rows = [1,2,3].map((id,index)=>({id,index,windowId:10,url:`https://example.test/${id}`,title:`Page ${id}`,active:id===3,pinned:false,audible:false,incognito:false,groupId:-1}));
  const log=[];let stored={};let next=100;const groups=new Map();
  const env={
    storage:{get:async()=>structuredClone(stored),set:async value=>{stored=structuredClone(value);}},
    isAgent:id=>id===99,isAgentWindow:id=>id===99,
    tabs:{
      get:async id=>{const t=rows.find(x=>x.id===id);if(!t)throw Error('missing');return {...t};},
      group:async ({tabIds})=>{log.push(['group',tabIds]);const id=next++;groups.set(id,{id});for(const t of rows)if(tabIds.includes(t.id))t.groupId=id;return id;},
      ungroup:async id=>{log.push(['ungroup',id]);rows.find(t=>t.id===id).groupId=-1;},
      move:async (id,{index})=>{log.push(['move',id,index]);const [t]=rows.splice(rows.findIndex(t=>t.id===id),1);rows.splice(index,0,t);rows.forEach((x,i)=>x.index=i);},
    },
    groups:{update:async (id,props)=>{Object.assign(groups.get(id),props);},get:async id=>({...groups.get(id)})},
  };
  const request={version:1,id:'test-request-1',reviewed:true,groups:[{name:'Research',color:'blue',tabs:rows.slice(0,2).map(({id,index,windowId,url,title})=>({id,index,windowId,url,title}))}]};
  return {env,rows,log,request,groups,organizer:createUserTabOrganizer(env)};
}
test('native grouping is verified and duplicates do not dispatch again',async()=>{
 const f=fixture();const r=await f.organizer.organize(f.request);
 assert.deepEqual(r.results.map(x=>x.status),['grouped','grouped']);assert.equal(f.rows[2].groupId,-1);
 assert.equal(f.groups.get(100).title,'Research');assert.equal(f.groups.get(100).color,'blue');
 await f.organizer.organize(f.request);assert.equal(f.log.filter(x=>x[0]==='group').length,1);
 const changed=structuredClone(f.request);changed.groups[0].name='Other';await assert.rejects(f.organizer.organize(changed),/reused/);
});
test('grouping remains verified when Chrome rearranges tab indexes',async()=>{
 const f=fixture();
 f.env.tabs.group=async({tabIds})=>{
   f.log.push(['group',tabIds]);
   const id=100;f.groups.set(id,{id});
   f.rows.reverse();f.rows.forEach((tab,index)=>{tab.index=index;if(tabIds.includes(tab.id))tab.groupId=id;});
   return id;
 };
 const result=await f.organizer.organize(f.request);
 assert.deepEqual(result.results.map(x=>x.status),['grouped','grouped']);
});
test('every protected or changed page is skipped without mutation',async()=>{
 for(const patch of [{active:true},{pinned:true},{audible:true},{incognito:true},{groupId:55},{url:'https://changed.test/'},{title:'changed'},{windowId:99}]){
  const f=fixture();Object.assign(f.rows[0],patch);Object.assign(f.rows[1],patch);
  const result=await f.organizer.organize(f.request);assert.ok(result.results.every(x=>x.status==='skipped'));assert.equal(f.log.length,0);
 }
 const f=fixture();f.env.isAgent=()=>true;await f.organizer.organize(f.request);assert.equal(f.log.length,0);
});
test('window boundaries, request size and review markers are validated',async()=>{
 const f=fixture();const request=structuredClone(f.request);request.groups[0].tabs[1].windowId=20;
 await assert.rejects(f.organizer.organize(request),/cross windows/);
 await assert.rejects(f.organizer.organize({...f.request,reviewed:false}),/reviewed/);
 assert.equal(f.log.length,0);
});
test('undo restores only unchanged members and is idempotent',async()=>{
 const f=fixture();await f.organizer.organize(f.request);
 const result=await f.organizer.undo({version:1,id:f.request.id,reviewed:true});
 assert.deepEqual(result.undoResults.map(x=>x.status),['restored','restored']);assert.equal(f.rows[0].groupId,-1);
 const count=f.log.length;await f.organizer.undo({version:1,id:f.request.id,reviewed:true});assert.equal(f.log.length,count);
});
test('undo preserves newly active pages and user-renamed groups',async()=>{
 const f=fixture();await f.organizer.organize(f.request);f.groups.get(100).title='User edited';
 const result=await f.organizer.undo({version:1,id:f.request.id,reviewed:true});assert.ok(result.undoResults.every(x=>x.status==='skipped'));
 assert.equal(f.log.filter(x=>x[0]==='ungroup').length,0);
});
test('a failed API reply is unconfirmed and is never replayed after worker restart',async()=>{
 const f=fixture();const group=f.env.tabs.group;f.env.tabs.group=async params=>{await group(params);throw Error('reply lost');};
 const result=await f.organizer.organize(f.request);assert.ok(result.results.every(x=>x.status==='unconfirmed'));
 await createUserTabOrganizer(f.env).organize(f.request);assert.equal(f.log.filter(x=>x[0]==='group').length,1);
});
