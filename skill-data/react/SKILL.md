---
name: react
description: Inspect React apps and measure Web Vitals with chrome-use's built-in `react …` and `vitals` commands (component tree, props/hooks/state, re-render profiling, Suspense, LCP/CLS/INP, SPA pushstate). Requires launching with --enable react-devtools. Use when debugging a React app's renders/state or measuring page performance.
allowed-tools: Bash(chrome-use:*), Bash(chrome-use:*), Bash(abs:*), Bash(npx chrome-use:*), Bash(npx chrome-use:*)
---

# chrome-use — React / Web Vitals

Inspect React apps and measure Web Vitals with chrome-use's built-in `react …` and `vitals` commands.

## React / Web Vitals (built-in, any React app)

chrome-use ships with first-class React introspection. Works on any
React app — Next.js, Remix, Vite+React, CRA, TanStack Start, React Native
Web, etc. The `react …` commands require the React DevTools hook to be
installed at launch via `--enable react-devtools`:

```bash
chrome-use open --enable react-devtools http://localhost:3000
chrome-use react tree                         # component tree
chrome-use react inspect <fiberId>            # props, hooks, state, source
chrome-use react renders start                # begin re-render recording
chrome-use react renders stop                 # print render profile
chrome-use react suspense [--only-dynamic]    # Suspense boundaries + classifier
chrome-use vitals [url]                       # LCP/CLS/TTFB/FCP/INP + hydration
chrome-use pushstate <url>                    # SPA navigation (auto-detects Next router)
```

Without `--enable react-devtools`, the `react …` commands error. `vitals`
and `pushstate` work on any site regardless of framework.
