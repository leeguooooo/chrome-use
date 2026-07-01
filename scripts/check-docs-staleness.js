#!/usr/bin/env node

/**
 * Reminds the committer (human OR agent) to update the docs site
 * (chrome-use.leeguoo.com — the /*.html + /en/*.html pages) when a change
 * touches a file the docs are GENERATED FROM but leaves the matching doc page
 * unstaged.
 *
 * The docs pages are hand-derived from skill-data/** , the command reference,
 * and README.md — so those sources drifting out of sync with the rendered
 * pages is the failure mode this guards against.
 *
 * Behaviour: WARN and continue (exit 0) — same spirit as the pre-push hook.
 *   SKIP_DOCS_CHECK=1     -> skip entirely
 *   DOCS_CHECK_STRICT=1   -> block the commit (exit 1) instead of warning
 *   DOCS_CHECK_DIFF=<rng> -> diff a git range (e.g. "origin/main...HEAD")
 *                           instead of the staged index — used by CI on PRs
 *   DOCS_CHECK_GHA=1      -> emit GitHub Actions ::warning:: annotations
 *                           (non-blocking) instead of the boxed reminder
 *
 * Wired from .husky/pre-commit (local) and .github/workflows/ci.yml (PR net).
 */

import { execSync } from 'child_process';
import { existsSync } from 'fs';

if (process.env.SKIP_DOCS_CHECK === '1') process.exit(0);

// source path (matched against repo-root-relative staged paths) -> doc page slugs.
// slug 'overview' => index.html / en/index.html ; every other slug => <slug>.html / en/<slug>.html
const MAP = [
  { test: /^skill-data\/core\/references\/commands\.md$/,            pages: ['commands'] },
  { test: /^skill-data\/core\/references\/authentication\.md$/,      pages: ['login-auth'] },
  { test: /^skill-data\/core\/references\/session-management\.md$/,  pages: ['sessions'] },
  { test: /^skill-data\/core\/references\/video-recording\.md$/,     pages: ['media'] },
  { test: /^skill-data\/core\/references\/trust-boundaries\.md$/,    pages: ['troubleshooting'] },
  { test: /^skill-data\/core\/references\/(proxy-support|profiling)\.md$/, pages: ['commands'] },
  // the core skill feeds most of the guide pages
  { test: /^skill-data\/core\/SKILL\.md$/, pages: [
      'core-loop', 'reading', 'interacting', 'finding', 'waiting', 'extract',
      'login-auth', 'sessions', 'real-chrome', 'stealth', 'network', 'react', 'media', 'troubleshooting',
      'extract', 'site-adapters',
    ] },
  { test: /^skill-data\/canvas\//,         pages: ['canvas'] },
  { test: /^skill-data\/electron\//,       pages: ['electron'] },
  { test: /^skill-data\/slack\//,          pages: ['slack'] },
  { test: /^skill-data\/(test|dogfood)\//, pages: ['testing'] },
  { test: /^skill-data\/(vercel-sandbox|agentcore)\//, pages: ['cloud'] },
  { test: /^README\.md$/,                  pages: ['install', 'family', 'overview'] },
];

const pageFiles = (slug) =>
  slug === 'overview' ? ['index.html', 'en/index.html'] : [`${slug}.html`, `en/${slug}.html`];

const range = process.env.DOCS_CHECK_DIFF;
const diffCmd = range
  ? `git diff --name-only --diff-filter=ACMR ${range}`
  : 'git diff --cached --name-only --diff-filter=ACMR';
let staged;
try {
  staged = execSync(diffCmd, { encoding: 'utf8' })
    .split('\n').map((s) => s.trim()).filter(Boolean);
} catch {
  process.exit(0); // not a git context / nothing to diff — don't get in the way
}
const stagedSet = new Set(staged);

// collect (source -> stale pages) where the source is staged but its page(s) are not
const findings = [];
for (const { test, pages } of MAP) {
  const hitSources = staged.filter((f) => test.test(f));
  if (hitSources.length === 0) continue;
  const stalePages = pages.filter((slug) => {
    const files = pageFiles(slug).filter((p) => existsSync(p)); // only pages that exist
    if (files.length === 0) return false;
    return !files.some((p) => stagedSet.has(p)); // stale if NONE of zh/en are staged
  });
  if (stalePages.length) findings.push({ sources: hitSources, pages: stalePages });
}

if (findings.length === 0) process.exit(0);

const stalePageSlugs = [...new Set(findings.flatMap((f) => f.pages))];

// CI mode: emit non-blocking GitHub Actions annotations and stop.
if (process.env.DOCS_CHECK_GHA === '1') {
  for (const f of findings) {
    const pages = f.pages.map((s) => (s === 'overview' ? 'index.html' : `${s}.html`)).join(', ');
    for (const src of f.sources) {
      console.log(
        `::warning file=${src}::${src} changed but its docs page(s) were not — update ${pages} (and the /en/ mirror), or this may be intentional.`
      );
    }
  }
  process.exit(0);
}
const y = (s) => `\x1b[33m${s}\x1b[0m`;
const b = (s) => `\x1b[1m${s}\x1b[0m`;

console.error('');
console.error(y('┌─ 📖 docs reminder ─────────────────────────────────────────'));
console.error(y('│') + ` You staged doc SOURCES but not the docs pages they generate.`);
console.error(y('│') + ` chrome-use.leeguoo.com pages are derived from these files —`);
console.error(y('│') + ` update the matching page(s) ${b('and their /en/ mirror')} so the site`);
console.error(y('│') + ` doesn't go stale:`);
console.error(y('│'));
for (const f of findings) {
  console.error(y('│') + `   ${f.sources.join(', ')}`);
  console.error(y('│') + `     → ${f.pages.map((s) => (s === 'overview' ? 'index.html' : `${s}.html`)).join(', ')} ${b('(+ /en/…)')}`);
}
console.error(y('│'));
console.error(y('│') + ` If this commit genuinely needs no doc change: ${b('SKIP_DOCS_CHECK=1 git commit …')}`);
console.error(y('│') + ` To regenerate at scale, re-run the docs workflow for: ${stalePageSlugs.join(', ')}`);
console.error(y('└────────────────────────────────────────────────────────────'));
console.error('');

if (process.env.DOCS_CHECK_STRICT === '1') {
  console.error('DOCS_CHECK_STRICT=1 — blocking commit. Update the docs or set SKIP_DOCS_CHECK=1.');
  process.exit(1);
}
process.exit(0);
