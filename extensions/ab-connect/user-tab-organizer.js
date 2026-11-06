// Native grouping of explicitly reviewed user tabs. Separate from ABExt.call: the generic
// API continues to refuse foreign-tab mutations. This door groups only unchanged, ordinary,
// ungrouped background tabs and keeps a durable, idempotent undo record in this profile.
export const USER_TAB_GROUPS_VERSION = 1;
// Reported alongside the version (kept at 1 so existing callers still match): a group may name
// `groupId`, a group this organizer created earlier, to add tabs to it.
export const USER_TAB_GROUPS_JOIN = true;
// Also reported: a group this organizer created can be renamed when its title is still the one
// the caller saw (Thermo renaming groups it titled with a bare host to the site's name).
export const USER_TAB_GROUPS_RENAME = true;
const COLORS = new Set(['grey','blue','red','yellow','green','pink','purple','cyan','orange']);
const KEY = 'thermo_tab_organizer_v1';
// Native groups this organizer created. Only these can be joined, so a chrome-use agent group
// (or any group the user made) is never a target, whatever its title.
const CREATED = 'thermo_tab_organizer_groups_v1';
const UUID = /^[a-zA-Z0-9_-]{8,100}$/;

function validate(request) {
  if (request?.version !== 1 || !UUID.test(request?.id || '') || request?.reviewed !== true)
    throw Error('organizeTabs: version, stable request id and reviewed selection are required');
  if (!Array.isArray(request.groups) || !request.groups.length || request.groups.length > 100)
    throw Error('organizeTabs: choose 1–100 groups');
  let count = 0; const seen = new Set();
  for (const group of request.groups) {
    if (typeof group.name !== 'string' || !group.name.trim() || group.name.length > 100 || !COLORS.has(group.color))
      throw Error('organizeTabs: invalid group name or color');
    if (group.groupId !== undefined && (!Number.isInteger(group.groupId) || group.groupId < 0))
      throw Error('organizeTabs: invalid groupId');
    if (!Array.isArray(group.tabs) || !group.tabs.length) throw Error('organizeTabs: empty group');
    let window;
    for (const tab of group.tabs) {
      if (!Number.isInteger(tab.id) || !Number.isInteger(tab.windowId) || !Number.isInteger(tab.index) || tab.index < 0 ||
          typeof tab.url !== 'string' || !/^https?:\/\//i.test(tab.url) || typeof tab.title !== 'string' || seen.has(tab.id))
        throw Error('organizeTabs: invalid or duplicate tab identity');
      if (window !== undefined && window !== tab.windowId) throw Error('organizeTabs: a group cannot cross windows');
      window = tab.windowId; seen.add(tab.id); count++;
    }
  }
  if (count > 1000 || JSON.stringify(request).length > 750000) throw Error('organizeTabs: selection exceeds the bounded request limit');
}

export function userTabProtected(tab, env) {
  return !tab || tab.active !== false || tab.pinned !== false || tab.audible !== false ||
    tab.incognito !== false || env.isAgent(tab.id) || env.isAgentWindow(tab.windowId) ||
    !/^https?:\/\//i.test(tab.url || '');
}
function sameIdentity(tab, expected) {
  return tab.id === expected.id && tab.windowId === expected.windowId &&
    tab.url === expected.url && tab.title === expected.title;
}

export function createUserTabOrganizer(env) {
  let queue = Promise.resolve();
  const serial = (work) => {
    const result = queue.then(work, work);
    queue = result.catch(() => {}); return result;
  };
  const load = async () => (await env.storage.get(KEY))?.[KEY] || {};
  const save = async records => {
    // Retain incomplete operations; bound completed history by count and storage size.
    const complete = Object.values(records).filter(r => r.phase === 'complete' || r.phase === 'undone').sort((a,b)=>b.created-a.created);
    for (const r of complete.slice(30)) delete records[r.id];
    for (const r of complete.slice().reverse()) {
      if (JSON.stringify(records).length <= 7_000_000) break;
      delete records[r.id];
    }
    await env.storage.set({[KEY]: records});
  };
  // Also the groups in this organizer's own batch records: builds before CREATED existed made
  // groups they never listed there, and those must stay joinable or every later run duplicates them.
  const created = async () => new Set([...((await env.storage.get(CREATED))?.[CREATED] || []),
    ...Object.values(await load()).flatMap(r => (r.originals || []).map(t => t.newGroupId)).filter(Number.isInteger)]);
  const remember = async id => {
    const ids = [...(await created()), id];
    await env.storage.set({[CREATED]: ids.slice(-1000)});
  };
  // The group to add to: one this organizer made, still in the tabs' window with the reviewed
  // title. Anything else means a new group.
  const joinable = async (group, windowId) => {
    if (group.groupId === undefined || !(await created()).has(group.groupId)) return undefined;
    let info; try { info = await env.groups.get(group.groupId); } catch {}
    return info && info.windowId === windowId && info.title === group.name ? info : undefined;
  };
  const response = record => ({version:1,id:record.id,phase:record.phase,results:record.results || [],undoResults:record.undoResults || []});
  return {
    organize(request) { return serial(async () => {
      validate(request);
      const records = await load(); const fingerprint = JSON.stringify(request);
      if (records[request.id]) {
        if (records[request.id].fingerprint !== fingerprint) throw Error('organizeTabs: request id reused with a different selection');
        return response(records[request.id]); // never replay uncertain delivery
      }
      const record = {id:request.id,created:Date.now(),phase:'applying',fingerprint,results:[],originals:[]};
      records[request.id] = record; await save(records);
      for (const group of request.groups) {
        const accepted = [];
        for (const expected of group.tabs) {
          let tab; try { tab = await env.tabs.get(expected.id); } catch {}
          if (!tab || !sameIdentity(tab,expected) || userTabProtected(tab,env) || tab.groupId !== -1) {
            record.results.push({tabId:expected.id,status:'skipped'}); continue;
          }
          accepted.push(expected);
        }
        if (!accepted.length) { await save(records); continue; }
        // Re-read immediately before mutation; never reuse a native group by title alone.
        const fresh = [];
        for (const expected of accepted) {
          let tab; try { tab=await env.tabs.get(expected.id); } catch {}
          if (tab && sameIdentity(tab,expected) && !userTabProtected(tab,env) && tab.groupId === -1) fresh.push(expected);
          else record.results.push({tabId:expected.id,status:'skipped'});
        }
        if (!fresh.length) { await save(records); continue; }
        record.originals.push(...fresh.map(t=>({...t,groupName:group.name,groupColor:group.color})));
        await save(records); // original positions survive worker termination
        let groupId;
        try {
          const existing = await joinable(group, fresh[0].windowId);
          if (existing) {
            groupId = existing.id;
            await env.tabs.group({groupId, tabIds:fresh.map(t=>t.id)});
          } else {
            // windowId keeps the group in the tabs' own window; without it Chrome builds the group
            // in the focused window and moves the tabs there.
            groupId = await env.tabs.group({tabIds:fresh.map(t=>t.id), createProperties:{windowId:fresh[0].windowId}});
            await remember(groupId);
          }
          for (const original of record.originals.filter(t=>fresh.some(x=>x.id===t.id))) {
            original.newGroupId = groupId;
            // Undo checks the group still looks the way this batch left it.
            if (existing) original.groupColor = existing.color;
          }
          await save(records);
          if (!existing) await env.groups.update(groupId,{title:group.name,color:group.color,collapsed:false});
          const info = await env.groups.get(groupId);
          const color = existing ? existing.color : group.color;
          for (const expected of fresh) {
            let tab; try { tab=await env.tabs.get(expected.id); } catch {}
            const verified = tab && sameIdentity(tab,expected) && tab.groupId === groupId && info.title === group.name && info.color === color;
            record.results.push({tabId:expected.id,groupId,status:verified?'grouped':'unconfirmed'});
          }
        } catch {
          for (const expected of fresh) record.results.push({tabId:expected.id,groupId,status:'unconfirmed'});
        }
        await save(records);
      }
      record.phase='complete'; await save(records); return response(record);
    }); },
    rename(request) { return serial(async () => {
      if (request?.version !== 1 || !UUID.test(request?.id || '') || request?.reviewed !== true ||
          !Array.isArray(request.groups) || !request.groups.length || request.groups.length > 100)
        throw Error('renameTabGroups: version, request id and 1–100 reviewed groups are required');
      const mine = await created(); const records = await load(); const results = [];
      for (const g of request.groups) {
        if (!Number.isInteger(g?.groupId) || typeof g.from !== 'string' || typeof g.to !== 'string' || !g.to.trim() || g.to.length > 100) {
          results.push({groupId: g?.groupId ?? -1, status: 'invalid'}); continue;
        }
        // Only a group this organizer made, still titled as the caller saw it: never a user's or an agent's group.
        let info; try { info = await env.groups.get(g.groupId); } catch {}
        if (!mine.has(g.groupId) || !info || info.title !== g.from) { results.push({groupId: g.groupId, status: 'skipped'}); continue; }
        await env.groups.update(g.groupId, {title: g.to});
        // Undo checks a group still carries the title its batch gave it; carry the new one over.
        for (const r of Object.values(records)) for (const t of r.originals || []) if (t.newGroupId === g.groupId && t.groupName === g.from) t.groupName = g.to;
        let after; try { after = await env.groups.get(g.groupId); } catch {}
        results.push({groupId: g.groupId, status: after?.title === g.to ? 'renamed' : 'unconfirmed'});
      }
      await save(records);
      return {version: 1, id: request.id, results};
    }); },
    undo(request) { return serial(async () => {
      if (request?.version !== 1 || !UUID.test(request?.id || '') || request?.reviewed !== true)
        throw Error('undoOrganizeTabs: reviewed batch id is required');
      const records=await load();const record=records[request.id];
      if (!record) throw Error('undoOrganizeTabs: saved operation no longer available in this Chrome profile');
      if (record.phase==='undone' || record.phase==='undoing') return response(record);
      record.phase='undoing';record.undoResults=[];await save(records);
      const originals=[...record.originals].sort((a,b)=>a.windowId-b.windowId || a.index-b.index);
      for (const expected of originals) {
        let tab,group;
        try { tab=await env.tabs.get(expected.id); group=await env.groups.get(expected.newGroupId); } catch {}
        if (!tab || !sameIdentity(tab,expected) || userTabProtected(tab,env) || tab.groupId!==expected.newGroupId ||
            group?.title!==expected.groupName || group?.color!==expected.groupColor) {
          record.undoResults.push({tabId:expected.id,status:'skipped'});await save(records);continue;
        }
        // Unknown delivery must not be replayed after a worker restart or concurrent caller.
        record.undoResults.push({tabId:expected.id,status:'undoing'});await save(records);
        const outcome=record.undoResults.at(-1);
        try {
          await env.tabs.ungroup(expected.id);
          await env.tabs.move(expected.id,{index:expected.index});
          const current=await env.tabs.get(expected.id);
          outcome.status=sameIdentity(current,expected)&&current.groupId===-1&&current.index===expected.index?'restored':'unconfirmed';
        } catch { outcome.status='unconfirmed'; }
        await save(records);
      }
      record.phase='undone';await save(records);return response(record);
    }); },
  };
}
