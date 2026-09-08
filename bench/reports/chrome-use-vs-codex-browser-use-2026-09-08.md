# chrome-use 与 Codex 内置 browser-use：能力与实测体验

记录日期：2026-09-08。chrome-use 的同人对照版本为 **1.5.106**；Codex 本机插件目录版本为 **26.901.51231**。这些是本轮环境记录，不代表最新版本的能力或缺陷状态。

**chrome-use 的优势是可脚本化、快照裁剪选项丰富；Codex 内置工具的优势是默认观察流程省心，动作、观察和备用定位接口能在同一个会话里组合。** 最后一轮由同一个 agent 串行操作两套工具，内置工具完成结账，chrome-use 登录后失去驱动能力。这个结果支持本轮的体验判断，不足以证明哪套工具在所有网站上更可靠。

## 比较对象

这里的 chrome-use 是 [leeguooooo/chrome-use](https://github.com/leeguooooo/chrome-use)，不是其他同名或近似名称的浏览器自动化项目。

“Codex 内置 browser-use”指本会话通过 `mcp__cua_repl.js` 使用的浏览器能力，包含 CUA 的 AX/截图/输入接口，以及同一运行环境公开的 Playwright 定位器。实测连接类型为 **Chrome extension**，不是 in-app browser。

| 方面 | chrome-use | Codex 内置 browser-use |
|---|---|---|
| 调用入口 | CLI 命令，命名 session 维持浏览器状态 | 持久 JavaScript 会话，保存 browser/tab 对象 |
| 基本操作 | `open`、`click @ref`、`fill`、`select` | `createBrowserTab`、`click`、`setValue`、键盘与坐标操作 |
| 页面观察 | `snapshot`，支持主动选择裁剪与 diff | 创建/取得标签时自动观察；`getAXState()` 默认可返回 diff |
| 动作后观察 | `--observe` 将动作和观察放在一条命令中 | 一次工具调用可顺序执行动作，再读取状态 |
| 定位备用路径 | 文档提供 selector、find、eval 等能力 | 同一工具内可使用 Playwright locator；本轮实际用于恢复 |
| 图像观察 | 提供截图命令及输出尺寸选项 | 可直接返回截图，也可同时返回 AX 与截图 |
| 使用范围 | 可供不同 agent、终端脚本及回放流程调用 | 本次能力随 Codex 工具环境提供 |

CLI 每条命令独立启动，不等于每次都重新启动浏览器；持久 REPL 也不等于每个动作没有内部请求。比较时需要分别计数工具调用、CLI 命令和页面动作。

### 浏览器操作是否经过原生 Computer Use？

本次能确认的是接口与连接方式：`cua_repl` 同时公开浏览器和桌面应用控制，浏览器连接为扩展，页面可返回 AX 树，也能用 Playwright 接口操作。

**统一工具入口不代表所有浏览器操作都会经过 `Codex Computer Use.app`。** `AXWebArea` 等输出词汇也不能单独证明使用了 macOS 原生辅助功能。本文没有独立验证内部完整调用链，不把 CDP、原生 AX、WASM 之间的具体分工当作体验比较的前提。

## 哪些方面谁做得更好

### Codex 内置工具：默认观察流程更顺手

打开标签页就返回可操作状态，不需要先打开再另发一次快照请求。HN 实测第一步读到了完整首页 30 条，第二步点击评论链接并读取评论页，两次工具调用完成任务。

同页小变更时，默认 diff 能直接突出数量和按钮状态。例如加入两件商品后，观察结果只需新增购物车数量、把 Add to cart 改成 Remove。调用方无需预先决定是否启用差异模式。

动作与观察可自然组合：

```javascript
await tab.click(buttonIndex);
await tab.getAXState();
```

这不是内置工具独有的能力。chrome-use 已提供 `--observe`；真正需要比较的是默认使用时是否方便，以及观察返回的引用下一步能否直接使用。

### chrome-use：输出裁剪和脚本接入更灵活

其文档公开了 `snapshot -i`（交互元素）、`-s`（CSS 范围）、`-d`（深度）、`-f`（正则及祖先上下文），以及 diff、字节预算和续读能力。面对长评论页，这些选项能避免返回大量无关正文。

本轮 CUA `getAXState` 公开选项只有 `emit` 与 `disableDiffing`，没有 subtree、depth 或 interactive-only。它可以改用 Playwright 定向读取，但需要换一种观察方式。`emit:false` 仅控制自动输出，不能据此声称底层提取量更小。

CLI 命令也便于写成任务文件和回放脚本。仓库已有 [bench 回放说明](../README.md)，能记录版本、预热状态、退出码和任务断言。这是 chrome-use 的集成优势；本文没有完成两套工具全能力的逐项测试。

### 恢复体验：本次内置工具更好，但两边都有问题

chrome-use 的错误信息更详细，会给出 `tab list`、`tab select`、`tab inspect` 等下一步。然而实际操作中，列表和 inspect 都显示商品页存在，select 与 snapshot 仍报标签消失。提示具体，却没有把调用方带回可操作状态。

内置工具出现的连接错误较简短，需要自行检查当前状态。不过最后一轮可以通过同一工具内的 DOM/Playwright 接口恢复登录，之后完成购物流程。

这不说明内置工具一直可靠：另一遍重跑用了 31 次调用，最终仍卡在 Checkout。恢复能力应按成功率和恢复成本评价，不能只挑完成的那一次。

### 两边都需要把“动作成功”与“观察成功”分开

内置工具两次出现排序 `setValue` 报错：

```text
Select did not retain the requested option
```

下一次 AX 却确认 Price (low to high) 已选中，商品也已重排。能确定的是“返回错误，但随后可见状态已成功”；为什么校验报错尚未确认。

chrome-use 的 Login 点击返回 Done，同时给出未稳定警告，observed URL 变为空串。后续标签列表位于 inventory.html，说明不能简单认定登录没发生。问题在于返回结果没有清楚区分动作派发、导航结果和观察是否可用。

理想输出应让调用方知道：目标已确认达到、已确认未达到，还是结果未确认。对结果未知的操作，不应自动重放。

## 实测记录

| 场景 | 工具与执行者 | 调用记录 | 结果与限制 |
|---|---|---|---|
| HN 首页找评论最多帖子 | 本 agent，内置工具 | 2 次 CUA | 完成，进入评论页核实；复用已有浏览器连接 |
| HN 同任务 | 另一 agent，chrome-use | 对方报告 4 次，其中一次重复快照 | 报告结果一致；不是同人对照，不能把重复操作算成工具必要成本 |
| SauceDemo 首次 | 本 agent，内置工具 | 17 次 CUA | 完成；排序经失败恢复，最终用 Playwright selectOption |
| SauceDemo 再跑 | 本 agent，内置工具 | 31 次 CUA | 未完成；排序报错但生效，加购刷新后恢复，最终 Checkout 无响应 |
| SauceDemo 同人对照，先跑 | 本 agent，chrome-use 1.5.106 | 9 次浏览器命令，另 3 次准备命令 | 未完成；登录后失去驱动能力，未到排序 |
| SauceDemo 同人对照，后跑 | 本 agent，内置工具 | 13 次 CUA | 完成；登录需 Playwright 恢复，排序报错但实际生效 |
| Not a Robot | 本 agent，内置工具 | 执行轮 28 次 CUA，另一次准备调用 | 通过前 4 关；第 5 关三次验证失败后停止，无对方结果可比较 |

准备命令指 chrome-use 的版本查询、core 和 real-chrome 技能读取；本地技能文件读取另计。CUA 次数包含错误、截图、恢复以及执行轮内的现场保留操作。**CLI 命令数与 CUA 工具调用数不是等价的网络往返数。**

成功的 SauceDemo 运行均从汇总页读到 Item total **$17.98**、Tax **$1.44**、Total **$19.42**，随后点击 Finish 并确认成功页。未完成的运行没有借用其他运行的金额充当结果。

HN 观察到的最高评论帖子为 [Nitter and XCancel resume service after legal advice](https://news.ycombinator.com/item?id=49588988)，当时为 347 条评论；这是历史快照，不是当前排名。

游戏测试使用正常 UI、AX 和截图，不读取隐藏答案。第 5 关卡在图块方向与道路接缝判断，操作接口未报错，因此不能归因为工具不支持旋转拼图。另一工具尚无可用结果，不能给出游戏能力胜负。

### 最后一轮的关键路径

chrome-use：

```text
open --observe → 填写凭据 → click Login --observe
→ Done + 页面未稳定警告 + 空 observed URL
→ snapshot 报 tab gone
→ tab list 显示 created 标签位于 inventory.html
→ 按提示 tab select，仍报 tab gone
→ snapshot 再次失败
→ tab inspect 能返回商品页标题与 URL
→ 停止，未到排序
```

内置工具：

```text
打开 → 登录连接错误 → DOM 接口恢复登录
→ 移除已有两件测试商品，确认空车
→ setValue 排序报错 → AX 确认排序成功
→ 加入两件商品 → 购物车 → Checkout
→ 填写信息 → Continue → 读取三项金额 → Finish 成功
```

排序确认成功后直接继续，没有因排序报错重试排序。31 次调用那一遍的刷新是为恢复加购无响应，不能写成“排序假失败导致刷新”的因果链。

## 数据能说明什么

本线程 CUA 调用次数已从 rollout 的工具调用记录重新核对。历史转述的字节统计与当前日志序列化结果并不采用同一口径，因此本文不发布精确的字节倍数或速度排名。

以后应分开统计页面文本字节、日志封装字节、图像数据和模型输入 token。截图运行不能只用文本长度作为成本。工具执行时间、模型推理时间、等待人工或其他会话的时间也不能混在一起。

本次还存在以下限制：

- 同人对照中，CLI 自动选中了已连接的 Chrome profile，CUA 沿用已有 browser binding；未核实两者 profile 一致。
- CUA 是 warm session，CLI 使用新命名 session，未单独预热。warm binding 也不保证底层连接始终处于热状态。
- CUA 登录后发现已有测试购物车，额外执行了清理。新标签不等于网站状态隔离。
- 每个条件没有多次重复；工具顺序固定，agent 已熟悉任务。一个人测试两套工具能减少执行者差异，不能消除学习和顺序偏差。
- detached 不能直接归因于两个工具争抢，同一浏览器的不同标签也不能一概视为冲突。
- 截图尺寸变化没有被确认为 viewport override；默认 diff 在跨页时返回全树的内部判定也未确认。

## 使用与改进建议

| 需求 | 本轮更倾向的选择 | 理由 |
|---|---|---|
| Codex 内临时浏览、多步表单、需要观察后继续操作 | 内置 browser-use | 默认状态输出与 diff 方便；本次能用备用定位接口恢复 |
| 终端脚本、可回放任务、跨 agent 接入 | chrome-use | CLI/session 适合编排，已有基准回放流程 |
| 长页面定向读取、严格控制返回内容 | chrome-use 的裁剪快照，或内置 Playwright 定向读取 | 前者选项更集中，后者需要显式定位目标 |
| 当前真实 Chrome 上的关键流程 | 先做短程验收，再选工具 | 两边都出现失败；本轮不能承诺无人值守可靠性 |
| 视觉谜题与游戏 | 暂不排名 | 只有内置工具单边记录，视觉推理与工具可靠性应分开 |

这些选择来自本轮体验，不是对最新版本的通用推荐。

chrome-use 优先改进结果分层和恢复验证：确认“恢复后还能驱动原页面”，保留目标身份，并避免重复推荐已失败的恢复步骤。其次是带语义上下文的裁剪快照、短文档入口和默认脱敏的本地诊断包。

内置工具最需要改进的是排序结果的误报、连接异常的恢复说明，以及动作无效时的可诊断性。默认 diff 已减少观察负担，但大页面若能提供直接的范围或预算控制，调用方就不必为减少输出切换接口。

相关反馈已归档：

- [#235 恢复路径不闭环](https://github.com/leeguooooo/chrome-use/issues/235)
- [#236 核心技能文档过长且描述冲突](https://github.com/leeguooooo/chrome-use/issues/236)
- [#237 动作与观察结果、诊断上下文及体验改进](https://github.com/leeguooooo/chrome-use/issues/237)
- [#231 基准完成率与 warm/cold 记录](https://github.com/leeguooooo/chrome-use/issues/231)

以上链接用于定位反馈，不表示当前仍未修复。后续对照应固定网站初始状态、profile、视口和版本，交换执行顺序，并重复多次；报告完成率、恢复成功率和完整失败成本后，再比较耗时与输出体积。

## 资料范围

实测来源为本会话 2026-09-07 至 2026-09-08 的操作记录，thread ID 为 `01a07c02-be85-71b3-880e-6c5832340953`。工具能力来自当时返回的 CUA/Playwright 文档及 chrome-use 1.5.106 内置技能。本文没有读取 Codex 实现源码或反编译结果，也没有重新执行浏览基准。
