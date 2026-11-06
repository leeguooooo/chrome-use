import test from 'node:test';
import assert from 'node:assert/strict';
import {createUserTabOrganizer} from './user-tab-organizer.js';
function fixture() {
  let rows = [1,2,3].map((id,index)=>({id,index,windowId:10,url:`https://example.test/${id}`,title:`Page ${id}`,active:id===3,pinned:false,audible:false,incognito:false,groupId:-1}));
  const log=[];let stored={};let next=100;const groups=new Map();
  const env={
    storage:{get:async()=>structuredClone(stored),set:async value=>{stored={...stored,...structuredClone(value)};}},
    isAgent:id=>id===99,isAgentWindow:id=>id===99,
    tabs:{
      get:async id=>{const t=rows.find(x=>x.id===id);if(!t)throw Error('missing');return {...t};},
      group:async ({tabIds,groupId,createProperties})=>{log.push(['group',tabIds,groupId,createProperties?.windowId]);const id=groupId??next++;if(!groups.has(id))groups.set(id,{id,windowId:createProperties?.windowId??rows.find(t=>t.id===tabIds[0]).windowId});for(const t of rows)if(tabIds.includes(t.id))t.groupId=id;return id;},
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

test('a later batch joins the group this organizer made instead of adding a duplicate',async()=>{
 const f=fixture();
 f.request.groups[0].tabs=f.request.groups[0].tabs.slice(0,1);
 await f.organizer.organize(f.request);
 assert.equal(f.rows[0].groupId,100);assert.deepEqual(f.log[0].slice(3),[10],'new groups stay in the tabs\' window');
 f.rows[2].active=false;
 const second={version:1,id:'test-request-2',reviewed:true,groups:[{name:'Research',color:'blue',groupId:100,
   tabs:[f.rows[2]].map(({id,index,windowId,url,title})=>({id,index,windowId,url,title}))}]};
 const r=await f.organizer.organize(second);
 assert.deepEqual(r.results,[{tabId:3,groupId:100,status:'grouped'}]);
 assert.equal(f.groups.size,1,'no second group');
 const undone=await f.organizer.undo({version:1,id:'test-request-2',reviewed:true});
 assert.deepEqual(undone.undoResults.map(x=>x.status),['restored']);assert.equal(f.rows.find(t=>t.id===1).groupId,100,'the first batch keeps its group');
});
test('a group made before the created list existed is still joined from the batch records',async()=>{
 const f=fixture();
 f.request.groups[0].tabs=f.request.groups[0].tabs.slice(0,1);
 await f.organizer.organize(f.request);
 const stored=await f.env.storage.get();delete stored.thermo_tab_organizer_groups_v1;
 f.env.storage.get=async()=>structuredClone(stored);
 f.rows[2].active=false;
 const second={version:1,id:'test-request-2',reviewed:true,groups:[{name:'Research',color:'blue',groupId:100,
   tabs:[f.rows[2]].map(({id,index,windowId,url,title})=>({id,index,windowId,url,title}))}]};
 const r=await f.organizer.organize(second);
 assert.deepEqual(r.results,[{tabId:3,groupId:100,status:'grouped'}]);
 assert.equal(f.groups.size,1,'no second group');
});
test('a group the organizer did not make is never joined, whatever its title',async()=>{
 const f=fixture();
 // e.g. a chrome-use agent session group titled like the reviewed group
 f.groups.set(7,{id:7,windowId:10,title:'Research',color:'blue'});
 f.request.groups[0].groupId=7;
 const r=await f.organizer.organize(f.request);
 assert.deepEqual(r.results.map(x=>x.status),['grouped','grouped']);
 assert.equal(f.rows[0].groupId,100,'a new group, not the foreign one');assert.equal(f.groups.get(7).title,'Research');
});
test('a renamed or moved group is not joined',async()=>{
 for(const patch of [{title:'Renamed'},{windowId:11}]){
   const f=fixture();
   f.request.groups[0].tabs=f.request.groups[0].tabs.slice(0,1);
   await f.organizer.organize(f.request);Object.assign(f.groups.get(100),patch);
   f.rows[2].active=false;
   const second={version:1,id:'test-request-2',reviewed:true,groups:[{name:'Research',color:'blue',groupId:100,
     tabs:[f.rows[2]].map(({id,index,windowId,url,title})=>({id,index,windowId,url,title}))}]};
   const r=await f.organizer.organize(second);
   assert.equal(r.results[0].groupId,101,JSON.stringify(patch));
 }
});
test('rename touches only groups this organizer made, still titled as reviewed, and keeps undo working',async()=>{
 const f=fixture();await f.organizer.organize(f.request);
 f.groups.set(500,{id:500,windowId:10,title:'Research',color:'blue'}); // the user's own group, same title
 const r=await f.organizer.rename({version:1,id:'rename-request-1',reviewed:true,groups:[
   {groupId:100,from:'Research',to:'研究'},{groupId:500,from:'Research',to:'研究'},{groupId:100,from:'Stale',to:'X'}]});
 assert.deepEqual(r.results.map(x=>x.status),['renamed','skipped','skipped']);
 assert.equal(f.groups.get(100).title,'研究');assert.equal(f.groups.get(500).title,'Research');
 const undone=await f.organizer.undo({version:1,id:'test-request-1',reviewed:true});
 assert.deepEqual(undone.undoResults.map(x=>x.status),['restored','restored']);
});
