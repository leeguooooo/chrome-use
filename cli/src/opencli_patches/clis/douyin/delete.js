import { cli, Strategy } from '@jackwener/opencli/registry';
import { ArgumentError, CommandExecutionError } from '@jackwener/opencli/errors';
import { browserFetch } from './_shared/browser-fetch.js';
import { requireObjectEvaluateResult } from './_shared/evaluate-result.js';
import { BIGINT_JSON_PAGE_SOURCE } from './_shared/bigint-json.js';

// chrome-use patch (leeguooooo/chrome-use#508) over @jackwener/opencli@1.8.8:
// - work_list is parsed with parseJsonKeepingBigInts, so item_id (a bare JSON
//   number past 2^53) stays an exact string and ids compare as strings;
// - the work card is found by its title (or id) instead of by position, which
//   needed a card for every work work_list returned: the page showed fewer,
//   and the run failed with card_not_found (#508). It scrolls to load more
//   cards, and when the work has a title only a card showing it is clicked.

const CREATOR_MANAGE_URL = 'https://creator.douyin.com/creator-micro/content/manage';
const WORK_LIST_URL = '/janus/douyin/creator/pc/work_list?status=0&count=20&max_cursor=0&scene=star_atlas&device_platform=android&aid=1128';

function readAwemeId(raw) {
    const value = String(raw ?? '').trim();
    if (!value) {
        throw new ArgumentError('douyin delete aweme_id cannot be empty');
    }
    if (!/^\d+$/.test(value)) {
        throw new ArgumentError('douyin delete aweme_id must be a numeric id');
    }
    return value;
}

function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

async function deleteViaCreatorManage(page, workId) {
    await page.goto(CREATOR_MANAGE_URL);
    await sleep(3000);
    await sleep(3000);
    const result = requireObjectEvaluateResult(await page.evaluate(`
    (async () => {
      ${BIGINT_JSON_PAGE_SOURCE}
      const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
      const idOf = (value) => (value == null ? '' : String(value));
      const targetId = ${JSON.stringify(String(workId))};
      const textOf = (node) => (node && (node.innerText || node.textContent) || '').trim();
      const normalize = (value) => String(value || '').replace(/\\s+/g, ' ').trim();

      async function loadTarget() {
        const res = await fetch(${JSON.stringify(WORK_LIST_URL)}, { credentials: 'include' });
        const payload = parseJsonKeepingBigInts(await res.text());
        const list = Array.isArray(payload.aweme_list) ? payload.aweme_list : [];
        const matches = list
          .map((entry, index) => ({ entry, index }))
          .filter(({ entry }) => idOf(entry.aweme_id) === targetId || idOf(entry.item_id) === targetId);
        if (matches.length === 0) {
          return { ok: false, reason: 'not_found', status_code: payload.status_code, count: list.length };
        }
        if (matches.length !== 1) {
          return { ok: false, reason: 'target_not_unique', count: matches.length };
        }
        const { entry, index } = matches[0];
        const item = { aweme_id: idOf(entry.aweme_id), item_id: idOf(entry.item_id) };
        const title = normalize(entry.desc || entry.caption || entry.title || entry.item_title || '');
        // What a work card shows: its title, or the first line of its text.
        const key = normalize(entry.item_title || String(entry.desc || entry.caption || entry.title || '').split('\\n')[0]).slice(0, 12);
        return { ok: true, item, index, listCount: list.length, title, key };
      }

      function visibleWorkCards() {
        const candidates = Array.from(document.querySelectorAll('[class*="video-card"]'))
          .filter((element) => {
            const text = normalize(textOf(element));
            return text.includes('删除作品') && text.includes('继续编辑');
          });
        return candidates.filter((candidate) => !candidates.some((other) => other !== candidate && other.contains(candidate)));
      }

      // The card for the target. With a title: the only card showing it; among
      // several, the one whose markup carries the id, else the one at the API
      // index. Without a title: the only card whose markup carries the id, else
      // the API index once every listed work is rendered (the original rule).
      function findCard(cards, target) {
        const ids = [target.item.aweme_id, target.item.item_id].filter(Boolean);
        const carriesId = (card) => ids.some((id) => String(card.outerHTML || '').includes(id));
        const atIndex = cards[target.index] || null;
        if (target.key) {
          const byTitle = cards.filter((card) => normalize(textOf(card)).includes(target.key));
          if (byTitle.length === 1) return byTitle[0];
          const byId = byTitle.filter(carriesId);
          if (byId.length === 1) return byId[0];
          return byTitle.includes(atIndex) ? atIndex : null;
        }
        const byId = cards.filter(carriesId);
        if (byId.length === 1) return byId[0];
        return cards.length >= target.listCount ? atIndex : null;
      }

      const target = await loadTarget();
      if (!target.ok) return target;

      const allTab = Array.from(document.querySelectorAll('button,[role="button"],span,div'))
        .find((element) => /^全部作品$/.test(normalize(textOf(element))));
      allTab?.click();
      await sleep(1000);
      for (let attempt = 0; attempt < 20; attempt += 1) {
        const cards = visibleWorkCards();
        const card = findCard(cards, target);
        if (card) {
          const deleteButton = Array.from(card.querySelectorAll('button,[role="button"],span,div'))
            .find((element) => /^删除作品$/.test(normalize(textOf(element))));
          if (!deleteButton) return { ok: false, reason: 'delete_button_not_found', aweme_id: target.item.aweme_id, item_id: target.item.item_id, index: target.index, cardCount: cards.length };
          deleteButton.click();
          await sleep(800);
          const confirmButton = Array.from(document.querySelectorAll('button,[role="button"]'))
            .find((element) => ['确定', '确认', '删除'].includes(normalize(textOf(element))));
          if (!confirmButton) return { ok: false, reason: 'confirm_button_not_found', aweme_id: target.item.aweme_id, item_id: target.item.item_id };
          confirmButton.click();
          for (let wait = 0; wait < 20; wait += 1) {
            await sleep(500);
            const after = await loadTarget();
            if (!after.ok && after.reason === 'not_found') {
              return { ok: true, aweme_id: target.item.aweme_id, item_id: target.item.item_id, title: target.title };
            }
          }
          return { ok: false, reason: 'delete_not_confirmed', aweme_id: target.item.aweme_id, item_id: target.item.item_id };
        }
        // Cards load as the list scrolls; bring the last one into view.
        cards[cards.length - 1]?.scrollIntoView?.({ block: 'end' });
        await sleep(500);
      }
      return { ok: false, reason: 'card_not_found', aweme_id: target.item.aweme_id, item_id: target.item.item_id, index: target.index, listCount: target.listCount, cardCount: visibleWorkCards().length };
    })()
  `), '抖音后台管理删除响应异常');

    if (!result?.ok) {
        throw new CommandExecutionError(`抖音后台管理删除失败: ${JSON.stringify(result)}`);
    }
    return result;
}

async function findWorkListItem(page, workId) {
    const data = await browserFetch(page, 'GET', `https://creator.douyin.com${WORK_LIST_URL}`, { timeoutMs: 8000 });
    const list = data.data?.work_list ?? data.aweme_list ?? data.work_list ?? [];
    if (!Array.isArray(list)) {
        throw new CommandExecutionError('抖音作品列表响应缺少 work_list/aweme_list');
    }
    return list.find((entry) => String(entry.aweme_id || '') === workId || String(entry.item_id || '') === workId) || null;
}

cli({
    site: 'douyin',
    name: 'delete',
    access: 'write',
    description: '删除作品（优先使用创作者后台作品管理；找不到时回退到旧删除接口）',
    domain: 'creator.douyin.com',
    strategy: Strategy.COOKIE,
    siteSession: 'persistent',
    args: [
        { name: 'aweme_id', required: true, positional: true, help: '作品 ID / item_id' },
    ],
    columns: ['status'],
    func: async (page, kwargs) => {
        const awemeId = readAwemeId(kwargs.aweme_id);
        try {
            const deleted = await deleteViaCreatorManage(page, awemeId);
            return [{ status: `✅ 已通过后台管理删除 ${deleted.aweme_id || awemeId}` }];
        } catch (fallbackError) {
            const fallbackMessage = fallbackError instanceof Error ? fallbackError.message : String(fallbackError);
            if (!fallbackMessage.includes('"reason":"not_found"')) {
                throw fallbackError;
            }
        }

        const before = await findWorkListItem(page, awemeId);
        if (!before) {
            throw new CommandExecutionError(`抖音作品 ${awemeId} 未在作品列表中找到，未执行删除`);
        }
        const url = 'https://creator.douyin.com/web/api/media/aweme/delete/?aid=1128';
        await browserFetch(page, 'POST', url, { body: { aweme_id: awemeId }, timeoutMs: 8000 });
        const deadline = Date.now() + 10_000;
        while (Date.now() < deadline) {
            await sleep(500);
            const after = await findWorkListItem(page, awemeId);
            if (!after) {
                return [{ status: `✅ 已删除 ${awemeId}` }];
            }
        }
        throw new CommandExecutionError(`抖音作品 ${awemeId} 删除后仍在作品列表中，删除未确认`);
    },
});
