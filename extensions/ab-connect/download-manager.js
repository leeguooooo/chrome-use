function safeFilename(name) {
  const cleaned = String(name || 'download')
    .replace(/[\\/]/g, '_')
    .replace(/^\.+/, '')
    .trim()
  return cleaned || 'download'
}

function publicDownload(item) {
  return {
    id: item.id,
    url: item.finalUrl || item.url || null,
    filename: item.filename || null,
    state: item.state || null,
    danger: item.danger || null,
    mime: item.mime || null,
    bytesReceived: item.bytesReceived ?? null,
    totalBytes: item.totalBytes ?? null,
    startTime: item.startTime || null,
    endTime: item.endTime || null,
    error: item.error || null,
  }
}

export async function startDownload(params, deps) {
  const url = String(params?.url || '').trim()
  if (!/^https?:\/\//i.test(url)) {
    throw new Error('downloadUrl: url must use http:// or https://')
  }

  const options = {
    url,
    conflictAction: 'uniquify',
    saveAs: false,
  }
  if (params?.filename) {
    const token = deps.makeToken()
    options.filename = `chrome-use/${token}/${safeFilename(params.filename)}`
  }
  const id = await deps.download(options)

  const timeoutMs = Math.max(1, Number(params?.timeoutMs) || 30000)
  const deadline = deps.now() + timeoutMs
  while (deps.now() < deadline) {
    const [item] = await deps.search({ id })
    if (item?.state === 'complete') return publicDownload(item)
    if (item?.state === 'interrupted') {
      throw new Error(`downloadUrl: interrupted${item.error ? ` (${item.error})` : ''}`)
    }
    await deps.sleep(100)
  }

  throw new Error(`downloadUrl: timed out after ${timeoutMs}ms`)
}

export async function listDownloads(params, deps) {
  const limit = Math.min(100, Math.max(1, Number(params?.limit) || 20))
  const items = await deps.search({ orderBy: ['-startTime'], limit })
  return { downloads: items.map(publicDownload) }
}

export async function clearDownloads(_params, deps) {
  const erasedIds = await deps.erase({})
  return { cleared: erasedIds.length }
}
