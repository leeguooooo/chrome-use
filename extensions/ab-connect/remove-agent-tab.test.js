import assert from 'node:assert/strict'
import test from 'node:test'

import { removeAgentTab } from './remove-agent-tab.js'

function deps({ ungroupError = null, removeError = null, agentTab = true } = {}) {
  const calls = []
  return {
    calls,
    isAgentTab: () => agentTab,
    ungroup: async (id) => {
      calls.push(['ungroup', id])
      if (ungroupError) throw new Error(ungroupError)
    },
    remove: async (id) => {
      calls.push(['remove', id])
      if (removeError) throw new Error(removeError)
    },
  }
}

test('a tab leaves its group before it is removed', async () => {
  const d = deps()
  await removeAgentTab(11, d)
  assert.deepEqual(d.calls, [['ungroup', 11], ['remove', 11]])
})

test('a failed ungroup still removes the tab', async () => {
  const d = deps({ ungroupError: 'Tab is not in a group' })
  await removeAgentTab(11, d)
  assert.deepEqual(d.calls, [['ungroup', 11], ['remove', 11]])
})

test('a failed removal is reported, not swallowed', async () => {
  const d = deps({ removeError: 'No tab with id: 11.' })
  await assert.rejects(removeAgentTab(11, d), /No tab with id: 11/)
})

test("a user's own tab is removed without leaving its group", async () => {
  const d = deps({ agentTab: false })
  await removeAgentTab(11, d)
  assert.deepEqual(d.calls, [['remove', 11]])
})
