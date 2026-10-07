#!/usr/bin/env python3
"""Materialize crawler-visible metadata and sitemap. Run from any directory."""
from pathlib import Path
import datetime, html, re, subprocess, sys
import xml.etree.ElementTree as ET
ROOT = Path(__file__).resolve().parents[2]
DOCS = ROOT / 'docs'
ORIGIN = 'https://chrome-use.leeguoo.com'
NS = 'http://www.sitemaps.org/schemas/sitemap/0.9'
ALT = 'http://www.w3.org/1999/xhtml'
ET.register_namespace('', NS)
ET.register_namespace('xhtml', ALT)
check = '--check' in sys.argv
changes = []
urls = ET.Element(f'{{{NS}}}urlset')
seen = set()
pages = sorted([*DOCS.glob('*.html'), *DOCS.glob('en/*.html')])
for p in pages:
    rel = p.relative_to(DOCS).as_posix()
    slug = p.stem
    en = rel.startswith('en/')
    route = ('/en/' if en else '/') + ('' if slug == 'index' else slug + '.html')
    canonical = ORIGIN + route
    mirror = DOCS / (slug + '.html') if en else DOCS / 'en' / (slug + '.html')
    source = p.read_text(encoding='utf-8')
    # Some legacy root pages are English aliases rather than Chinese translations.
    actual_en = bool(re.search(r'<html[^>]+lang=["\']en', source))
    zh_page = DOCS / (slug + '.html')
    has_zh = zh_page.exists() and not re.search(r'<html[^>]+lang=["\']en', zh_page.read_text(encoding='utf-8'))
    if actual_en and not en and mirror.exists():
        canonical = ORIGIN + '/en/' + ('' if slug == 'index' else slug + '.html')
    alts = [('zh-Hans', '/'), ('en', '/en/'), ('x-default', '/')] if has_zh else [('en', '/en/')]
    # Retain any page-specific robots policy.
    clean = re.sub(r'\s*<link\b[^>]*\brel=["\'](?:canonical|alternate)["\'][^>]*>', '', source)
    tags = [f'<link rel="canonical" href="{canonical}" />']
    if mirror.exists():
        for lang, prefix in alts:
            href = ORIGIN + prefix + ('' if slug == 'index' else slug + '.html')
            tags.append(f'<link rel="alternate" hreflang="{lang}" href="{href}" />')
    result = clean.replace('</head>', '\n  ' + '\n  '.join(tags) + '\n</head>')
    # Internal links must be visible even to a crawler that does not execute JS.
    static = '<nav aria-label="'+('Documentation' if actual_en else '文档')+'"><a href="'+('/en/' if actual_en else '/')+'">chrome-use</a> · <a href="'+('/en/' if actual_en else '/')+'install.html">'+('Install' if actual_en else '安装')+'</a> · <a href="'+('/en/' if actual_en else '/')+'logged-in-browser.html">'+('Logged-in browser guide' if actual_en else '已登录浏览器指南')+'</a> · <a href="'+('/en/' if actual_en else '/')+'mcp.html">MCP</a> · <a href="'+('/en/' if actual_en else '/')+'compare.html">'+('Compare' if actual_en else '选型')+'</a></nav>'
    result = re.sub(r'<nav aria-label="(?:Documentation|文档)">.*?</nav>', '', result)
    result = re.sub(r'(<main\b[^>]*>)', lambda m: m[0]+static, result, count=1)
    if result != source:
        changes.append(rel)
        if not check: p.write_text(result, encoding="utf-8")
    if canonical in seen: continue
    seen.add(canonical)
    item = ET.SubElement(urls, f'{{{NS}}}url')
    ET.SubElement(item, f'{{{NS}}}loc').text = canonical
    if mirror.exists():
        for lang, prefix in alts:
            ET.SubElement(item, f'{{{ALT}}}link', {'rel':'alternate', 'hreflang':lang, 'href':ORIGIN+prefix+('' if slug=='index' else slug+'.html')})
    # Use content history, not a blanket "today" date for every old page.
    dirty = subprocess.check_output(['git','status','--porcelain','--',str(p)], cwd=ROOT, text=True).strip()
    date = datetime.date.today().isoformat() if dirty or result != source else subprocess.check_output(['git','log','-1','--format=%cs','--',str(p)], cwd=ROOT, text=True).strip()
    if date: ET.SubElement(item, f'{{{NS}}}lastmod').text = date
ET.indent(urls, space='  ')
sitemap = '<?xml version="1.0" encoding="UTF-8"?>\n' + ET.tostring(urls, encoding='unicode') + '\n'
if check:
    # lastmod may change after commits: validate the URL set rather than demand churn.
    existing = ET.parse(DOCS/'sitemap.xml')
    if set(x.text for x in existing.findall('.//{*}loc')) != set(x.text for x in urls.findall('.//{*}loc')): changes.append('sitemap URL coverage')
    def alternates(tree):
        return {entry.find('{*}loc').text: {(link.get('hreflang'), link.get('href')) for link in entry.findall('{*}link')} for entry in tree.findall('.//{*}url')}
    if alternates(existing) != alternates(urls): changes.append('sitemap language alternates')
    if changes: raise SystemExit('Stale SEO metadata: '+', '.join(changes))
else:
    (DOCS/'sitemap.xml').write_text(sitemap, encoding='utf-8')
print(f'SEO metadata verified for {len(pages)} pages' if check else f'Materialized SEO metadata for {len(pages)} pages')
