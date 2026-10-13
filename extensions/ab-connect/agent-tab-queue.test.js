import assert from 'node:assert/strict'
import test from 'node:test'
import { createAgentTabQueue } from './agent-tab-queue.js'

function fixture() {
  let windowId = null
  let nextTab = 10
  const records = {}
  const tabMap = new Map()
  const removed = []
  const ownedTabs = new Set()
  const adopted = new Set()
  const windowOptions = []
  const chrome = {
    storage: { local: {
      async get() { return { ...records } },
      async set(values) { Object.assign(records, values) },
    } },
    windows: {
      async getAll() { return windowId == null ? [] : [{ id: windowId, state: 'normal' }] },
      async get(id) { return { id, type: 'normal', focused: false, state: 'normal' } },
      async create(options) {
        windowOptions.push(options)
        const tab = { id: nextTab++, windowId: 1, url: options.url, active: true }
        tabMap.set(tab.id, tab)
        return { id: 1, tabs: [tab] }
      },
    },
    tabs: {
      async query({ windowId }) { return [...tabMap.values()].filter(t => t.windowId === windowId) },
      async get(id) { return tabMap.get(id) },
      async create(options) {
        const tab = { ...options, id: nextTab++ }
        tabMap.set(tab.id, tab)
        return tab
      },
      async remove(id) { removed.push(id); tabMap.delete(id) },
    },
  }
  const errors = []
  const deps = {
    chrome, timeoutMs: 10, windowKey: 'window', placeholderKey: 'placeholder',
    getWindowId: () => windowId, setWindowId: id => { windowId = id },
    rejectWindow() {}, ownedTabs, agentPopups: new Set(),
    async loadOwnedTabs() {}, async loadAgentPopups() {}, async persistOwnedTabs() {},
    isAdopted: id => ownedTabs.has(id) || adopted.has(id), onCleanupError: e => errors.push(e),
  }
  return { chrome, deps, removed, ownedTabs, adopted, errors, windowOptions, get windowId() { return windowId } }
}

async function seedPlaceholder(f) {
  const win = await f.chrome.windows.create({ url: 'about:blank' })
  await f.chrome.storage.local.set({ window: win.id, placeholder: { windowId: win.id, tabId: win.tabs[0].id } })
}

test('expired tabs.create releases queue; late tab is closed without changing window identity', async () => {
  const f = fixture()
  const create = createAgentTabQueue(f.deps)
  await create('https://warm.test')
  const normal = f.chrome.tabs.create
  let finish
  let count = 0
  f.chrome.tabs.create = options => ++count === 1 ? new Promise(resolve => { finish = resolve }) : normal(options)
  const first = assert.rejects(create('https://first.test'), /createTarget: create tab/)
  const second = create('https://second.test')
  await first
  const tab = await second
  assert.ok(f.ownedTabs.has(tab.id))
  const winId = f.windowId
  finish({ id: 99, windowId: 999 })
  await new Promise(resolve => setTimeout(resolve, 0))
  assert.ok(f.removed.includes(99))
  assert.equal(f.windowId, winId)
  assert.equal(f.ownedTabs.has(99), false)
  assert.deepEqual(f.errors, [])
})

test('late created tab adopted by another request is not closed', async () => {
  const f = fixture()
  const create = createAgentTabQueue(f.deps)
  await create('https://warm.test')
  let finish
  f.chrome.tabs.create = () => new Promise(resolve => { finish = resolve })
  await assert.rejects(create('https://first.test'), /relay timeout/)
  f.adopted.add(99)
  finish({ id: 99 })
  await new Promise(resolve => setTimeout(resolve, 0))
  assert.equal(f.removed.includes(99), false)
})

test('normal creation uses the initial agent tab without a blank placeholder', async () => {
  const f = fixture()
  const create = createAgentTabQueue(f.deps)
  const first = await create('https://first.test')
  const second = await create('https://second.test')
  assert.deepEqual(f.windowOptions, [{ focused: false, url: 'https://first.test' }])
  assert.equal(second.active, false)
  assert.equal(second.windowId, first.windowId)
  assert.deepEqual([...f.ownedTabs], [first.id, second.id])
  assert.deepEqual(f.removed, [])
  assert.equal((await f.chrome.storage.local.get()).placeholder, null)
})

test('failed placeholder removal retains identity and reuses the same window on retry', async () => {
  const f = fixture()
  await seedPlaceholder(f)
  const remove = f.chrome.tabs.remove
  let fail = true
  f.chrome.tabs.remove = async id => {
    if (fail) { fail = false; throw new Error('transient removal failure') }
    return remove(id)
  }
  let created = 0
  const normal = f.chrome.windows.create
  f.chrome.windows.create = options => { created++; return normal(options) }
  const create = createAgentTabQueue(f.deps)
  await assert.rejects(create('https://first.test'), /transient removal failure/)
  assert.deepEqual((await f.chrome.storage.local.get()).placeholder, { windowId: 1, tabId: 10 })
  assert.equal((await create('https://second.test')).windowId, 1)
  assert.equal(created, 0)
  assert.equal((await f.chrome.storage.local.get()).placeholder, null)
})

test('concurrent sessions share one background agent window', async () => {
  const f = fixture()
  let windows = 0
  const normal = f.chrome.windows.create
  f.chrome.windows.create = options => { windows++; return normal(options) }
  const create = createAgentTabQueue(f.deps)
  const tabs = await Promise.all(Array.from({ length: 20 }, (_, i) => create(`https://agent.test/${i}`)))
  assert.equal(windows, 1)
  assert.equal(new Set(tabs.map(tab => tab.windowId)).size, 1)
  assert.equal(f.ownedTabs.size, 20)
})

test('unconfirmed placeholder removal retains the record and refuses success', async () => {
  const f = fixture()
  await seedPlaceholder(f)
  f.chrome.tabs.remove = async () => {}
  await assert.rejects(createAgentTabQueue(f.deps)('https://agent.test'), /removal is unconfirmed/)
  assert.deepEqual((await f.chrome.storage.local.get()).placeholder, { windowId: 1, tabId: 10 })
})

test('unsettled placeholder metadata is retained instead of becoming a user tab', async () => {
  const f = fixture()
  await seedPlaceholder(f)
  const query = f.chrome.tabs.query
  f.chrome.tabs.query = async options => (await query(options)).map(tab =>
    tab.id === 10 ? { ...tab, url: '', pendingUrl: 'about:blank' } : tab)
  const create = createAgentTabQueue(f.deps)
  await assert.rejects(create('https://agent.test'), /placeholder navigation is unconfirmed/)
  assert.deepEqual((await f.chrome.storage.local.get()).placeholder, { windowId: 1, tabId: 10 })
  await assert.rejects(create('https://again.test'), /refusing to create another window/)
  f.chrome.tabs.query = query
  assert.equal((await create('https://settled.test')).windowId, 1)
})

test('missing window record recovers an all-agent window instead of opening another', async () => {
  const f = fixture()
  const existing = await f.chrome.tabs.create({ url: 'https://agent.test', windowId: 7 })
  f.ownedTabs.add(existing.id)
  f.chrome.windows.getAll = async () => [{ id: 7, state: 'normal' }]
  f.chrome.windows.create = async () => { throw new Error('must reuse existing agent window') }
  const create = createAgentTabQueue(f.deps)
  assert.equal((await create('https://next.test')).windowId, 7)
})

test('unknown window tab inventory refuses instead of creating another window', async () => {
  const f = fixture()
  await f.chrome.storage.local.set({ window: 7 })
  f.chrome.tabs.query = async () => { throw new Error('inventory unavailable') }
  f.chrome.windows.create = async () => { throw new Error('must not create another window') }
  await assert.rejects(createAgentTabQueue(f.deps)('https://next.test'), /inventory unavailable/)
})

test('user tabs in the shared agent window hand it to the user and open one fresh agent window', async () => {
  const f = fixture()
  const create = createAgentTabQueue(f.deps)
  const first = await create('https://agent.test')
  await f.chrome.tabs.create({ url: 'https://user.test', windowId: 1 })
  const normal = f.chrome.windows.create
  let created = 0
  f.chrome.windows.create = async options => {
    created++
    const win = await normal(options)
    win.tabs[0].windowId = 2
    return { id: 2, tabs: win.tabs }
  }
  const next = await create('https://next.test')
  assert.equal(next.windowId, 2)
  assert.equal((await f.chrome.storage.local.get()).window, 2)
  assert.ok(f.ownedTabs.has(first.id), 'the agent tab left in the user window stays owned')
  assert.equal((await create('https://third.test')).windowId, 2)
  assert.equal(created, 1, 'later sessions share the fresh window')
})

test('window recovery skips a mixed window and finds the all-agent one', async () => {
  const f = fixture()
  for (const [url, windowId, owned] of [['https://old-agent.test', 1, true], ['https://user.test', 1, false], ['https://agent.test', 2, true]]) {
    const tab = await f.chrome.tabs.create({ url, windowId })
    if (owned) f.ownedTabs.add(tab.id)
  }
  f.chrome.windows.getAll = async () => [{ id: 1, state: 'normal' }, { id: 2, state: 'normal' }]
  f.chrome.windows.create = async () => { throw new Error('must reuse the all-agent window') }
  assert.equal((await createAgentTabQueue(f.deps)('https://next.test')).windowId, 2)
})

test('expired windows.create cannot publish late window and cleans only its created tabs', async () => {
  const f = fixture()
  const normal = f.chrome.windows.create
  let finish
  f.chrome.windows.create = () => new Promise(resolve => { finish = resolve })
  const create = createAgentTabQueue(f.deps)
  await assert.rejects(create('https://first.test'), /create agent window/)
  f.chrome.windows.create = normal
  await create('https://second.test')
  finish({ id: 99, tabs: [{ id: 98 }] })
  await new Promise(resolve => setTimeout(resolve, 0))
  assert.equal(f.windowId, 1)
  assert.ok(f.removed.includes(98))
})

for (const step of ['loadOwnedTabs', 'loadAgentPopups', 'persistOwnedTabs']) {
  test(`queue deadline bounds ${step} and next request can proceed`, async () => {
    const f = fixture()
    const create = createAgentTabQueue(f.deps)
    await create('https://warm.test')
    const normal = f.deps[step]
    f.deps[step] = () => new Promise(() => {})
    await assert.rejects(create('https://expired.test'), /relay timeout/)
    f.deps[step] = normal
    assert.ok((await create('https://next.test')).id)
  })
}

for (const [api, method] of [['windows', 'get'], ['windows', 'getAll'], ['tabs', 'query'], ['tabs', 'remove']]) {
  test(`queue deadline bounds chrome.${api}.${method}`, async () => {
    const f = fixture()
    const create = createAgentTabQueue(f.deps)
    if (method === 'remove') await seedPlaceholder(f)
    if (method === 'get' && api === 'windows' || method === 'query') await create('https://warm.test')
    const normal = f.chrome[api][method]
    f.chrome[api][method] = () => new Promise(() => {})
    await assert.rejects(create('https://expired.test'), /relay timeout/)
    f.chrome[api][method] = normal
    assert.ok((await create('https://next.test')).id)
  })
}

test('late storage window write is reconciled to current transaction', async () => {
  const f = fixture()
  const normal = f.chrome.storage.local.set
  let finish
  f.chrome.storage.local.set = values => new Promise(resolve => {
    finish = async () => { await normal(values); resolve() }
  })
  const create = createAgentTabQueue(f.deps)
  await assert.rejects(create('https://expired.test'), /persist agent window/)
  f.chrome.storage.local.set = normal
  await create('https://next.test')
  await finish()
  await new Promise(resolve => setTimeout(resolve, 0))
  const records = await f.chrome.storage.local.get()
  assert.equal(records.window, f.windowId)
  assert.equal(records.placeholder, null)
})

test('a stale placeholder record does not erase the persisted agent window after a worker restart', async () => {
  const f = fixture()
  // Persisted window 1 holds only an agent tab; its recorded placeholder is gone.
  f.chrome.storage.local.set({ window: 1, placeholder: { windowId: 1, tabId: 10 } })
  const agentTab = await f.chrome.tabs.create({ url: 'https://agent.test', windowId: 1 })
  f.ownedTabs.add(agentTab.id)
  let windowsCreated = 0
  const normal = f.chrome.windows.create
  f.chrome.windows.create = (options) => { windowsCreated++; return normal(options) }
  const records = await f.chrome.storage.local.get()
  assert.equal(records.window, 1)
  const create = createAgentTabQueue(f.deps)
  const tab = await create('https://next.test')
  assert.equal(tab.windowId, 1)
  assert.equal(windowsCreated, 0)
  const after = await f.chrome.storage.local.get()
  assert.equal(after.window, 1)
  assert.equal(after.placeholder, null)
})
