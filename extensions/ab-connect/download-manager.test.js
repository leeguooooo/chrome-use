import assert from 'node:assert/strict'
import test from 'node:test'

import { clearDownloads, listDownloads, startDownload } from './download-manager.js'

test('startDownload waits for completion and returns the browser filename', async () => {
  let now = 0
  let polls = 0
  const result = await startDownload(
    { url: 'https://example.com/media.mp4', filename: '../media.mp4', timeoutMs: 1000 },
    {
      makeToken: () => 'token',
      now: () => now,
      sleep: async (ms) => {
        now += ms
      },
      download: async (options) => {
        assert.equal(options.filename, 'chrome-use/token/_media.mp4')
        return 17
      },
      search: async ({ id }) => {
        assert.equal(id, 17)
        polls += 1
        return [
          polls === 1
            ? { id, state: 'in_progress' }
            : {
                id,
                state: 'complete',
                filename: '/Users/test/Downloads/chrome-use/token/_media.mp4',
                finalUrl: 'https://cdn.example.com/media.mp4',
              },
        ]
      },
    },
  )

  assert.equal(result.state, 'complete')
  assert.equal(result.filename, '/Users/test/Downloads/chrome-use/token/_media.mp4')
})

test('listDownloads bounds the result count and clear erases history', async () => {
  let query
  const listed = await listDownloads(
    { limit: 500 },
    {
      search: async (value) => {
        query = value
        return [{ id: 1, state: 'complete', filename: '/tmp/a' }]
      },
    },
  )
  assert.equal(query.limit, 100)
  assert.equal(listed.downloads[0].id, 1)

  const cleared = await clearDownloads(
    {},
    {
      erase: async (value) => {
        assert.deepEqual(value, {})
        return [1, 2]
      },
    },
  )
  assert.equal(cleared.cleared, 2)
})

test('startDownload rejects non-http URLs before invoking Chrome', async () => {
  await assert.rejects(
    startDownload(
      { url: 'file:///tmp/private' },
      {
        download: async () => {
          throw new Error('must not run')
        },
      },
    ),
    /http:\/\/ or https:\/\//,
  )
})
