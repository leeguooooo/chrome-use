import { test } from 'node:test'
import assert from 'node:assert/strict'
import { clearExtensionFrames, removeForeignExtensionFrames, PASSWORD_MANAGER_FRAME_HOSTS } from './extension-frames.js'

const OWN = 'knfcmbamhjmaonkfnjhldjedeobeafmk'
const OP = 'aeblfdkhhhdcdjpifhhbdiojplfjncoa'

// A tiny DOM: elements with children, an optional (closed) shadow root, remove().
function el(tagName, attrs = {}, children = []) {
  const node = { tagName: tagName.toUpperCase(), ...attrs, children: [], parent: null, shadowRoot: null, rootNode: null }
  node.getAttribute = (k) => attrs[k] ?? null
  node.getRootNode = () => node.rootNode
  node.remove = () => {
    const list = node.parent.children
    list.splice(list.indexOf(node), 1)
  }
  for (const c of children) { c.parent = node; node.children.push(c) }
  return node
}
function all(node) {
  return node.children.flatMap((c) => [c, ...all(c)])
}
function root(children) {
  const r = { children: [], host: null }
  for (const c of children) { c.parent = r; r.children.push(c) }
  r.querySelectorAll = () => all(r)
  r.getElementsByTagName = (tag) => all(r).filter((n) => n.tagName === tag.toUpperCase())
  return r
}
function withShadow(host, children) {
  const shadow = root(children)
  shadow.host = host
  for (const n of all(shadow)) n.rootNode = shadow
  host.closedShadow = shadow
  return host
}

test('a foreign extension frame in a closed shadow root goes; the page keeps its own frames', () => {
  const opFrame = el('iframe', { src: `chrome-extension://${OP}/inline/universal-sign-on/universal-sign-on.html?tabId=1` })
  const uso = withShadow(el('com-1password-uso'), [opFrame])
  const ownFrame = el('iframe', { src: `chrome-extension://${OWN}/inspect-frame.html` })
  const pageFrame = el('iframe', { src: 'https://login.example.com/form' })
  const doc = root([el('div', {}, [pageFrame, ownFrame]), uso])
  // chrome.dom.openOrClosedShadowRoot stand-in.
  globalThis.chrome = { dom: { openOrClosedShadowRoot: (n) => n.closedShadow || null } }
  try {
    const { removed } = removeForeignExtensionFrames(OWN, PASSWORD_MANAGER_FRAME_HOSTS, doc)
    assert.deepEqual(removed, [
      { extension: OP, tag: 'iframe', host: 'com-1password-uso' },
    ])
    assert.equal(uso.closedShadow.children.length, 0)
    // The empty host goes too, so it cannot put the frame back.
    assert.equal(doc.getElementsByTagName('com-1password-uso').length, 0)
    assert.ok(doc.querySelectorAll().includes(pageFrame))
    assert.ok(doc.querySelectorAll().includes(ownFrame))
  } finally {
    delete globalThis.chrome
  }
})

test('nothing foreign: nothing removed', () => {
  const doc = root([el('iframe', { src: 'https://x.test/' })])
  assert.deepEqual(removeForeignExtensionFrames(OWN, PASSWORD_MANAGER_FRAME_HOSTS, doc).removed, [])
})

test('only an agent tab, only web frames, in the isolated world', async () => {
  const calls = []
  const deps = {
    isOwned: (id) => id === 7,
    getAllFrames: async () => [
      { frameId: 0, url: 'https://www.aliexpress.com/' },
      { frameId: 3, url: 'https://login.aliexpress.com/identity' },
      { frameId: 4, url: `chrome-extension://${OP}/inline/menu/menu.html` },
    ],
    executeScript: async (injection) => {
      calls.push(injection)
      return [{ result: { removed: [{ extension: OP, tag: 'iframe', host: 'com-1password-uso' }] } }, { result: { removed: [] } }]
    },
    ownId: OWN,
  }
  const out = await clearExtensionFrames(7, deps)
  assert.deepEqual(out, { removed: [{ extension: OP, tag: 'iframe', host: 'com-1password-uso' }], frames: 2 })
  assert.deepEqual(calls[0].target, { tabId: 7, frameIds: [0, 3] })
  assert.equal(calls[0].world, 'ISOLATED')
  assert.equal(calls[0].args[0], OWN)
  await assert.rejects(clearExtensionFrames(8, deps), /only a tab chrome-use created/)
  await assert.rejects(
    clearExtensionFrames(7, { ...deps, executeScript: async () => { throw new Error('Cannot access contents of the page') } }),
    /no site access/,
  )
})
