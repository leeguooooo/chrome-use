# chrome-use 技能分发 v2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `chrome-use skill install` 成为跨-runner 的统一技能安装入口（委托 skills.sh，不自建目录映射），install.sh 以 doctor 自检 + 首条 agent prompt 收尾，doctor 新增只读技能探测，stub 路由表补全 12 个场景。

**Architecture:** 二进制仍是唯一事实源。新命令 `skill install` 是 `npx skills add leeguooooo/chrome-use` 的薄包装器；纯逻辑（argv 构建、目录探测）抽成可单测函数，副作用（spawn npx、进程退出）留薄壳。doctor 加一个只读 check 模块。install.sh 把 23 行 npx 分支收敛成一句 `chrome-use skill install`。

**Tech Stack:** Rust（cli/，`cargo test`）、POSIX sh（install.sh，`sh -n`）、skills.sh（`npx skills@latest`）。i18n 走既有 `crate::connect::ui_zh()`。

参考规格：`docs/superpowers/specs/2026-07-08-skill-distribution-v2-design.md`

---

## File Structure

| 文件 | 责任 | 动作 |
|---|---|---|
| `cli/src/skills.rs` | 新增 `install` 子命令：argv 构建（纯函数）+ spawn 薄壳 | Modify |
| `cli/src/main.rs:1157` | 顶层 dispatch 让 `skill` 成为 `skills` 的别名 | Modify |
| `cli/src/doctor/skill.rs` | 只读探测 agent 技能目录的 doctor check | Create |
| `cli/src/doctor/mod.rs:11-22,105` | 注册 `mod skill;` + 调用 `skill::check` | Modify |
| `skills/chrome-use/SKILL.md:36-48` | Specialized skills 列表 → 场景路由表（12 项） | Modify |
| `install.sh:114-136` | 23 行 npx 分支 → 一句 `skill install`；末尾加 doctor 自检 + 首条 prompt | Modify |

**关键设计约束（来自 spec，实现时不得违反）：**
- `skill install` 只 spawn `npx`，**绝不自己往任何 runner 目录写文件**。
- doctor 的技能 check **只读探测**，状态只能是 Pass / Warn / Info，**永不 Fail**。
- 路由表只放「名字 + 场景」（版本稳定），正文仍由 `skills get` 释出。

---

## Task 1: `skill install` 的 argv 构建（纯函数 + TDD）

**Files:**
- Modify: `cli/src/skills.rs`（在文件末尾 `#[cfg(test)] mod tests` 之前加 `build_skill_install_argv`；测试加进已存在的 `mod tests`）

- [ ] **Step 1: 写失败测试**

在 `cli/src/skills.rs` 的 `#[cfg(test)] mod tests { … }` 内追加：

```rust
    #[test]
    fn skill_install_argv_defaults_to_global() {
        let argv = build_skill_install_argv(false);
        assert_eq!(
            argv,
            vec![
                "-y".to_string(),
                "skills@latest".to_string(),
                "add".to_string(),
                "leeguooooo/chrome-use".to_string(),
                "-g".to_string(),
            ]
        );
    }

    #[test]
    fn skill_install_argv_project_drops_global() {
        let argv = build_skill_install_argv(true);
        assert!(!argv.contains(&"-g".to_string()));
        assert_eq!(argv.last().unwrap(), "leeguooooo/chrome-use");
    }
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test -p chrome-use --manifest-path cli/Cargo.toml skill_install_argv 2>&1 | tail -20`
Expected: 编译失败 `cannot find function build_skill_install_argv`

（注：crate 名以 `cli/Cargo.toml` 的 `[package] name` 为准；若不是 `chrome-use` 则去掉 `-p` 直接 `cargo test --manifest-path cli/Cargo.toml skill_install_argv`。先跑 `grep '^name' cli/Cargo.toml` 确认。）

- [ ] **Step 3: 写最小实现**

在 `cli/src/skills.rs` 里 `run_skills` 上方（或就近）加：

```rust
/// Build the argv passed to `npx` for `chrome-use skill install`.
/// Delegates to skills.sh — we never write runner dirs ourselves.
/// Global (`-g`) by default; `--project` installs into the current project.
fn build_skill_install_argv(project: bool) -> Vec<String> {
    let mut v = vec![
        "-y".to_string(),
        "skills@latest".to_string(),
        "add".to_string(),
        "leeguooooo/chrome-use".to_string(),
    ];
    if !project {
        v.push("-g".to_string());
    }
    v
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --manifest-path cli/Cargo.toml skill_install_argv 2>&1 | tail -20`
Expected: `test result: ok. 2 passed`

- [ ] **Step 5: 提交**

```bash
git add cli/src/skills.rs
git commit -m "feat(skills): skill install argv 构建（-g 默认，--project 去掉）"
```

---

## Task 2: `skill install` 的 spawn 薄壳 + 命令路由

**Files:**
- Modify: `cli/src/skills.rs`（`run_skills` 的 `match subcommand` 加 `Some("install")`；新增 `run_skill_install`）
- Modify: `cli/src/main.rs:1157`（`skill` 别名）

> 说明：`run_skill_install` 会 spawn 外部进程并 `exit()`，不做单元测试（副作用壳）。可测的逻辑已在 Task 1 抽走。本任务的验证靠编译 + 手动跑。

- [ ] **Step 1: 在 `run_skills` 的 match 里接住 `install`**

`cli/src/skills.rs` 的 `match subcommand { … }`，在 `Some("path") => …` 之后加：

```rust
        Some("install") => {
            let project = args[2..].iter().any(|a| a == "--project" || a == "-p");
            run_skill_install(project, json_mode);
        }
```

- [ ] **Step 2: 实现 `run_skill_install`**

在 `run_skills` 下方新增（`use std::process::Command;` 若文件顶部没有则补，已有 `exit`）：

```rust
/// `chrome-use skill install` — delegate to skills.sh (`npx skills add …`).
/// We never write runner skill dirs ourselves; skills.sh owns that mapping
/// across 20+ runners. Exit codes: npx missing -> 1 (with guidance);
/// skills.sh ran -> pass through its exit code so scripts can tell
/// "no Node" apart from "skills.sh failed".
fn run_skill_install(project: bool, json_mode: bool) -> ! {
    use std::process::Command;
    let zh = crate::connect::ui_zh();
    let argv = build_skill_install_argv(project);

    match Command::new("npx").args(&argv).status() {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            if code != 0 && !json_mode {
                // skills.sh itself failed (network/permission). Print the
                // manual one-liner as a fallback, then pass the code through.
                let g = if project { "" } else { " -g" };
                eprintln!(
                    "{} {}\n  npx skills add leeguooooo/chrome-use{}",
                    color::warning_indicator(),
                    if zh { "技能安装失败。可手动重试：" } else { "Skill install failed. Retry manually:" },
                    g
                );
            }
            exit(code);
        }
        Err(_) => {
            // npx not found (no Node). We deliberately do NOT write runner
            // dirs ourselves — print the two ways out and exit 1.
            if json_mode {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "success": false,
                        "error": "npx not found; install Node then run `chrome-use skill install`, or use the Claude Code plugin marketplace",
                    }))
                    .unwrap_or_default()
                );
            } else if zh {
                eprintln!(
                    "{} 没找到 npx（未装 Node）。装 agent 技能有两条出路：\n  1. 装 Node 后重跑：chrome-use skill install\n  2. Claude Code：/plugin marketplace add leeguooooo/plugins 再 /plugin install chrome-use@leeguooooo-plugins",
                    color::warning_indicator()
                );
            } else {
                eprintln!(
                    "{} npx not found (no Node). Two ways to install the agent skill:\n  1. Install Node, then rerun: chrome-use skill install\n  2. Claude Code: /plugin marketplace add leeguooooo/plugins then /plugin install chrome-use@leeguooooo-plugins",
                    color::warning_indicator()
                );
            }
            exit(1);
        }
    }
}
```

- [ ] **Step 3: 顶层 `skill` 别名**

`cli/src/main.rs:1157` 附近：

```rust
    // Handle skills command (doesn't need daemon). `skill` is an alias.
    if matches!(clean.first().map(|s| s.as_str()), Some("skills") | Some("skill")) {
        skills::run_skills(&clean, flags.json);
        return;
    }
```

（`run_skills` 用 `args.get(1)` 取子命令，`clean[0]` 是 `skill`/`skills` 都不影响。）

- [ ] **Step 4: 编译确认**

Run: `cargo build --manifest-path cli/Cargo.toml 2>&1 | tail -20`
Expected: 编译通过（可能有既有 warning，无 error）

- [ ] **Step 5: 手动烟测（本机有 npx）**

Run: `cargo run --manifest-path cli/Cargo.toml -- skill install --project 2>&1 | tail -8`
Expected: 触发 `npx skills add leeguooooo/chrome-use`（会真的装到当前目录 `.claude/skills` 等）。跑完后 `find .claude/skills -name SKILL.md` 应只见 `chrome-use/SKILL.md`。清理：`rm -rf .claude/skills`（勿提交测试产物）。

- [ ] **Step 6: 提交**

```bash
git add cli/src/skills.rs cli/src/main.rs
git commit -m "feat(skills): chrome-use skill install 委托 skills.sh + skill 别名"
```

---

## Task 3: doctor 只读技能探测

**Files:**
- Create: `cli/src/doctor/skill.rs`
- Modify: `cli/src/doctor/mod.rs`（`mod skill;` + `skill::check(&mut checks);`）

- [ ] **Step 1: 写失败测试**

新建 `cli/src/doctor/skill.rs`，先只放测试 + 待实现的纯函数签名：

```rust
//! Read-only probe: is the chrome-use agent skill installed anywhere an
//! agent runner would look? Never writes — installing runner dirs is
//! skills.sh's job (`chrome-use skill install`). Never Fail: a skill
//! installed into a runner we don't probe (Cursor/Windsurf/…) is normal.

use super::{Check, Status};
use std::path::{Path, PathBuf};

/// Given candidate base dirs, return those that actually contain a
/// `chrome-use/SKILL.md`. Pure + injectable for tests.
fn probe_installed(candidates: &[PathBuf]) -> Vec<PathBuf> {
    candidates
        .iter()
        .filter(|d| d.join("chrome-use").join("SKILL.md").is_file())
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn probe_finds_installed_skill() {
        let tmp = std::env::temp_dir().join(format!("cu-doctor-skill-{}", std::process::id()));
        let claude = tmp.join(".claude/skills");
        fs::create_dir_all(claude.join("chrome-use")).unwrap();
        fs::write(claude.join("chrome-use/SKILL.md"), "stub").unwrap();
        let empty = tmp.join(".codex/skills");
        fs::create_dir_all(&empty).unwrap();

        let found = probe_installed(&[claude.clone(), empty]);
        assert_eq!(found, vec![claude]);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn probe_empty_when_none_installed() {
        let tmp = std::env::temp_dir().join(format!("cu-doctor-skill-none-{}", std::process::id()));
        let d = tmp.join(".claude/skills");
        fs::create_dir_all(&d).unwrap();
        assert!(probe_installed(&[d]).is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }
}
```

- [ ] **Step 2: 运行测试确认失败（编译不过，check 未接线）**

Run: `cargo test --manifest-path cli/Cargo.toml doctor::skill 2>&1 | tail -20`
Expected: 失败 —— `skill.rs` 尚未在 mod.rs 声明，`probe_installed` 测试不被编译/运行

- [ ] **Step 3: 接线 mod.rs + 实现 `check`**

在 `cli/src/doctor/mod.rs` 的 `mod native_host;` 一组里加 `mod skill;`（保持字母序：`mod security;` 前后皆可，放 `mod security;` 之后）。在 `run_doctor` 里 `providers::check(&mut checks);` 之后加 `skill::check(&mut checks);`。

在 `cli/src/doctor/skill.rs` 的 `probe_installed` 下方加：

```rust
/// Candidate skill base dirs across the common runners (global + current
/// project). Read-only. Not exhaustive — skills.sh knows 20+; we probe the
/// popular ones and say so.
fn candidate_dirs() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(home) = dirs::home_dir() {
        v.push(home.join(".claude/skills"));
        v.push(home.join(".codex/skills"));
        v.push(home.join(".cursor/skills"));
        v.push(home.join(".agents/skills"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        v.push(cwd.join(".claude/skills"));
        v.push(cwd.join(".agents/skills"));
    }
    v
}

pub(super) fn check(checks: &mut Vec<Check>) {
    let category = "Agent skill";
    let found = probe_installed(&candidate_dirs());
    if found.is_empty() {
        checks.push(
            Check::new(
                "skill.installed",
                category,
                Status::Warn,
                "chrome-use agent skill not found in the probed runner dirs \
                 (installs into other runners like Cursor/Windsurf won't show here)",
            )
            .with_fix("chrome-use skill install"),
        );
    } else {
        checks.push(Check::new(
            "skill.installed",
            category,
            Status::Pass,
            format!("Agent skill installed ({} location(s))", found.len()),
        ));
    }
}
```

（注：`.with_fix(...)` 与 `Check::new(...).with_fix(...)` 的用法见 `cli/src/doctor/native_host.rs`。`dirs` crate 已是依赖。）

- [ ] **Step 4: 运行测试 + doctor 冒烟**

Run: `cargo test --manifest-path cli/Cargo.toml doctor::skill 2>&1 | tail -20`
Expected: `2 passed`

Run: `cargo run --manifest-path cli/Cargo.toml -- doctor --quick --offline 2>&1 | grep -i 'Agent skill\|skill.installed'`
Expected: 出现一行 Agent skill（本机装了则 pass，否则 warn），且 doctor 未因它 fail（退出码不受影响，warn 不计入 fail）。

- [ ] **Step 5: 提交**

```bash
git add cli/src/doctor/skill.rs cli/src/doctor/mod.rs
git commit -m "feat(doctor): 只读探测 agent 技能是否已装（warn 不 fail）"
```

---

## Task 4: stub 路由表补全（发现性）

**Files:**
- Modify: `skills/chrome-use/SKILL.md:36-48`（`## Specialized skills` 整节）

> markdown 改动，无 Rust 单测；验证靠 skills.sh 回归 + 目视。

- [ ] **Step 1: 替换 `## Specialized skills` 整节**

把 `skills/chrome-use/SKILL.md` 从 `## Specialized skills` 到该节末（`Run \`chrome-use skills list\` …` 段之前）替换为按症状组织的路由表：

```markdown
## Route to a specialized skill by symptom

Load a specialized guide when the task falls outside plain browser web pages.
Match on the situation you're in, then run the command — the binary serves the
full, version-matched content:

| What you're hitting | Run |
|---|---|
| An element is clearly on screen but snapshot/find returns no `@ref` (canvas/WebGL/game/map) | `chrome-use skills get canvas` |
| Mock an API response, rewrite request headers, block a URL, record HAR | `chrome-use skills get network` |
| Debug React renders/state, or measure LCP/CLS/INP | `chrome-use skills get react` |
| Drive the user's real, already-logged-in Chrome (reuse the session) | `chrome-use skills get real-chrome` |
| Parallel sessions, multiple accounts, recover a stuck tab | `chrome-use skills get sessions` |
| Turn manual checks into a re-runnable regression suite | `chrome-use skills get test` |
| Electron desktop apps (VS Code, Slack, Discord, Figma, …) | `chrome-use skills get electron` |
| Slack workspace automation | `chrome-use skills get slack` |
| Exploratory testing / QA / bug hunt | `chrome-use skills get dogfood` |
| chrome-use inside Vercel Sandbox microVMs | `chrome-use skills get vercel-sandbox` |
| AWS Bedrock AgentCore cloud browsers | `chrome-use skills get agentcore` |
```

保留其后的 `Run \`chrome-use skills list\` to see everything available…` 段作为兜底。

- [ ] **Step 2: 回归 —— skills.sh 仍只发现 1 个技能**

Run（用 scratchpad 复刻已提交内容不必，直接对当前工作树建临时 git 仓库验证；或复用先前验证方式）：

```bash
D=/private/tmp/claude-501/-Users-leo-github-com-agent-browser-stealth/133d8d62-db93-4ccf-b509-5455becc4664/scratchpad/verify2
rm -rf "$D" && mkdir -p "$D/.claude-plugin"
cp -R skills skill-data "$D/"
cp .claude-plugin/marketplace.json "$D/.claude-plugin/"
(cd "$D" && git init -q && git add -A && git -c user.email=t@t.co -c user.name=t commit -qm x)
npx -y skills@latest add "$D" --list 2>&1 | grep -aiE 'Found [0-9]+ skill|chrome-use|core|test' | sed 's/\x1b\[[0-9;]*m//g'
```

Expected: `Found 1 skill` + `chrome-use`，无 core/test。

- [ ] **Step 3: 目视确认 12 个 skill-data 场景都在表里**

Run: `grep -c 'skills get' skills/chrome-use/SKILL.md`
Expected: ≥ 13（表内 11 行 + 顶部 `skills get core` 两处）。核对 canvas/network/react/real-chrome/sessions/test 六个新增名都在。

- [ ] **Step 4: 提交**

```bash
git add skills/chrome-use/SKILL.md
git commit -m "docs(skill): stub 路由表补全 12 个场景（按症状路由）"
```

---

## Task 5: install.sh 变薄 + doctor 自检 + 首条 prompt

**Files:**
- Modify: `install.sh:114-136`（替换 skill 分支）；末尾（`case ":$PATH:"` 之前）加自检 + 首条 prompt

- [ ] **Step 1: 用 tty 分支替换现有 skill 安装块**

把 `install.sh` 里从 `# --- install the AI agent skill (skills.sh) ---` 注释到对应 `fi` 的整块，替换为：

```sh
# --- install the AI agent skill (delegates to the binary → skills.sh) --------
# The binary alone lets *you* run chrome-use; the skill teaches your AI agent
# (Claude Code, Cursor, Codex, …) how. `chrome-use skill install` shells out to
# `npx skills add …`; it never writes runner dirs itself. Non-fatal, opt-out
# with AGENT_BROWSER_NO_SKILL=1. Branch on tty (NOT exit code) so a no-Node box
# doesn't print the guidance twice.
if [ -z "${AGENT_BROWSER_NO_SKILL:-}" ]; then
  if [ -e /dev/tty ]; then
    "$bindir/${BIN_NAME}" skill install < /dev/tty > /dev/tty 2>&1 || true
  else
    "$bindir/${BIN_NAME}" skill install || true
  fi
fi
```

- [ ] **Step 2: 在 `case ":$PATH:"` 之前加自检 + 首条 prompt**

```sh
# --- self-check + first prompt ----------------------------------------------
# One read-only pass so the user sees binary/extension/skill status at a glance,
# then a copy-paste prompt that exercises the whole chain in their agent.
info "self-check..."
"$bindir/${BIN_NAME}" doctor --quick --offline 2>/dev/null || true
printf '\n\033[36m==>\033[0m %s\n\n    %s\n\n' \
  "All set. Paste this into your AI agent (Claude Code / Cursor / Codex):" \
  "Use chrome-use to open https://news.ycombinator.com and tell me the top 3 titles"
```

- [ ] **Step 3: 语法检查**

Run: `sh -n install.sh && echo OK`
Expected: `OK`

- [ ] **Step 4: 干跑非 tty 分支（不实际改机器）**

Run: `AGENT_BROWSER_NO_SKILL=1 sh -n install.sh && echo "opt-out path parses"`
Expected: `opt-out path parses`（仅语法层校验；真机跑留待 Task 6 集成）

- [ ] **Step 5: 提交**

```bash
git add install.sh
git commit -m "chore(install): skill 安装收敛为 chrome-use skill install + doctor 自检 + 首条 prompt"
```

---

## Task 6: 构建、集成验证、发版

> 本机构建慢，按记忆 `delegate-builds-to-chrome-use-build` 把 cargo 构建/clippy/test 交给 AgentParty 上的 `@chrome-use-build`；发版按 `release-flow`。

- [ ] **Step 1: 全量测试 + clippy（委托 @chrome-use-build 或本地）**

Run: `cargo test --manifest-path cli/Cargo.toml 2>&1 | tail -15` → 全绿
Run: `cargo clippy --manifest-path cli/Cargo.toml 2>&1 | tail -15` → 无新 error

- [ ] **Step 2: 真机集成（有 npx + 有 Chrome 的开发机）**

逐条验证 spec 的「测试策略」：
- `AGENT_BROWSER_NO_SKILL=1` 跳过技能步 → 二进制仍装好
- `chrome-use skill install --project` 只写当前项目 `.claude/skills`，`find` 只见 `chrome-use/SKILL.md`
- 无 Node 机器（或临时 `PATH` 去掉 npx）→ 打印两条出路，退出 1，二进制安装不受影响
- `chrome-use doctor` 三件套：binary pass / extension（视连接）/ Agent skill pass|warn，且技能一项永不 fail

- [ ] **Step 3: 版本 bump + sync + tag + push（release-flow）**

```bash
# 编辑 package.json version → 1.5.74
node scripts/sync-version.js
git add package.json cli/Cargo.toml <其余同步文件>
git commit -m "release: v1.5.74"
git tag v1.5.74
git push --no-verify origin main --tags   # 触发 release-binaries CI
```

- [ ] **Step 4: 分发生效确认**

- `npx skills add leeguooooo/chrome-use --list` → `Found 1 skill: chrome-use`（stub 路由表随 main 即时生效）
- plugin 路径靠 `leeguooooo/plugins` auto-sync cron 同步（如需即时，手动跑一次同步）
- 更新 CHANGELOG / docs changelog.html（按 `docs-site-deploy`，必要时手动触发 Pages build）

- [ ] **Step 5: 更新记忆**

在 `skills-sh-frontmatter-gotcha` 或新建一条记录本次 v2 分发设计落地（`chrome-use skill install` 委托模型、doctor 只读探测、stub 路由表）。

---

## 备注

- **DRY**：技能安装逻辑只在二进制里一份（`run_skill_install`），install.sh 只调用它。
- **YAGNI**：不做 `skill uninstall`、不做二进制直写 runner 目录、不做命名空间化多技能。
- **TDD**：可测的纯逻辑（argv 构建、目录探测）先写测试；spawn/exit 副作用壳靠集成/手动验证。
- **提交节奏**：每个 Task 一次提交，共 6 次（Task 6 含 release 提交）。
