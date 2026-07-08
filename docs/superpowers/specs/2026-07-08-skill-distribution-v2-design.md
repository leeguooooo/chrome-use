# chrome-use 技能分发 v2 设计

- 日期：2026-07-08
- 状态：设计已获用户认可，待 spec review
- 相关仓库：`leeguooooo/chrome-use`（本仓库，remote `agent-browser-stealth`）

## 背景

本次会话先修了三个安装 UX 问题（已合并到 main，commit `9a2306f7` / `08aa11f1`）：

1. `curl … install.sh | sh` 装完二进制不装 agent 技能。
2. 文档只主推 curl，技能步骤不醒目。
3. `npx skills add leeguooooo/chrome-use` 装成了 `core`/`test`/`canvas`…，看不到 `chrome-use`——根因是 `skills/chrome-use/SKILL.md` 的 `description` 是含 `: `（冒号+空格）的非法 YAML 纯量，skills.sh 静默丢弃该技能后触发深度兜底扫描，把 `skill-data/*` 内容章节当技能装了进去。已用折叠块标量 `>-` 修复（详见记忆 `skills-sh-frontmatter-gotcha`）。

三个热修复到位后，用户要求「好好想想这个功能」——本文是对技能分发链路的一次系统重审。

## 架构现状（事实源）

- `skills/chrome-use/SKILL.md`：**发现存根（stub）**，很短，装进 agent 的技能目录。它的设计前提是「内容不随版本变」——只指向 `chrome-use skills get`。
- `skill-data/*/`（core 59KB、electron、slack、dogfood、canvas、network、react、real-chrome、sessions、test、agentcore、vercel-sandbox 共 12 个）：**编译进二进制**（`cli/src/skills.rs:14` `include_dir!("$CARGO_MANIFEST_DIR/../skill-data")`），agent 按需 `chrome-use skills get <name>` 加载，内容永远和已装二进制版本一致。
- 三条分发渠道：
  - `/plugin install chrome-use@leeguooooo-plugins`（Claude Code plugin marketplace，自动更新，走 `leeguooooo/plugins`）
  - `npx skills add leeguooooo/chrome-use`（skills.sh，覆盖 20+ runner 的目录映射）
  - `curl … install.sh | sh`（GitHub Release 二进制 + 现已顺带装技能）

**核心不变式：二进制是唯一事实源。** stub 是版本稳定的入口，真正内容永远由本机二进制的 `skills get` 释出。任何设计都不得破坏这条不变式。

## 决策约束（来自用户澄清）

- **出发点**：发现性、安装链路、整体架构、用户增长四者都要顾。
- **目标用户**：runner 无关，一视同仁（Claude Code / Cursor / Codex / Windsurf / 自研 agent 同等重要）。
- **依赖策略**：两条腿都要——二进制自装为兜底方向，skills.sh/plugin 为主渠道。**但**（后续澄清收敛）：`chrome-use skill install` 走 `npx skills add` 的方式，**不自己处理 runner 目录映射**。runner 目录写入永远归 skills.sh 管，我们不重复造轮子。
- **发现性**：单入口 + 强化路由表（不做命名空间化多技能，避免 10+ 个 description 常驻每个 session 的上下文）。
- **安装成功终点**：`chrome-use doctor` 自检 + 打印给 agent 的首条 prompt。

## 设计

### 组件 1：`chrome-use skill install` 命令（新增）

统一入口，本质是 skills.sh 的包装器。

**行为**：

- 默认全局（透传 `-g`）；`--project` 装当前项目（透传给 skills.sh，不加 `-g`）。
- 探测 `npx`：
  - **存在** → 执行 `npx -y skills@latest add leeguooooo/chrome-use [-g]`，透传其 stdout/stderr（skills.sh 自己有交互/agent 探测）。
  - **不存在** → **不自己写任何 runner 目录**（这是用户定的硬边界）。打印两条出路：
    1. 装 Node 后重跑 `chrome-use skill install`
    2. Claude Code 用户走 `/plugin marketplace add leeguooooo/plugins && /plugin install chrome-use@leeguooooo-plugins`
- 输出经 `ui_zh()`：默认英文，检测到 zh locale / `CHROME_USE_LANG` 才中文（与既有 i18n 一致，见记忆 `i18n-english-default`）。
- 退出码：skills.sh 成功→0；npx 缺失→非 0（提示性失败，便于脚本判断），但在 install.sh 里以 `|| true` 消化，绝不因技能装不上而让二进制安装失败。

**明确不做（YAGNI）**：

- 不做 `chrome-use skill uninstall`（skills.sh 有 `skills remove`）。
- 不自建 runner 目录映射表（skills.sh 已维护 20+ runner 的 `AGENT_PROJECT_SKILL_DIRS`）。
- 不做二进制直写 stub 到 runner 目录的「零依赖」路径（本次收敛掉；若将来 Node 依赖成为真实痛点再议）。

### 组件 2：install.sh 变薄

现状 install.sh 里约 23 行的 npx 分支逻辑（tty/非 tty/无 npx 三分支）**收敛为一行**：

```sh
if [ -z "${AGENT_BROWSER_NO_SKILL:-}" ]; then
  if [ -e /dev/tty ]; then
    "$bindir/${BIN_NAME}" skill install < /dev/tty > /dev/tty 2>&1 || true
  else
    "$bindir/${BIN_NAME}" skill install || true
  fi
fi
```

分支**按 tty 是否存在**选择（而非按退出码重试）。这一点很重要：`skill install` 在无 Node 时会**故意返回非 0**（组件 1），若用退出码触发重试，无 Node 但有 tty 的机器会把「两条出路」提示打印两遍。改成按 tty 分支后，交互/非交互各跑一次，绝不重复。逻辑本体只在二进制里维护一份。保留 `AGENT_BROWSER_NO_SKILL=1` opt-out。

### 组件 3：install.sh 新终点——doctor 自检 + 首条 prompt

在 extension + skill 步骤之后追加：

```
==> 自检...
    （运行 chrome-use doctor，只读，无需 tty，CI 可跑）
==> 全部就绪。去你的 AI agent（Claude Code / Cursor / Codex）里粘贴这句试试：

    用 chrome-use 打开 https://news.ycombinator.com ，告诉我 Top 3 标题
```

- 自检 = `chrome-use doctor`（只读）。任何黄/红灯，doctor 自带修复指引，脚本不重复。
- 首条 prompt 故意选一个走完整链路的任务：技能发现 → `skills get core` → `open` → `read` → 回答。让人 30 秒内看到 aha moment（服务「用户增长」目标）。

### 组件 4：doctor 新增「agent 技能」检查项

- **只读探测**几个最常见位置：`~/.claude/skills/chrome-use`、`~/.codex/skills/chrome-use`、`.agents/skills/chrome-use`、当前项目 `.claude/skills/chrome-use`、Claude Code plugin 缓存（`~/.claude/plugins/**/chrome-use`）。
- 都没有 → 黄灯：「agent 还不认识 chrome-use，跑 `chrome-use skill install`」，并注明「其他 runner 装的这里检测不到属正常」。
- 边界重申：**只读探测，不写入**。写入永远归 skills.sh 管。

### 组件 5：stub 路由表强化（发现性）

现状 stub 只列了 5 个专门化技能，`skill-data` 实有 12 个——`canvas / network / react / real-chrome / sessions / test` 六个在 stub 里缺失，agent 无从发现。

把「列表」升级为「场景 → 命令」路由表，按 agent 遇到的**症状**组织（agent 按当下困境匹配，而非按名字）：

| 你遇到的情况 | 先跑 |
|---|---|
| 元素明明在屏幕上，snapshot/find 却拿不到 @ref（canvas/WebGL/游戏/地图） | `chrome-use skills get canvas` |
| 要 mock API 响应、改请求头、屏蔽 URL、录 HAR | `chrome-use skills get network` |
| 调试 React 渲染/组件状态，或测 LCP/CLS/INP | `chrome-use skills get react` |
| 驱动用户真实已登录的 Chrome（复用登录态） | `chrome-use skills get real-chrome` |
| 多会话并行、多账号、tab 卡死要恢复 | `chrome-use skills get sessions` |
| 把手工检查固化成可重跑的回归测试套件 | `chrome-use skills get test` |
| Electron 桌面应用（VS Code/Slack/Discord/Figma…） | `chrome-use skills get electron` |
| Slack 工作区自动化 | `chrome-use skills get slack` |
| 探索性测试/QA/bug hunt | `chrome-use skills get dogfood` |
| Vercel Sandbox / AWS AgentCore 云浏览器 | `chrome-use skills get vercel-sandbox` / `agentcore` |

- 版本稳定性不破坏：表里只有**名字和场景**（都稳定），内容仍由二进制保鲜；末尾保留 `chrome-use skills list` 作为兜底（未来新增章节靠它发现）。
- stub 体积从 4.1K 涨到约 5K，一次性成本可忽略。

## 数据流（新用户首次安装）

```
curl … | sh
  ├─ 下载 + 校验 + 安装二进制 → $bindir/chrome-use
  ├─ chrome-use extension install（交互，装 Chrome 扩展 + 注册 native host）
  ├─ chrome-use skill install → npx skills add leeguooooo/chrome-use -g
  │     └─ skills.sh 探测 runner，写入各自技能目录（skills.sh 的活）
  ├─ chrome-use doctor（自检：binary ✓ / extension ✓ / skill ✓⚠）
  │     （skill 可能为黄灯——skills.sh 写到了 doctor 探测清单外的 runner
  │      目录如 Cursor/Windsurf 时属正常，不是失败）
  └─ 打印首条 agent prompt
                │
用户粘贴 prompt 进 agent
  └─ agent 命中 chrome-use 技能（stub）
       ├─ chrome-use skills get core（二进制释出完整正文）
       ├─ 遇专门化场景 → 查路由表 → skills get <name>
       └─ open → read → 回答 → aha moment
```

## 错误处理

- npx 缺失：`skill install` 提示性失败（退出码 **1**），install.sh `|| true` 消化，绝不阻断二进制安装。
- skills.sh 本身失败（网络/权限）：透传其错误 + 打印手动一行命令兜底，`skill install` **原样返回 skills.sh 的退出码**（便于脚本区分「没装 Node」与「装了但 skills.sh 跑挂了」；单元测试据此断言）。
- doctor 技能检查：永远 warn，绝不 fail（其他 runner 检测不到属正常）。
- stub 分发的 main 分支版本 ≠ 本机二进制版本：不构成问题（stub 版本稳定，正文永远由本机二进制释出）。

## 测试策略

- 单元：`skill install` 的 arg 解析（`--project` → 不带 `-g`；默认 → `-g`）；npx 存在/缺失两分支的输出与退出码；`ui_zh()` 中英切换。
- 单元：doctor 技能探测——mock 各目录存在/缺失，断言黄灯文案与不 fail。
- 集成（人工，二进制在位，见记忆 `delegate-builds-to-chrome-use-build` 委托构建）：
  - 全新机器跑 `curl … | sh`，验证三件套自检全绿 + 首条 prompt 打印。
  - `AGENT_BROWSER_NO_SKILL=1` 跳过技能步。
  - `chrome-use skill install --project` 只写当前项目 `.claude/skills`。
  - 无 Node 机器：打印两条出路，二进制安装成功。
- 回归：`npx skills add leeguooooo/chrome-use --list` 仍报 `Found 1 skill: chrome-use`（守住本次会话的 YAML 修复）。

## 分发注意

- `chrome-use skill install` / doctor / stub 都需要**发新二进制版本**才生效（走 `release-flow` 记忆的流程；构建委托 `@chrome-use-build`）。
- stub 路由表改的是 `skills/chrome-use/SKILL.md`，`npx skills add` 路径 push main 即生效；plugin 路径靠 `leeguooooo/plugins` auto-sync cron 同步。
- 关联记忆：`skills-sh-frontmatter-gotcha`、`plugins-marketplace-use-family`、`release-flow`、`i18n-english-default`、`delegate-builds-to-chrome-use-build`、`docs-site-deploy`。

## 明确排除范围（YAGNI）

- 二进制直写 runner 目录的零依赖路径。
- `chrome-use skill uninstall`。
- 命名空间化多技能（chrome-use-electron 等独立技能）。
- 自建 runner 目录映射表。
- 远程 relay / 云端分发（一贯保持本地）。
