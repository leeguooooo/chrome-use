# Catalog scan review (2026-10-17)

Source revision: `f9ee93e2a15a4afd2af6a1967495686bdfa6fccf`. With plugin-scanner 3.0.123, the original local score was 61; after these changes it is 81. These are local preflight results. Listing still requires the catalog's own completed centralized score of at least 80.

## Changes and review

Actions are pinned to immutable SHAs (the Rust action uses upstream master history with an explicit stable toolchain input where applicable), a vulnerability reporting policy and Dependabot are added, and a read-only, offline source scan runs on pushes and pull requests using the catalog's exact action SHA and score threshold. No scanner suppressions or baseline are configured.

The nine HARDCODED_SECRET locations are synthetic redaction/leak-detection fixtures, a deliberately malformed password fixture, or a shell positional-argument expression. They are not embedded live credentials. The three RISKY_APPROVAL_DEFAULT findings are fixture captures, a design discussion and documentation mapping explicit permission choices; they are not a newly introduced default. Dynamic execution in the OpenCLI runner and vendored axe library remains visible for review; browser script execution is a product capability and must only be given trusted scripts. The postinstall/version-sync shell interpolation findings also remain visible for maintainer review, rather than being suppressed.

The OpenAI directory template now keeps its skill at the manifest-declared path. The assembled ZIP is checked independently; its README, license, security policy and packaging exclusions are included in the build. The old scanner asks for a Claude marketplace root strict field, which is not the supported per-plugin schema, so no invalid root field is added. Omitted optional Codex terms/screenshot fields remain informational.

## Reproduction

```sh
# In an isolated Python environment:
python -m pip install plugin-scanner==3.0.123
plugin-scanner scan . --format json --output /tmp/chrome-use-scan.json --min-score 80 --fail-on-severity none
```

## Remaining rule locations

| Rule | Severity | Location |
| --- | --- | --- |
| `HARDCODED_SECRET` | high | `cli/src/commands.rs:6360` |
| `HARDCODED_SECRET` | high | `cli/src/report.rs:1767` |
| `HARDCODED_SECRET` | high | `cli/src/agent_panel/config_transaction.rs:1131` |
| `HARDCODED_SECRET` | high | `cli/src/agent_panel/configuration.rs:438` |
| `HARDCODED_SECRET` | high | `cli/src/agent_panel/redact.rs:1899` |
| `HARDCODED_SECRET` | high | `cli/src/agent_panel/service.rs:4098` |
| `HARDCODED_SECRET` | high | `cli/src/agent_panel/inspect/replies.rs:591` |
| `HARDCODED_SECRET` | high | `cli/src/native/e2e_tests.rs:9487` |
| `HARDCODED_SECRET` | high | `scripts/cws-get-refresh-token.sh:17` |
| `DANGEROUS_DYNAMIC_EXECUTION` | high | `cli/src/opencli_runner.mjs` |
| `DANGEROUS_DYNAMIC_EXECUTION` | high | `cli/src/native/a11y/axe.min.js` |
| `SHELL_INJECTION_PATTERN` | high | `scripts/postinstall.js` |
| `SHELL_INJECTION_PATTERN` | high | `scripts/sync-version.js` |
| `CLAUDE_MARKETPLACE_STRICT_INVALID` | medium | `.claude-plugin/marketplace.json` |
| `RISKY_APPROVAL_DEFAULT` | medium | `cli/src/agent_panel/fixtures/README.md` |
| `RISKY_APPROVAL_DEFAULT` | medium | `docs/superpowers/plans/2026-10-15-verifiable-panel-turns.md` |
| `RISKY_APPROVAL_DEFAULT` | medium | `skill-data/core/references/connection.md` |
| `PLUGIN_JSON_INTERFACE_ASSET_TERMSOFSERVICEURL` | info | `packaging/openai/.codex-plugin/plugin.json` |
| `PLUGIN_JSON_INTERFACE_ASSET_SCREENSHOTS` | info | `packaging/openai/.codex-plugin/plugin.json` |
