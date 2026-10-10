// Tests for chrome-use's patches to OpenCLI's douyin adapters (opencli.rs
// PATCHES, chrome-use#508). Run: node --test cli/src/opencli_patches/douyin.test.mjs
//
// The id helpers need nothing else. The adapter test runs the patched
// douyin/delete through OpenCLI's own registry, so it needs the pinned package:
//   npm install --prefix <dir> @jackwener/opencli@1.8.8 --ignore-scripts
//   OPENCLI_PKG_DIR=<dir>/node_modules/@jackwener/opencli node --test ...
// The patches are copied over that install, as chrome-use does. Nothing here
// talks to Douyin: the page, its fetch and its DOM are stubs fed a fixture.
import assert from 'node:assert/strict';
import { copyFileSync, mkdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';
import vm from 'node:vm';

import {
    BIGINT_JSON_PAGE_SOURCE,
    parseJsonKeepingBigInts,
    quoteUnsafeIntegers,
} from './clis/douyin/_shared/bigint-json.js';

const here = dirname(fileURLToPath(import.meta.url));
const FIXTURE_TEXT = readFileSync(join(here, 'fixtures/douyin_work_list_bigint.json'), 'utf8');
const TARGET = '7694857245896576275';

test('a 19-digit id round-trips exactly; plain JSON.parse rounds it', () => {
    const text = `{"item_id":${TARGET}}`;
    assert.equal(String(JSON.parse(text).item_id), '7694857245896576000');
    const parsed = parseJsonKeepingBigInts(text);
    assert.equal(parsed.item_id, TARGET);
    assert.equal(JSON.stringify(parsed), `{"item_id":"${TARGET}"}`);
    assert.equal(parseJsonKeepingBigInts(JSON.stringify(parsed)).item_id, TARGET);
});

test('only unsafe integers are quoted', () => {
    const text = '{"a":9007199254740991,"b":9007199254740993,"c":-9223372036854775808,'
        + '"d":1.5e300,"e":12345678901234567890.5,"f":"x 7694857245896576275 \\"9999999999999999999\\"",'
        + '"g":[0,-1,18446744073709551615],"h":true,"i":null}';
    const v = parseJsonKeepingBigInts(text);
    assert.equal(v.a, 9007199254740991);
    assert.equal(v.b, '9007199254740993');
    assert.equal(v.c, '-9223372036854775808');
    assert.equal(v.d, 1.5e300);
    assert.equal(typeof v.e, 'number');
    assert.equal(v.f, 'x 7694857245896576275 "9999999999999999999"');
    assert.deepEqual(v.g, [0, -1, '18446744073709551615']);
    assert.equal(v.h, true);
    assert.equal(v.i, null);
    // Text without unsafe integers comes back unchanged.
    const safe = '{"n":1767000348000,"s":"\\\\"}';
    assert.equal(quoteUnsafeIntegers(safe), safe);
});

test('the fixture work_list keeps every item_id exact, also as page source', () => {
    const fromModule = parseJsonKeepingBigInts(FIXTURE_TEXT);
    const inPage = vm.runInNewContext(`${BIGINT_JSON_PAGE_SOURCE}\nparseJsonKeepingBigInts(text)`, {
        text: FIXTURE_TEXT,
    });
    // inPage comes from another realm; compare plain copies.
    for (const payload of [fromModule, JSON.parse(JSON.stringify(inPage))]) {
        const ids = payload.aweme_list.map((w) => [w.aweme_id, w.item_id]);
        assert.deepEqual(ids, [
            ['7694857245896576001', '7694857245896576001'],
            [TARGET, TARGET],
            ['7694857245896576999', '7694857245896576999'],
        ]);
        assert.equal(payload.max_cursor, 1767000348000);
        assert.equal(payload.aweme_list[2].desc, '第三条作品，引号 "7694857245896576275" 在字符串里');
    }
});

// ---- adapter test against the pinned OpenCLI package ----

const PKG = process.env.OPENCLI_PKG_DIR;
const skip = PKG ? false : 'set OPENCLI_PKG_DIR to an install of @jackwener/opencli@1.8.8';

function overlayPatches(pkg) {
    for (const rel of ['douyin/_shared/bigint-json.js', 'douyin/_shared/browser-fetch.js', 'douyin/delete.js']) {
        const to = join(pkg, 'clis', rel);
        mkdirSync(dirname(to), { recursive: true });
        copyFileSync(join(here, 'clis', rel), to);
    }
}

// A creator.douyin.com content/manage page: the works' cards (the first
// `rendered` of them, more after scrolling), 删除作品 on each, and a confirm
// dialog. work_list answers with the fixture until the deleted work is gone.
function fakeManagePage({ rendered, cardText = (w) => w.desc.split('\n')[0] }) {
    const fixture = parseJsonKeepingBigInts(FIXTURE_TEXT);
    const works = fixture.aweme_list;
    const deleted = [];
    let shown = rendered;
    let pendingDelete = null;
    const el = (text, extra = {}) => ({
        innerText: text, textContent: text, outerHTML: `<div>${text}</div>`,
        querySelectorAll: () => [], contains: () => false, click() {}, ...extra,
    });
    const cardFor = (w) => {
        const del = el('删除作品', { click() { pendingDelete = w.aweme_id; } });
        const buttons = [del, el('继续编辑')];
        const text = `${cardText(w)} 删除作品 继续编辑`;
        return el(text, {
            className: 'video-card',
            querySelectorAll: () => buttons,
            contains: (o) => buttons.includes(o),
            scrollIntoView() { shown = works.length; },
        });
    };
    const cards = works.map(cardFor);
    const confirm = el('确定', {
        click() {
            if (pendingDelete) deleted.push(pendingDelete);
            pendingDelete = null;
        },
    });
    const document = {
        querySelectorAll(sel) {
            const live = cards.filter((_, i) => i < shown && !deleted.includes(works[i].aweme_id));
            if (sel === '[class*="video-card"]') return live;
            if (sel === 'button,[role="button"]') return pendingDelete ? [confirm] : [];
            return [el('全部作品'), ...live.flatMap((c) => c.querySelectorAll())];
        },
    };
    // Served as Douyin sends it: item_id as a bare 19-digit number.
    const fetch = async () => {
        const body = { ...fixture, aweme_list: works.filter((w) => !deleted.includes(w.aweme_id)) };
        const text = JSON.stringify(body).replace(/"item_id":"(\d+)"/g, '"item_id":$1');
        return { ok: true, status: 200, text: async () => text, json: async () => JSON.parse(text) };
    };
    const page = {
        async goto() {},
        async evaluate(src) {
            const ctx = vm.createContext({ fetch, document, setTimeout: (fn) => setImmediate(fn) });
            return vm.runInContext(src, ctx);
        },
    };
    return { page, deleted };
}

async function loadDelete() {
    overlayPatches(PKG);
    // The adapter's own waits (3 s twice) are real timers; skip them.
    globalThis.setTimeout = ((orig) => (fn, ms, ...a) => orig(fn, 0, ...a))(globalThis.setTimeout);
    const { getRegistry } = await import(pathToFileURL(join(PKG, 'dist/src/registry-api.js')).href);
    await import(pathToFileURL(join(PKG, 'clis/douyin/delete.js')).href);
    return getRegistry().get('douyin/delete');
}

test('douyin/delete finds the work by title when fewer cards render than work_list lists', { skip }, async () => {
    const cmd = await loadDelete();
    // One card at first, the rest after scrolling: the old positional lookup
    // needed all three and failed with card_not_found (#508).
    const { page, deleted } = fakeManagePage({ rendered: 1 });
    const rows = await cmd.func(page, { aweme_id: TARGET });
    assert.deepEqual(deleted, [TARGET]);
    assert.deepEqual(rows, [{ status: `✅ 已通过后台管理删除 ${TARGET}` }]);
});

test('douyin/delete reports exact string ids and clicks nothing when no card shows the title', { skip }, async () => {
    const cmd = await loadDelete();
    const { page, deleted } = fakeManagePage({ rendered: 3, cardText: () => '别的作品' });
    await assert.rejects(cmd.func(page, { aweme_id: TARGET }), (err) => {
        const json = JSON.parse(err.message.slice(err.message.indexOf('{')));
        assert.equal(json.reason, 'card_not_found');
        assert.equal(json.aweme_id, TARGET);
        assert.equal(json.item_id, TARGET);
        assert.equal(json.listCount, 3);
        return true;
    });
    assert.deepEqual(deleted, []);
});
