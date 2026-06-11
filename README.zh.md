# agent-browser-stealth

[English](README.md) · **简体中文**

![agent-browser-stealth](assets/hero.png)

[agent-browser](https://github.com/vercel-labs/agent-browser) 的隐身分支 —— 直接连接**你自己**正在用的、已登录的 Chrome，复用你的登录态，对反爬/反自动化系统**完全不可检测**。

基础用法、命令与 API 参考见[上游文档](https://github.com/vercel-labs/agent-browser)。

## 把你**已经登录好**的浏览器，交给你的 AI agent

**不用开新 Chrome。不用重新登录。不用跟"你是不是机器人"较劲。**

agent-browser-stealth 让**任意** agent（Claude Code、Cursor、Codex、你自己的脚本）直接操作你**已经登录了所有网站**的那个 Chrome。它在**你的窗口里**点击，你看着它干活，撞到 2FA / 验证码的瞬间你接管一下，它接着跑。因为它**就是你的真实浏览器**（一键装的扩展、原生消息、无调试端口），网站眼里它 100% 是人：**[CreepJS 实测 0% 机器人](#反检测)。**

**为什么不用……**

- **Playwright / Puppeteer / browser-use？** 它们开的是**空**浏览器 —— 每个登录你重做、每个验证码你硬扛、最后还被标成自动化。我们直接用你**现成的**会话。
- **Claude 的 Chrome 插件？** 很好，但**只能给 Claude 用**。我们给**任意** agent / CLI 用。
- **裸 `--remote-debugging-port`**（web-access 等）？ Chrome 136+ **每次连都弹** "Allow remote debugging?"。我们**永不弹** —— 商店一键装，原生消息。

<details>
<summary><b>完整对比矩阵</b>（要细节的看这里）</summary>

| | [Claude in Chrome](https://www.anthropic.com/claude/chrome) | web-access / 裸 CDP 端口 | Playwright · Puppeteer · browser-use | **agent-browser-stealth** |
|---|:---:|:---:|:---:|:---:|
| **任意** agent / CLI 都能用（不绑单一 app） | ❌ 仅 Claude | ✅ | ✅ | ✅ |
| 驱动你**真实、已登录**的 Chrome | ✅ | ✅ | ❌ 全新空 profile | ✅ |
| **不弹 "Allow remote debugging?"** | ✅ | ❌ 每次连都弹 | —（自带浏览器） | ✅ 原生消息 |
| 真实浏览器指纹（CreepJS ~0%）¹ | ✅ | ✅ | ❌ 自动化特征 / headless | ✅ **已实测 0%** |
| **无 `Runtime.enable` CDP 泄漏**（rebrowser）² | — | ❌ 泄漏 | ❌ 泄漏 | ✅ **默认关闭** |
| 多 agent 共用**同一个**真实 Chrome、标签组隔离³ | ❌ 单 app | ⚠️ 共享 tab、无隔离 | ❌ 各开各的浏览器 | ✅ |
| 权限面 | 16 个，含 `<all_urls>` | 完整 CDP | 完全控制 | **7 个，无 `<all_urls>`** |

<sub>¹ 三家"真实 Chrome"工具在 CreepJS 上都 ~0%（毕竟是真浏览器），我们的是实测过的。² rebrowser `runtimeEnableLeak` —— 我们的中继路径实测无泄漏；Claude in Chrome 未独立测试（—）。³ web-access 也能跑并行子 agent，但无每会话隔离；本工具每个 `--session` 拿到自己彩色、命令隔离的标签组。实测数字见 [反检测](#反检测)。</sub>

</details>

## 为什么要 fork

<img src="assets/fingerprint.png" alt="真实但不可检测的指纹" width="300" align="right" />

**agent-browser**（上游）启动的是空 profile 的全新浏览器：你得重新登录，网站也能看出是自动化。

**agent-browser-stealth** 连接你**现有**的 Chrome —— cookies、会话、浏览器指纹全是真的，因为它**就是**你的真实浏览器。

| | agent-browser | agent-browser-stealth |
|---|---|---|
| 浏览器 | 启动新 Chrome | 连接你的 Chrome |
| 登录态 | 空，要重新登 | 你现有的会话 |
| 指纹 | 带自动化标记 | 你的真实指纹 |
| 协作 | 独立窗口 | 同一窗口，随时接管 |
| 验证码 | Agent 卡住 | 你点一下，Agent 继续 |

## 工作原理

![工作原理](assets/how-it-works.png)

你的 **agent-browser CLI** 通过 Chrome **原生消息（native messaging）** 和一个小**浏览器扩展**通信 —— 这是本机进程间通道，**无网络端口、无 token、无远程服务器**。扩展用 `chrome.debugger` 驱动你指定的标签页（在你**已登录**的 Chrome 里），再把结果交还给 CLI。全程都在你本机。

![架构](assets/architecture.png)

每个 `--session` 拿到**自己的彩色标签组**，多个 agent 共用同一个真实浏览器、互不干扰，也不动你自己的标签页。

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/leeguooooo/agent-browser-stealth/main/install.sh | sh
```

从最新的 [GitHub Release](https://github.com/leeguooooo/agent-browser-stealth/releases) 下载对应平台的预编译二进制，安装 `agent-browser`（以及 `abs` 别名）。无需 npm，无需 token。

### 安装 AI agent skills

```bash
npx skills add leeguooooo/agent-browser-stealth
```

把 `skills/agent-browser` 拉进当前项目，让你的 AI agent 拿到正确的用法和预授权的 bash 权限。

## 连接你的 Chrome

**推荐 —— 浏览器扩展（一键，无弹窗）。** 从 Chrome 应用商店安装 [**agent-browser-stealth** 扩展](https://chromewebstore.google.com/detail/agent-browser-stealth/knfcmbamhjmaonkfnjhldjedeobeafmk)，再注册一次本地桥：

```bash
agent-browser extension install      # 注册原生消息 host（一次性）
agent-browser open https://x.com/home
```

之后 `agent-browser open` 就通过**原生消息**驱动你真实、已登录的 Chrome —— 无调试端口、无 token、**永远不弹 "Allow remote debugging?"**。扩展自动更新、重启不掉，零确认（适合无人值守 / agent 场景）。

<details>
<summary>备选 —— 裸 remote-debugging 端口（会弹同意框）</summary>

不装扩展时，agent-browser 退回用 CDP 连接，而 Chrome 只在带 remote-debugging 端口启动时才暴露它：

```bash
# macOS
open -a "Google Chrome" --args --remote-debugging-port=9222
# Linux
google-chrome --remote-debugging-port=9222
# Windows: 给 Chrome 快捷方式 target 加 --remote-debugging-port=9222
```

然后 `agent-browser open <url>` 自动发现端口。首次连接 **Chrome 136+ 会弹 "Allow remote debugging?"** —— 点一次 Allow（该 Chrome 会话内持续有效）。上面的扩展则完全避开这个框。
</details>

## 用法

```bash
# 连接你的 Chrome 并导航
agent-browser open https://example.com

# 一切都在你已登录的浏览器里进行
agent-browser click "Post"
agent-browser fill "Title" "Hello World"
agent-browser screenshot ./page.png
```

Agent 在你的 Chrome 里操作 —— 你能实时看到开标签、加载、点击。任意时刻都能接管（比如手动过验证码），然后让 agent 继续。

### 独立模式（`--launch`）

```bash
# 临时：全新空 profile —— 无 cookie 无登录（适合 CI / 测试）
agent-browser --launch open https://example.com

# 保留登录：用你真实的 Chrome profile 启动
agent-browser --launch --profile auto open https://x.com/home
```

## 反检测

连接你真实 Chrome 时，我们**零** JS 注入 —— 浏览器指纹完全是真的。指导原则是 **native CDP/Chrome 覆盖优先于 JS 谎言**：被重定义的 getter 本身可被检测，原生覆盖则不会。

- `navigator.webdriver = false` 走 `Emulation.setAutomationOverride`（原生，CreepJS 类说谎检测查不出）。
- **`Runtime.enable` 默认关闭** —— 活着的 `Runtime` 域是可被检测的 CDP 信号（patchright/rebrowser 的 "runtime leak"），即便连的是你真实 Chrome。只在你主动开启 console/错误捕获时才启用。

**实测结果（连接真实 Chrome，中继路径）：**

| 检测站 | 结果 |
|---|---|
| [CreepJS](https://abrahamjuliot.github.io/creepjs/) | **0% stealth · 0% headless**（零 override 痕迹） |
| [bot.incolumitas.com](https://bot.incolumitas.com/) | 全部 OK（overflowTest / overrideTest / puppeteerExtraStealth / worker 一致性） |
| [rebrowser-bot-detector](https://bot-detector.rebrowser.net/) | `runtimeEnableLeak` 🟢 · `pwInitScripts` 🟢 |
| [bot.sannysoft.com](https://bot.sannysoft.com) | 全绿 |

`--launch` 独立模式下会改用一整套隐身补丁，同样过上述检测。

### 类人输入（行为隐身）

指纹隐身只是一半——最强的反爬厂商（Akamai、PerimeterX、DataDome）还会给**行为**打分。点击时光标瞬移到元素正中心、没有接近轨迹、按下即抬起,这本身就是破绽,**哪怕我们的 CDP 事件是 `isTrusted`**。

开启 humanize 后,光标像手在动:点击走带减速的贝塞尔曲线、落在元素内**偏离正中心**的抖动点;打字用变速的击键间隔;滚动分段缓动;拖拽走曲线。而且**自适应**——每次导航探测页面是否有已知反爬厂商(cookie/脚本/全局变量),命中就自动升到全套类人动作,普通站点保持瞬时(零开销)。

页面自己的 `mousemove` 流看到的(行为检测器分析的正是这个):

| | 轨迹 |
|---|---|
| **off**（默认） | 直线 · 死磕正中心 · 瞬时 |
| **human** | 曲线 · 先慢后快再慢 · 落点偏移 |

用 `--humanize off\|fast\|human` 或 `AGENT_BROWSER_HUMANIZE` 控制。默认 `off`,自适应检测器按页面自动升档。

### 静默操作

操作你的真实 Chrome 不该打断你的工作。agent **全程在后台操作**:新标签后台打开(在自己的彩色会话标签组里),**从不强制把标签拽到前台**,并用 `Emulation.setFocusEmulationEnabled` 让每个 agent 标签照常渲染、`document.hasFocus()` / `visibilityState` 仍报 `visible`。于是截图正常、页面不被降频,"标签全程隐藏"也不会变成新的机器人信号。你在自己的标签里照常工作,agent 在旁边默默干活。(想置顶某个标签仍可显式调用命令。)

## 与上游的差异

基于 [agent-browser v0.27.0](https://github.com/vercel-labs/agent-browser)：

- **默认 auto-connect** —— `agent-browser open` 连你的 Chrome 而非启新的
- **CDP 原生隐身** —— `Emulation.setAutomationOverride` 而非 JS 补丁
- **双隐身模式** —— 真实 Chrome 零补丁，`--launch` 全补丁
- **`--launch` / `--new`** —— 显式启动独立浏览器
- **CI 自动检测** —— 设了 `CI` 环境变量时走独立模式

所有上游功能（命令、快照、截图、录制、标签、会话等）保持一致。

## License

Apache-2.0（与上游一致）
