# chrome-use

[English](README.md) · **简体中文**

![chrome-use](assets/hero.png)

**chrome-use** 让任意 AI agent 直接操作你自己正在用的、已登录的 Chrome —— 复用你的登录态，对反爬/反自动化系统**完全不可检测**，因为它**就是**你的真实浏览器。属于 `*-use` 家族（iphone-use 驱动你的真实 iPhone，chrome-use 驱动你的真实 Chrome）。

<sub>最初基于 [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser)（Apache-2.0）；现已是独立项目 —— 隐身/扩展中继架构、反检测、humanize、多 agent 隔离与 CLI 都已大幅分化。</sub>

## 把你**已经登录好**的浏览器，交给你的 AI agent

**不用开新 Chrome。不用重新登录。不用跟"你是不是机器人"较劲。**

chrome-use 让**任意** agent（Claude Code、Cursor、Codex、你自己的脚本）直接操作你**已经登录了所有网站**的那个 Chrome。它在**你的窗口里**点击，你看着它干活，撞到 2FA / 验证码的瞬间你接管一下，它接着跑。因为它**就是你的真实浏览器**（一键装的扩展、原生消息、无调试端口），网站眼里它 100% 是人：**[CreepJS 实测 0% 机器人](#反检测)。**

**为什么不用……**

- **Playwright / Puppeteer / browser-use？** 它们开的是**空**浏览器 —— 每个登录你重做、每个验证码你硬扛、最后还被标成自动化。我们直接用你**现成的**会话。
- **Claude 的 Chrome 插件？** 很好，但**只能给 Claude 用**。我们给**任意** agent / CLI 用。
- **裸 `--remote-debugging-port`**（web-access 等）？ Chrome 136+ **每次连都弹** "Allow remote debugging?"。我们**永不弹** —— 商店一键装，原生消息。

<details>
<summary><b>完整对比矩阵</b>（要细节的看这里）</summary>

| | [Claude in Chrome](https://www.anthropic.com/claude/chrome) | web-access / 裸 CDP 端口 | Playwright · Puppeteer · browser-use | **chrome-use** |
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

## 为什么选 chrome-use

<img src="assets/fingerprint.png" alt="真实但不可检测的指纹" width="300" align="right" />

**常规浏览器自动化**（Playwright / Puppeteer，或全新 `--launch`）启动的是空 profile 的全新浏览器：你得重新登录，网站也能看出是自动化。

**chrome-use** 连接你**现有**的 Chrome —— cookies、会话、浏览器指纹全是真的，因为它**就是**你的真实浏览器。

| | 常规自动化 | chrome-use |
|---|---|---|
| 浏览器 | 启动新 Chrome | 连接你的 Chrome |
| 登录态 | 空，要重新登 | 你现有的会话 |
| 指纹 | 带自动化标记 | 你的真实指纹 |
| 协作 | 独立窗口 | 同一窗口，随时接管 |
| 验证码 | Agent 卡住 | 你点一下，Agent 继续 |

## 工作原理

![工作原理](assets/how-it-works.png)

你的 **chrome-use CLI** 通过 Chrome **原生消息（native messaging）** 和一个小**浏览器扩展**通信 —— 这是本机进程间通道，**无网络端口、无 token、无远程服务器**。扩展用 `chrome.debugger` 驱动你指定的标签页（在你**已登录**的 Chrome 里），再把结果交还给 CLI。全程都在你本机。

![架构](assets/architecture.png)

每个 `--session` 拿到**自己的彩色标签组**，多个 agent 共用同一个真实浏览器、互不干扰，也不动你自己的标签页。

## 为什么用扩展（而非裸调试端口）

其他本地工具走裸 `--remote-debugging-port`（CDP）驱动 Chrome。从 **Chrome 136** 起，每次这样连接都会弹出一个阻塞式的 **"Allow remote debugging?"** 同意框 —— 而且端口得提前开好。我们的扩展改用原生消息：**装一次，之后零确认。**

| | **chrome-use**（本扩展） | web-access（裸 CDP 端口） | Claude in Chrome（chrome.debugger） |
|---|---|---|---|
| 连接方式 | 原生消息 —— 无端口、无 token | `--remote-debugging-port` | `chrome.debugger` |
| **"Allow remote debugging?" 弹框** | **从不** ✅ | **每次连都弹** 🔴 | 无 |
| 复用你的真实登录 | 是 | 是 | 是 |
| `Runtime.enable`（CDP）泄漏¹ | **默认关闭 → 干净** ✅ | 域已启用 | 不适用 |
| CreepJS 隐身分² | **0% stealth · 0% headless** ✅ | 真实 Chrome | 真实 Chrome |
| 每会话标签组 / 并发 agent | **支持** ✅ | 无 | 无 |
| 为 chrome-use CLI 打造 | 是 | 独立代理 | 单 app 助手 |

> ¹ 对 [rebrowser-bot-detector](https://bot-detector.rebrowser.net/) 实测：我们的中继报 `runtimeEnableLeak: 🟢 No leak`、`navigatorWebdriver: 🟢`。
> ² 对 [CreepJS](https://abrahamjuliot.github.io/creepjs/) 在「连接真实 Chrome」路径上实测 —— 见 [反检测](#反检测)。
>
> 同意框不是假想：裸端口工具**每次** attach 都会弹（Chrome 136+ 安全策略）。扩展路径从不弹。

## 安装

```bash
curl -fsSL https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh | sh
```

从最新的 [GitHub Release](https://github.com/leeguooooo/chrome-use/releases) 下载对应平台的预编译二进制，安装 `chrome-use`（以及 `abs` 别名）。无需 npm，无需 token。

<details>
<summary>其他安装方式</summary>

- **锁定版本：** `AGENT_BROWSER_VERSION=v0.27.0-fork.12 curl -fsSL https://raw.githubusercontent.com/leeguooooo/chrome-use/main/install.sh | sh`
- **自定义路径：** `AGENT_BROWSER_BIN_DIR=$HOME/bin curl -fsSL … | sh`
- **Windows：** 从 [Releases 页](https://github.com/leeguooooo/chrome-use/releases) 下载 `chrome-use-win32-x64.tar.gz`，把 `chrome-use.exe` 放进 PATH。
- **npm（旧渠道）：** `npm install -g chrome-use` —— 仍在发布，但 GitHub Releases 现在是主渠道。
</details>

### 安装 AI agent skills

```bash
npx skills add leeguooooo/chrome-use
```

把 `skills/chrome-use` 拉进当前项目，让你的 AI agent 拿到正确的用法和预授权的 bash 权限。

## 命令名

`chrome-use`、`chrome-use`、`abs` 是**同一个二进制** —— `abs` 只是短别名。没有单独的「隐身可执行文件」；隐身是**运行时行为**（见下方 [反检测](#反检测)），根据你是连接真实 Chrome 还是 `--launch` 全新实例自动启用。

## 连接你的 Chrome

**推荐 —— 浏览器扩展（一键，无弹窗）。** 从 Chrome 应用商店安装 [**chrome-use** 扩展](https://chromewebstore.google.com/detail/chrome-use/knfcmbamhjmaonkfnjhldjedeobeafmk)，再注册一次本地桥：

```bash
chrome-use extension install      # 注册原生消息 host（一次性）
chrome-use open https://x.com/home
```

之后 `chrome-use open` 就通过**原生消息**驱动你真实、已登录的 Chrome —— 无调试端口、无 token、**永远不弹 "Allow remote debugging?"**。扩展自动更新、重启不掉，零确认（适合无人值守 / agent 场景）。

<details>
<summary>备选 —— 裸 remote-debugging 端口（会弹同意框）</summary>

不装扩展时，chrome-use 退回用 CDP 连接，而 Chrome 只在带 remote-debugging 端口启动时才暴露它：

```bash
# macOS
open -a "Google Chrome" --args --remote-debugging-port=9222
# Linux
google-chrome --remote-debugging-port=9222
# Windows: 给 Chrome 快捷方式 target 加 --remote-debugging-port=9222
```

然后 `chrome-use open <url>` 自动发现端口。首次连接 **Chrome 136+ 会弹 "Allow remote debugging?"** —— 点一次 Allow（该 Chrome 会话内持续有效）。上面的扩展则完全避开这个框。
</details>

## 用法

```bash
# 连接你的 Chrome 并导航
chrome-use open https://example.com

# 一切都在你已登录的浏览器里进行
chrome-use click "Post"
chrome-use fill "Title" "Hello World"
chrome-use screenshot ./page.png
```

Agent 在你的 Chrome 里操作 —— 你能实时看到开标签、加载、点击。任意时刻都能接管（比如手动过验证码），然后让 agent 继续。

### 独立模式（`--launch`）

```bash
# 临时：全新空 profile —— 无 cookie 无登录（适合 CI / 测试）
chrome-use --launch open https://example.com

# 保留登录：用你真实的 Chrome profile 启动
chrome-use --launch --profile auto open https://x.com/home
# 或显式指定：--profile Default / --profile "Profile 1"
```

## 站点适配器 —— 把一个网站变成「结构化数据 CLI」

大多数「读 GitHub issue」「搜 Reddit」「拉我的 B 站动态」这类任务，根本不需要点击 +
截图 —— 网站登录态背后本来就有 JSON 接口。**站点适配器**就是一小段 JS 函数，它在你
**已登录的标签页内**调用那个接口（用你的 cookie、同源 `fetch`、网站自己的模块），返回
干净的 JSON。网站分辨不出它和你的区别，因为它**就是你**。

chrome-use 本身不附带任何适配器 —— `site update` 会在运行时拉取社区的
[**bb-sites**](https://github.com/epiral/bb-sites) 适配器包（就像包管理器拉依赖），
然后在 chrome-use 的隐身通道上运行它们：

```bash
chrome-use site update                          # 拉取适配器包（约 145 条命令）
chrome-use site list                            # github/issues、reddit/search、bilibili/feed…
chrome-use site info github/issues              # 查看某个适配器的参数 + 域名

# 运行一个 —— 会导航到对应站点（已在该站点则复用当前标签页）并返回 JSON
chrome-use site github/issues epiral/bb-browser --json
chrome-use site reddit/search "rust async" --json
chrome-use site bilibili/feed --json            # 能用，因为走的是你的登录态
```

位置参数按适配器声明的参数顺序填入；`--key value` 按名覆盖。适配器由 bb-sites 社区编写、
版权归各自作者所有 —— chrome-use 只负责运行它们。

## 自动化测试（`chrome-use test`）

把反复的「打开它、点一圈、看对不对」变成**可重跑的测试套件** —— 前端的单元测试。用 YAML 写用例；步骤复用 chrome-use 自己的命令，断言编译成一次检查：

```yaml
# smoke.yaml
suite: chatgpt smoke
setup:
  - account: chatgpt/huayue          # 注入一个 cookie-use 登录（可选）
cases:
  - name: home loads logged in
    steps:
      - open: https://chatgpt.com/
      - wait: { load: networkidle }
    assert:
      - url: { contains: chatgpt.com }
      - visible: "#prompt-textarea"
```

```bash
chrome-use test smoke.yaml                     # 启动隔离浏览器，跑用例
chrome-use test smoke.yaml --session default   # …或对你已连接的 Chrome 跑
```

```
suite: chatgpt smoke  (session cu-test)
  ✓ home loads logged in   1.2s
  ✗ composer takes text    0.8s
      assert text "#prompt-textarea" contains "hi" → got ""
      ↳ cu-test-artifacts/composer-takes-text.png
2 cases · 1 passed · 1 failed
```

任一用例失败时退出码非零（可直接丢进 CI），失败用例会存截图。断言：`url` · `visible` · `hidden` · `text` · `count` · `eval`。步骤：`open` · `click` · `fill` · `type` · `press` · `wait` · `scroll` · `eval`。完整指南：`chrome-use skills get test`。发现回归？加个用例 —— 用得越多，套件越值钱。

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

CreepJS 上的 `0% stealth` 是关键数字：因为连接路径**什么都不打补丁**，根本没有可供说谎检测器抓的 override。（读 `navigator.languages` 顺序或 IP 地理位置的面板可能给个软性的「navigator」/「location」标记 —— 那反映的是*你真实 Chrome* 的语言列表和网络，不是自动化破绽。）

`--launch` 独立模式（全新浏览器）会改用一整套隐身补丁，也能过上述检测 —— 唯一例外：CreepJS 报 **~20% stealth**，因为 srcdoc-iframe 的 `contentWindow` 补丁触发了它的 `hasIframeProxy` 探测（用来藏自动化的 proxy 本身成了破绽）。其余全干净（`0% headless`、sannysoft/browserscan 全绿、Cloudflare 通过）。设 **`AGENT_BROWSER_DISABLE_IFRAME_PROXY=1`** 去掉那个补丁即可拿到干净的 **0% stealth**（代价是放弃小众的 srcdoc-iframe 遮蔽）。**扩展连接路径**（你的真实 Chrome）零 JS 注入、不受影响 —— 它才是货真价实的 0% 路径。

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

### 自己验证

别光听我们说 —— 把你连接的 Chrome 指向最硬的公开检测器,自己对比:

- **[CreepJS](https://abrahamjuliot.github.io/creepjs/)** —— 最全面的指纹 / 说谎检测器
- **[bot.incolumitas.com](https://bot.incolumitas.com/)** —— 行为 + 指纹打分,方法公开
- **[BrowserScan](https://www.browserscan.net/bot-detection)** —— Webdriver / User-Agent / CDP / Navigator
- **[bot.sannysoft.com](https://bot.sannysoft.com)** —— 经典自动化特征清单
- **[pixelscan.net](https://pixelscan.net/)** · **[iphey.com](https://iphey.com/)** —— 一致性与身份

我们故意**不自带 bot 检测器** —— 最强、最诚实的基准,就是拿市面上最好的检测器去测你的真实浏览器。

### 调参（环境变量）

| 变量 | 默认 | 作用 |
|---|---|---|
| `AGENT_BROWSER_CAPTURE_CONSOLE` | 关 | 启用 `Runtime` 域,让 `console` / `errors` 捕获页面输出。关闭可保持最隐身的画像。 |
| `AGENT_BROWSER_HUMANIZE` | 关 | 类人输入动作:`off`(瞬时)、`fast`(轻量缓动轨迹)、`human`(全套曲线轨迹 + 落点抖动 + 击键节奏 + 缓动滚动/拖拽)。也可用 `--humanize`。默认 `off`;自适应检测器会把 Akamai/PerimeterX/DataDome 守护的页面自动升到 `human`。 |
| `AGENT_BROWSER_TIMEZONE` | 未设 | 仅 `--launch`。IANA id(如 `Asia/Tokyo`)原生设置时区(Intl + Date 跟随,无 JS 谎言)以匹配代理;`auto` 按 locale 推导。 |
| `AGENT_BROWSER_BLOCK_WEBRTC` | auto | 仅 `--launch`。设了代理时自动强制 WebRTC 走代理(不泄漏真实 IP)。`1` 无代理时也隐藏本地 IP;`0` 退出。 |
| `AGENT_BROWSER_HIDE_CANVAS` | 关 | 仅 `--launch`。加入会话稳定的 canvas/audio 指纹噪声。默认关(噪声本身就是一种「谎言」)。 |
| `AGENT_BROWSER_ADAPTIVE_REF` | 开 | 当保存的 `@ref` 移动且 role/name 重查失败时,按指纹相似度重定位(需高分 + 明显领先,否则明确报错)。`0` 关闭。 |
| `AGENT_BROWSER_CLICK_MODE` | _(auto)_ | 点击策略。默认先滚动入视、派发坐标点击,若被浮层遮挡则回退 DOM `.click()`。`dom` 始终用 `.click()`(适合 blur 即关的自动补全/菜单项);`coord` 严格只用坐标(遮挡时硬失败)。 |

## chrome-use 的独特之处

- **默认 auto-connect** —— `chrome-use open` 连你现有的 Chrome 而非启新的
- **扩展中继传输** —— 一键安装的 Chrome 商店扩展 + 原生消息，无调试端口、无 "Allow remote debugging?" 弹框
- **CDP 原生隐身** —— 反检测走 Chrome/CDP 覆盖而非 JS 补丁；连真实 Chrome 零补丁，仅 `--launch` 用全补丁
- **Humanize** —— 类人光标轨迹 + 自适应反爬处理
- **多 agent 隔离** —— 多个 agent 通过 per-session 标签组共享同一个真实 Chrome，互不串扰
- **静默运行** —— 后台操作，绝不抢你的前台标签

<sub>最初基于 [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser)（Apache-2.0）；两个项目已大幅分化。</sub>

## License

Apache-2.0

---

> 由 **leeguooooo** 打造 —— AI agent、逆向工程与 Cloudflare Workers 的实战笔记见 **[blog.misonote.com](https://blog.misonote.com)** · 关注 **[X @leeguooooo](https://x.com/leeguooooo)**
