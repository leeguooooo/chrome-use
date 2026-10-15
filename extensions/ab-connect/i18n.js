// User-visible text lives in _locales/<lang>/messages.json. chrome.i18n picks
// the file for Chrome's UI language and falls back to English ("en").
export function t(key, ...substitutions) {
  const i18n = globalThis.chrome?.i18n;
  return (i18n?.getMessage && i18n.getMessage(key, substitutions.map(String))) || key;
}

// A label table whose values are looked up when read, so modules can declare
// it at import time before chrome.i18n (or a test stub) is available.
export function messageMap(keys) {
  const map = {};
  for (const [name, key] of Object.entries(keys)) Object.defineProperty(map, name, { get: () => t(key), enumerable: true });
  return map;
}

const attributes = [['data-i18n', null], ['data-i18n-title', 'title'], ['data-i18n-placeholder', 'placeholder'], ['data-i18n-aria-label', 'aria-label']];

// Fill static page text from data-i18n* attributes: data-i18n sets the text
// content, the others set the named attribute. Without a message (a page
// opened outside the extension) the English written in the HTML stays.
export function applyI18n(root = globalThis.document) {
  const language = globalThis.chrome?.i18n?.getUILanguage?.();
  if (language && root?.documentElement) root.documentElement.lang = language;
  for (const [data, attribute] of attributes) {
    for (const element of root?.querySelectorAll?.(`[${data}]`) || []) {
      const text = globalThis.chrome?.i18n?.getMessage?.(element.getAttribute(data));
      if (!text) continue;
      if (attribute) element.setAttribute(attribute, text);
      else element.textContent = text;
    }
  }
}
