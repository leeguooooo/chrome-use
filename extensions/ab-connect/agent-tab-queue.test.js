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
  const chrome = {
    storage: { local: {
      async get() { return { ...records } },
      async set(values) { Object.assign(records, values) },
    } },
    windows: {
      async get(id) { return { id, type: 'normal', focused: false, state: 'normal' } },
      async create() {
        const tab = { id: nextTab++, windowId: 1, url: 'about:blank' }
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
  return { chrome, deps, removed, ownedTabs, adopted, errors, get windowId() { return windowId } }
}

test('expired tabs.create releases queue; late tab is closed without changing window identity', async () => {
  const f = fixture()
  const normal = f.chrome.tabs.create
  let finish
  let count = 0
  f.chrome.tabs.create = options => ++count === 1 ? new Promise(resolve => { finish = resolve }) : normal(options)
  const create = createAgentTabQueue(f.deps)
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
  let finish
  f.chrome.tabs.create = () => new Promise(resolve => { finish = resolve })
  const create = createAgentTabQueue(f.deps)
  await assert.rejects(create('https://first.test'), /relay timeout/)
  f.adopted.add(99)
  finish({ id: 99 })
  await new Promise(resolve => setTimeout(resolve, 0))
  assert.equal(f.removed.includes(99), false)
})

test('normal creation stays in background, marks ownership and removes only placeholder', async () => {
  const f = fixture()
  const create = createAgentTabQueue(f.deps)
  const first = await create('https://first.test')
  const second = await create('https://second.test')
  assert.equal(first.active, false)
  assert.equal(second.windowId, first.windowId)
  assert.deepEqual([...f.ownedTabs], [first.id, second.id])
  assert.deepEqual(f.removed, [10])
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

for (const [api, method] of [['windows', 'get'], ['tabs', 'query'], ['tabs', 'get'], ['tabs', 'remove']]) {
  test(`queue deadline bounds chrome.${api}.${method}`, async () => {
    const f = fixture()
    const create = createAgentTabQueue(f.deps)
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
