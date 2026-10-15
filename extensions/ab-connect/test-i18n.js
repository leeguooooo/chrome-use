// Test-only chrome.i18n stub backed by the real _locales files. Importing this
// module installs English on globalThis.chrome; tests that replace
// globalThis.chrome add `i18n: i18nStub()` to their own stub.
import { readFileSync } from 'node:fs';

export function loadMessages(locale = 'en') {
  return JSON.parse(readFileSync(new URL(`./_locales/${locale}/messages.json`, import.meta.url), 'utf8'));
}

// Same substitution rules as chrome.i18n.getMessage: $NAME$ placeholders expand
// to their content, then $1..$9 take the substitutions and $$ is a literal $.
export function i18nStub(locale = 'en') {
  const messages = loadMessages(locale);
  return {
    getUILanguage: () => locale.replace('_', '-'),
    getMessage(key, substitutions = []) {
      const entry = messages[key];
      if (!entry) return '';
      const values = [].concat(substitutions ?? []).map(String);
      const placeholders = Object.fromEntries(Object.entries(entry.placeholders || {}).map(([name, value]) => [name.toLowerCase(), value.content]));
      const expanded = entry.message.replace(/\$([A-Za-z0-9_@]+)\$/g, (whole, name) => placeholders[name.toLowerCase()] ?? whole);
      return expanded.replace(/\$(\$|[1-9])/g, (whole, which) => which === '$' ? '$' : values[Number(which) - 1] ?? '');
    },
  };
}

export function installI18n(locale = 'en', target = globalThis) {
  target.chrome ??= {};
  target.chrome.i18n = i18nStub(locale);
  return target.chrome.i18n;
}

installI18n();
