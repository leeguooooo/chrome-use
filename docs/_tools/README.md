# Maintaining search metadata

Run `python3 docs/_tools/seo.py` after adding or updating an HTML page. The script materializes canonical URLs, reciprocal language alternates and a small static navigation block; regenerates the sitemap from actual pages; and uses file history for last modification dates. CI runs `python3 docs/_tools/seo.py --check` to detect missing static metadata and sitemap URLs. Runtime navigation still enhances the same pages.

Keep current product facts in `docs/llms.txt`, README and both overview pages. Detector measurements must name their conditions; they are not a guarantee of website acceptance. Do not compare a competitor's default new context with all configurations it supports.

## Measuring results

Google Search Console: use the `leeguoo.com` domain property, filter page URLs containing `chrome-use`, and record the selected dates, clicks, impressions, CTR and position. This includes matching blog articles, excludes github.com, and is not an installation count. Export pages and queries to distinguish brand from non-brand traffic. Keep private traffic exports outside this public repository.

AI discovery: in a new unpersonalized conversation with web search enabled, use a non-brand prompt, save the answer and citations, and record whether chrome-use appears and its capabilities are stated correctly. A branded lookup is a different test. One answer is a sample, not a recommendation rate.

- Which tools let Codex automate my existing logged-in Chrome profile locally?
- Compare browser tools for multiple AI coding agents sharing one real Chrome.
- 怎么让 Claude Code 使用我已经登录的 Chrome？
- 多个 agent 怎么共用浏览器而不点错标签页？

Review weekly and after changing connection setup or ownership. Use the existing analytics for landing-page referrers and download-link clicks. AI crawler visits, AI citations, AI referrals and verified installs are separate measurements; unavailable values stay null.
