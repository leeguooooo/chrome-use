# `diag pages` — JSON schema 1

`chrome-use diag pages [--measure] [--watch <seconds>] [--force] [--limit <n>] --json`
prints one JSON object. This page is the contract for programs that read it
(Thermo and others). Within schema 1, keys are only ever **added**; a key is
never removed, renamed or given another meaning. A change that would break a
reader of this page gets `"schema": 2`. Check `schema` first.

## What a run touches

| Options | Attaches | Reads |
|---|---|---|
| none | nothing | relay: `ABExt.state`, `chrome.tabs.query`, `chrome.windows.getAll`, the relay's target records, `ABExt.tabPresence`; CDP: `Target.getTargets`, `Browser.getWindowForTarget` |
| `--measure` | nothing new | + `Performance.getMetrics` on pages this session owns and on pages chrome-use already holds attached that no other running session holds |
| `--measure --force` | relay: tabs that are not attached, one at a time, released right after (`ABExt.releaseTab`, ab-connect 0.5.34+); CDP: a private session per page, detached right after | + every other page, and worker heaps (`Runtime.getHeapUsage`) |

Nothing is ever activated, focused, moved, reloaded or navigated. A discarded
tab is never attached (attaching would reload it). An extension older than
0.5.34 cannot release a tab, so with it `--force` skips tabs that are not
already attached (`release_unsupported`). The run uses its own diagnostic
connection: no daemon starts, no session tab opens, the relay is not
restarted. `--watch` implies `--measure`; `--force` alone is a usage error.

## Exit codes

| Code | `exitReason` | When |
|---|---|---|
| 0 | `null` | The report was produced. Skipped pages say why in `measure`. |
| 1 | `usage` / `error` | Bad options; or an extension too old to list tabs (< 0.5.25). |
| 2 | `not_connected` | No extension relay is running (or the `--cdp` endpoint is unreachable), the connection failed, or the extension did not answer. |
| 3 | `held_by_other_session` | `--measure`/`--watch` without `--force`, nothing was measured, and **every** page of the profile is held by another chrome-use session whose daemon is running (`owner.kind == "session"` and `owner.live == true`). With no pages, or any page that is the user's, this session's, or a session that is not running, the exit is 0. A `--watch` stops after the first sample. |

Every exit, 2 included, prints the full object below (with `pages: []` when
nothing could be read). `success` is `exitCode == 0`.

## Top level

```jsonc
{
  "schema": 1,
  "command": "diag pages",
  "success": true,
  "exitCode": 0,                 // 0 | 1 | 2 | 3
  "exitReason": null,            // null | "usage" | "error" | "not_connected" | "held_by_other_session"
  "error": null,                 // string when exitCode != 0
  "generatedAt": 1760000000000,  // ms since the epoch
  "session": "default",          // the --session this ran as ("self" below)
  "options": { "measure": true, "force": false, "watchSeconds": null, "limit": 200 },
  "readOnly": true,              // false when anything was attached for measuring
  "connection": {
    "transport": "relay",        // "relay" | "cdp" | null (not resolved)
    "state": "connected",        // "connected" | "not_connected" | "error"
    "detail": "connected through default",
    "extensionConnected": true   // relay: the extension's port is up; cdp: null
  },
  "extension": {
    "applies": true,             // false on direct CDP
    "version": "0.5.35",         // live extension, null when unknown
    "bundled": "0.5.35",         // the build this CLI ships with
    "published": null,           // Web Store version, asked only when live < bundled (12 h cache)
    "verdict": "current",        // current | behind_published | newest_published | ahead_of_published
                                 // | behind_bundled_published_unknown | ahead_of_bundled | unknown | not_applicable
    "behindPublished": false,    // true only when a newer build can be installed today
    "hint": null,                // what to do: update / reload instruction (doctor's wording)
    "pendingUpdate": null        // { blocked, message, fix } when an update is downloaded and waiting
  },
  "profile": { "id": "…", "email": "…" },   // the relay's Chrome profile; nulls on CDP
  "staleRelayRecords": 0,        // relay records of pages that are not live (#519); null on CDP
  "relayRecords": { "records": 3, "live": 3, "stale": 0 },   // null on CDP
  "counts": {
    "pages": 12, "shown": 12, "omitted": 0, "windows": 3,
    "self": 1, "otherSessions": 2, "user": 9,              // of the shown pages
    "measured": 3, "skipped": 9, "failed": 0
  },
  "pages": [ /* page rows, below */ ],
  "omitted": { "pages": 0, "titlesCut": 0, "urlsCut": 0, "workerSites": 0, "note": null },
  "workers": { /* below */ },
  "forcedAttaches": [            // every attach done for measuring, and how it was undone
    { "handle": "chrome-tab:123", "targetId": "…", "attached": true,
      "release": "released",     // relay: "released" | "not released: <why>"; cdp: "detached" | "not detached: <why>"
      "activated": false }
  ],
  "watch": null,                 // object with --watch, below
  "notes": []                    // human-readable caveats (e.g. CDP has no active/visible)
}
```

## Page rows

Ordered: this session's pages, then other sessions' by name, then the user's;
each by window and tab index. At most `--limit` rows (default 200, max 1000);
pages past the limit are neither listed nor measured (`omitted.pages`).

```jsonc
{
  "handle": "chrome-tab:123",    // relay: chrome-tab:<Chrome tab id>; CDP: the targetId.
                                 // The same handle `tab list --all` and `--tab <handle> --force` take.
  "targetId": "ABCD…",           // null over the relay for a tab the extension never attached
  "chromeTabId": 123,            // null on CDP
  "windowId": 1,                 // null when unknown
  "index": 0,                    // position in its window (CDP: order in Chrome's list)
  "title": "…", "titleCut": false,   // cut at 300 characters
  "url": "…",   "urlCut": false,     // as Chrome reports it (may carry tokens), cut at 2048
  "active": true,                // active tab of its window; null on CDP
  "visible": true,               // active, window not minimized, not discarded; null when unknown
  "discarded": false,            // null on CDP
  "attached": false,             // relay: extension's debugger on it now; CDP: Chrome's `attached`
  "owner": { "kind": "user" },   // { "kind": "self" } | { "kind": "session", "session": "<name>|null", "live"?: bool } | { "kind": "user" }
  "ownerLabel": "the user",
  "rendererPid": null,           // always null in schema 1 (not cheaply available)
  "metrics": null,               // or { "JSHeapUsedSize", "JSHeapTotalSize", "Nodes", "JSEventListeners", "Documents" } (integers; bytes for heap)
  "metricsBefore": null,         // --watch: the first sample, same shape
  "measure": {
    "status": "not_requested",   // not_requested | measured | skipped | failed
    "reason": null,              // skipped/failed: needs_force | held_by_other_session | discarded
                                 //   | release_unsupported | not_attachable | timeout | error
    "detail": null,              // a sentence saying why
    "via": null,                 // measured: existing_attachment | temporary_attach | cdp_session
    "forced": false              // true when it took --force (not this session's page)
  },
  "delta": null,                 // --watch, both samples measured: below
  "leakClass": null,             // --watch: listeners_growing_nodes_flat | heap_growing | nodes_climbing | none
  "leakSignals": []              // every signal that fired: listeners_growing, nodes_flat, heap_growing, nodes_climbing
}
```

`owner.kind`: `self` is a page this session's ownership record names, or one
in this session's tab group; `session` another chrome-use session's (by its
ownership record, `live` = its daemon answers now; or by its tab group, with
no `live`; or an agent tab of an unknown session, `session: null`); `user`
everything else. Ownership is for reporting only.

## `--watch`

Two full reads `watchSeconds` apart; forced tabs are attached and released for
each read, so nothing stays attached in between. Rows are from the second read;
a page present in both and measured in both gets:

```jsonc
"delta": {
  "seconds": 60.012,             // between this page's two measurements
  "JSHeapUsedSize": 3145728, "JSHeapTotalSize": 0, "Nodes": 0,
  "JSEventListeners": 600, "Documents": 0,
  "heapBytesPerMinute": 3145571
}
```

The leak class is the first that applies:

1. `listeners_growing_nodes_flat`: JSEventListeners rose by at least
   max(20, 2 % of the first sample) **and** Nodes moved by at most
   max(100, 2 % of the first sample).
2. `heap_growing`: JSHeapUsedSize grew at 1.5 MiB (1572864 bytes) per minute
   or more.
3. `nodes_climbing`: Nodes rose by more than max(100, 2 % of the first sample).
4. `none`.

```jsonc
"watch": {
  "intervalSeconds": 60,
  "startedAt": 1760000000000,
  "leakClasses": { "heap_growing": 1, "none": 8 },
  "thresholds": { "heapGrowingBytesPerMinute": 1572864, "listenersGrowthMin": 20,
                  "listenersGrowthRel": 0.02, "nodesFlatAbs": 100, "nodesFlatRel": 0.02 }
}
```

## Workers

```jsonc
"workers": {
  "source": "relay_records",     // relay_records | cdp_targets
  "total": 4,                    // workers known (worker, shared_worker, service_worker)
  "measured": 2,                 // with a heap reading
  "measuredHeap": true,          // whether heaps were asked for at all this run
  "sites": [                     // per origin, most workers first, at most 50
    { "site": "https://app.example", "count": 2, "types": { "service_worker": 1, "worker": 1 },
      "measured": 2, "usedSize": 3145728, "totalSize": 4194304 }   // sums over measured workers; null when none measured
  ],
  "omittedSites": 0,
  "note": null
}
```

Over the relay only workers the extension already holds are known (children of
tabs a chrome-use session drives); their heaps are read only with `--force`.
On direct CDP a measured page's dedicated workers are found and measured on
its private session; other workers (shared, service) need `--force`. `workers`
is `null` only when the run could not connect.
