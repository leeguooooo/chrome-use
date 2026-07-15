import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import test from 'node:test'
import vm from 'node:vm'

const source = readFileSync(new URL('./tab-duplicate.js', import.meta.url), 'utf8')
const context = vm.createContext({})
vm.runInContext(source, context)
const { duplicateTab } = context.ABTabDuplicate

function fixture(overrides = {}) {
  const calls = []
  const deps = {
    tabForTarget: (targetId) => (targetId === 'source-target' ? 11 : null),
    getTab: async (tabId) => ({ id: tabId, windowId: 3, url: 'https://example.com/b' }),
    eligible: (tab) => Boolean(tab?.id && tab?.url),
    getLastFocusedWindow: async () => ({ id: 3 }),
    getActiveTabs: async () => [{ id: 7 }],
    duplicateTab: async (tabId) => {
      calls.push(['duplicate', tabId])
      return { id: 22 }
    },
    markOwned: (tabId) => calls.push(['markOwned', tabId]),
    unmarkOwned: (tabId) => calls.push(['unmarkOwned', tabId]),
    groupTabInto: async (tabId, group) => calls.push(['group', tabId, group]),
    attachTab: async (tabId) => {
      calls.push(['attach', tabId])
      return { targetId: 'duplicate-target' }
    },
    activateTab: async (tabId) => calls.push(['activate', tabId]),
    focusWindow: async () => {},
    removeTab: async (tabId) => calls.push(['remove', tabId]),
    ...overrides,
  }
  return { calls, deps }
}

const params = { sourceTargetId: 'source-target', agentGroup: 'agent-a' }

test('native duplicate is grouped, attached, owned, and restores the foreground tab', async () => {
  const { calls, deps } = fixture()
  const result = await duplicateTab(params, deps)

  assert.equal(result.sourceTargetId, 'source-target')
  assert.equal(result.targetId, 'duplicate-target')
  assert.deepEqual(calls, [
    ['duplicate', 11],
    ['markOwned', 22],
    ['group', 22, 'agent-a'],
    ['attach', 22],
    ['activate', 7],
  ])
})

test('native duplicate restores the active tab and focus from another window', async () => {
  const { calls, deps } = fixture({
    getLastFocusedWindow: async () => ({ id: 9 }),
    getActiveTabs: async (windowId) => {
      calls.push(['getActiveTabs', windowId])
      return [{ id: 70 }]
    },
    focusWindow: async (windowId) => calls.push(['focusWindow', windowId]),
  })
  await duplicateTab(params, deps)

  assert.ok(calls.some((call) => call[0] === 'getActiveTabs' && call[1] === 9))
  assert.ok(calls.some((call) => call[0] === 'activate' && call[1] === 70))
  assert.ok(calls.some((call) => call[0] === 'focusWindow' && call[1] === 9))
})

test('native duplicate failure is returned without orphan cleanup', async () => {
  const { calls, deps } = fixture({
    duplicateTab: async () => Promise.reject(new Error('native duplicate failed')),
  })
  await assert.rejects(duplicateTab(params, deps), /native duplicate failed/)
  assert.ok(!calls.some((call) => call[0] === 'unmarkOwned'))
  assert.ok(!calls.some((call) => call[0] === 'remove'))
})

for (const [stage, failure] of [
  ['group', { groupTabInto: async () => Promise.reject(new Error('group failed')) }],
  ['attach', { attachTab: async () => Promise.reject(new Error('attach failed')) }],
  ['restore', { activateTab: async () => Promise.reject(new Error('restore failed')) }],
]) {
  test(`${stage} failure removes the orphaned duplicate`, async () => {
    const { calls, deps } = fixture(failure)
    await assert.rejects(duplicateTab(params, deps), new RegExp(`${stage} failed`))
    assert.ok(calls.some((call) => call[0] === 'unmarkOwned' && call[1] === 22))
    assert.ok(calls.some((call) => call[0] === 'remove' && call[1] === 22))
  })
}
