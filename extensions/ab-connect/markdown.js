// Render agent Markdown as DOM nodes. Agent text is untrusted page-derived
// data, so nothing is parsed as HTML: every node is created explicitly, text
// goes through textContent, and links are limited to http(s) URLs.

const INLINE = /(`+)([\s\S]*?[^`])\1(?!`)|\*\*([^*]+)\*\*|(?<![A-Za-z0-9])__([^_]+)__(?![A-Za-z0-9])|\*([^*\s][^*]*)\*|(?<![A-Za-z0-9])_([^_\s][^_]*)_(?![A-Za-z0-9])|~~([^~]+)~~|\[([^\]]+)\]\(([^)\s]+)\)|(https?:\/\/[^\s<>()]+[^\s<>().,;:!?'"])/g;

function safeHref(url) {
  try {
    const parsed = new URL(url);
    return parsed.protocol === 'http:' || parsed.protocol === 'https:' ? parsed.href : null;
  } catch { return null; }
}

function inline(document, parent, text) {
  let last = 0;
  for (const match of text.matchAll(INLINE)) {
    if (match.index > last) parent.append(document.createTextNode(text.slice(last, match.index)));
    const [whole, , code, bold1, bold2, em1, em2, strike, label, url, bare] = match;
    let node;
    if (code !== undefined) { node = document.createElement('code'); node.textContent = code; }
    else if (bold1 !== undefined || bold2 !== undefined) { node = document.createElement('strong'); inline(document, node, bold1 ?? bold2); }
    else if (em1 !== undefined || em2 !== undefined) { node = document.createElement('em'); inline(document, node, em1 ?? em2); }
    else if (strike !== undefined) { node = document.createElement('s'); inline(document, node, strike); }
    else {
      const href = safeHref(url ?? bare);
      if (href) {
        node = document.createElement('a'); node.setAttribute('href', href); node.setAttribute('target', '_blank'); node.setAttribute('rel', 'noopener noreferrer');
        if (label !== undefined) inline(document, node, label); else node.textContent = bare;
      } else node = document.createTextNode(whole);
    }
    parent.append(node);
    last = match.index + whole.length;
  }
  if (last < text.length) parent.append(document.createTextNode(text.slice(last)));
}

const isFence = line => /^\s*(```|~~~)/.test(line);
const listItem = line => line.match(/^(\s*)([-*+]|\d+[.)])\s+(.*)$/);
const isTableRow = line => /^\s*\|.*\|\s*$/.test(line);
const isTableRule = line => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line);
const cells = line => line.trim().replace(/^\||\|$/g, '').split('|').map(cell => cell.trim());

// Block structure: fenced code, headings, rules, quotes, lists, pipe tables
// and paragraphs. Lines inside a paragraph are joined with line breaks, which
// is how agents write short multi-line answers.
export function renderMarkdown(document, container, source) {
  container.replaceChildren();
  const lines = String(source ?? '').replace(/\r\n?/g, '\n').split('\n');
  let index = 0;
  const el = (tag, className) => { const node = document.createElement(tag); if (className) node.className = className; return node; };
  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) { index++; continue; }
    if (isFence(line)) {
      const fence = line.trim().slice(0, 3), language = line.trim().slice(3).trim();
      const body = []; index++;
      while (index < lines.length && !lines[index].trim().startsWith(fence)) body.push(lines[index++]);
      index++;
      const pre = el('pre', 'md-code'), code = el('code'); code.textContent = body.join('\n');
      if (language) pre.setAttribute('data-language', language.slice(0, 32));
      pre.append(code); container.append(pre); continue;
    }
    const heading = line.match(/^(#{1,6})\s+(.*?)\s*#*\s*$/);
    if (heading) { const node = el(`h${Math.min(heading[1].length + 2, 6)}`, 'md-heading'); inline(document, node, heading[2]); container.append(node); index++; continue; }
    if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { container.append(el('hr')); index++; continue; }
    if (/^\s*>/.test(line)) {
      const body = [];
      while (index < lines.length && /^\s*>/.test(lines[index])) body.push(lines[index++].replace(/^\s*>\s?/, ''));
      const quote = el('blockquote'); renderMarkdown(document, quote, body.join('\n')); container.append(quote); continue;
    }
    if (isTableRow(line) && index + 1 < lines.length && isTableRule(lines[index + 1])) {
      const table = el('table'), head = el('thead'), headRow = el('tr');
      for (const cell of cells(line)) { const th = el('th'); inline(document, th, cell); headRow.append(th); }
      head.append(headRow); table.append(head);
      const body = el('tbody'); index += 2;
      while (index < lines.length && isTableRow(lines[index])) {
        const row = el('tr');
        for (const cell of cells(lines[index++])) { const td = el('td'); inline(document, td, cell); row.append(td); }
        body.append(row);
      }
      table.append(body);
      const wrap = el('div', 'md-table'); wrap.append(table); container.append(wrap); continue;
    }
    if (listItem(line)) {
      const ordered = /\d/.test(listItem(line)[2]);
      const list = el(ordered ? 'ol' : 'ul');
      if (ordered) { const start = parseInt(listItem(line)[2], 10); if (start > 1) list.setAttribute('start', String(start)); }
      const baseIndent = listItem(line)[1].length;
      let item = null;
      while (index < lines.length) {
        const current = lines[index], match = listItem(current);
        if (match && match[1].length <= baseIndent + 1 && /\d/.test(match[2]) === ordered) {
          item = el('li'); inline(document, item, match[3]); list.append(item); index++;
        } else if (item && current.trim() && (/^\s{2,}/.test(current))) {
          // Indented continuation or a nested list belongs to the current item.
          const nested = [];
          while (index < lines.length && lines[index].trim() && /^\s{2,}/.test(lines[index])) nested.push(lines[index++].replace(/^\s{2,4}/, ''));
          const sub = el('div'); renderMarkdown(document, sub, nested.join('\n')); item.append(...sub.children ?? []);
        } else break;
      }
      container.append(list); continue;
    }
    const paragraph = el('p'), body = [];
    while (index < lines.length && lines[index].trim() && !isFence(lines[index]) && !/^(#{1,6})\s/.test(lines[index]) && !listItem(lines[index]) && !/^\s*>/.test(lines[index]) && !(isTableRow(lines[index]) && isTableRule(lines[index + 1] || ''))) body.push(lines[index++]);
    body.forEach((text, n) => { if (n) paragraph.append(el('br')); inline(document, paragraph, text); });
    container.append(paragraph);
  }
}
