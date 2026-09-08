# 扩展恢复与重复动作：候选验证记录

日期：2026-09-08。目标仍是改善真实任务完成率、恢复体验和观察成本，最终通过重复对照验证是否优于 Codex 内置浏览器工具。本记录只覆盖其中一批候选修复，不表示目标已完成。

## 运行环境

- 候选基础提交：`b854d18f`，分支 `codex/reliable-browser-actions`。
- CLI 与原生主机：本机已安装的 chrome-use 1.5.106。
- 扩展：从独立工作树加载的 ab-connect 0.5.21，包含候选源码；未更新共享 Chrome 的扩展或发布商店版本。
- 浏览器：Chrome for Testing 152.0.7977.82，独立临时 profile。
- 创建浏览器的会话使用直连 CDP，仅负责测试环境及扩展自身页面；购物流程使用另一个明确绑定测试 profile 的 CLI session，经过候选扩展和原生消息主机。
- 未控制预热、固定视口或进行多次速度对照，因此以下记录不用于宣称提速。

原生消息主机应注册在临时 user-data-dir 的 `NativeMessagingHosts` 下。Chrome 官方文档说明了自定义 profile 与默认安装位置的区别：[Native messaging host location](https://developer.chrome.com/docs/extensions/develop/concepts/native-messaging#native-messaging-host-location)。测试配置没有增加原生主机的 allowed origins，也没有覆盖真实 Chrome 的配置。

## 自动化检查状态

- 扩展测试串行运行 61 项通过，包含重复提交、受限子框架、恢复目标及别名清理回归。
- 默认并发运行曾有一个既有 tab-duplicate 200ms 测试超时；随后串行全量通过。尚未单独证明该超时的根因。
- Rust 格式检查通过。实际 `error_envelope.rs` 模块独立编译后，4 项测试通过，包括 unknown 状态不被覆盖成可重试错误；这不替代完整 CLI 单元测试。
- 初次 Rust 编译因磁盘空间不足终止，没有得到测试结论。仅清理此工作树的构建产物后，已关闭调试符号与增量缓存、单任务重新编译；重试已完成：1227 项通过、0 失败、3 项忽略。
- 浏览器测试结束后关闭了本次创建的测试会话和 fixture 服务。清理时本机 CLI 已变为 1.5.107，并提示 daemon 版本不一致；不把清理命令算入前面的任务调用。

## 购物流程

候选扩展通过中继完成 SauceDemo：登录，按价格升序排序，核实商品价格，加入最便宜两件，进入购物车，填写测试信息，读取金额，再 Finish。

- 无名排序控件一次 select 成功。
- 最便宜两件为 Onesie 7.99 与 Bike Light 9.99，各一件。
- 汇总页读取 Item total 17.98、Tax 1.44、Total 19.42。
- Finish 后读取到完成页和订单成功提示。
- 共 18 次任务 CLI 命令。含一次使用旧购物车引用被拒绝，以及一次额外完成页正文确认；环境准备与诊断另计。
- 旧引用来自执行者在加购后仍使用之前的引用，是执行错误。工具拒绝后，用观察结果中的新引用继续成功，未重新加载网站来绕过。

这个结果证明候选在独立扩展中继环境能完成任务。共享真实 Chrome 中的 #217 尚未重测，不能由此宣称已修复；也没有同环境旧扩展的完整流程对照。

## 已执行动作的回包丢失

使用 [本地计数页面](../fixtures/action-outcome.html) 和 [扩展内探针](../fixtures/action-outcome-probe.js)。探针通过真实 `chrome.debugger.sendCommand` 点击页面按钮，等待成功后主动抛出 `Detached while handling command`，模拟回包丢失。页面计数反映真实执行次数。

| 实现 | 实际页面提交数 | 发送次数 | 结果 |
|---|---:|---:|---|
| `b854d18f` 的原恢复逻辑 | 2 | 2 | 重复提交，返回普通 detached 错误 |
| 本次禁止不确定动作重放的候选 | 1 | 1 | 返回 `action_outcome_unknown`，没有恢复或重发动作 |

这不是浏览器自发断连的复现，也没有跑完整业务 API；它验证的是：动作已执行后，即使调用方收到断连错误，恢复策略也不会把页面动作执行两次。

Rust 侧将该标记映射为 `code: action_outcome_unknown`、`retryable: false`，并保留原始说明，不用通用 stale-target 恢复提示覆盖它。这条 Rust 输出路径需要单独编译与单元验证，已安装 CLI 的版本不包含这一改动。

## 子框架与父页面

对候选模块注入失效子 session，真实 Chrome 返回 `Session with given id not found`。模块未调用父标签解绑或恢复，随后父页面读取成功。

这一浏览器错误与原回归中的受限扩展 iframe 错误不同，不能作为受限 iframe 故障的完整复现。受限子框架错误、恢复到替代 tab ID、旧 session 别名和无关 session 保留，目前有确定性扩展单元测试覆盖。

## 新发现，尚未修复

1. 原生主机不存在时，扩展弹窗曾显示 Connected；直接连接探针返回 `Specified native messaging host not found`。候选已改为收到主机实际回应后才确认，见下节。
2. `--observe` 的请求列表完整输出 data URL，包括长 SVG 和 base64 图片。此问题已在下一批候选中增加有界摘要，验证见下节。
3. 加购改变购物车 accessible name 后，引用变更需要显式跟随观察结果；错误文案却概括为每次 snapshot 都重新编号，与稳定引用说明不一致。

## 请求摘要与文本观察

下一批候选在生成 `observed.requests` 时最多保留 20 条摘要，每条最多 256 个 UTF-8 字节。data URL 的载荷不进入观察结果；JSON 附总数、省略条数和缩短条数。专用 `network requests --json` 保留原捕获记录。

- 模块实测：420,026 字节的合成图片请求行变为 64 字节摘要。
- 真实共享 Chrome：本地 fixture 不改变 DOM 和 URL，只发出 25 个 data 请求。候选 JSON 返回 `changed:false`、总数 25、省略 5、缩短 20，实际显示 20 条摘要，整份回复 1,501 字节。
- 两遍 probe 后，专用命令返回 50 条完整 data 请求，每条 URL 超过 420,000 字节；摘要没有修改 tracker 中的原记录。
- 文本首测发现 `eval --observe` 仍会因专用渲染分支提前返回而丢掉观察；现已统一在响应正文之后输出观察，并增加 eval、check、click、navigation 和 JSON 的子进程回归。测试也检查警告只输出一次、JSON 没有混入文本。
- 原推荐命令 `requests --json` 在实际 CLI 中无效，文档和提示已更正为 `network requests --json`。
- 此批完整 Rust 单元测试 1232 项通过、0 失败、3 项忽略；候选二进制构建后，真实 CLI 文本复核通过：25 个请求的响应为 1,374 字节，包含无页面变化提示、请求摘要及省略数量。

## 原生主机连接状态

候选扩展新增 connecting/connected/disconnected 状态。创建 Port 时只进入 connecting；收到主机 pong 或兼容旧主机的实际命令后才确认连接。旧 Port 的迟到消息和断开回调不能覆盖新连接。弹窗通过状态推送更新，单独打开 HTML 预览也不再伪造连接或标签数量。

在另一个独立 Chrome for Testing 临时 profile 中验证：

- 未注册原生主机：弹窗显示 Not paired 和 `Specified native messaging host not found.`，提示安装本地主机。
- 只在临时 profile 注册候选原生主机后：弹窗显示 Connected、`local CLI connection confirmed`；状态接口确认 connected，并通过这条中继打开 Example Domain。
- 终止这个测试主机：已打开的弹窗及时变为 Not paired 和 `Native host has exited.`，提示 `chrome-use status`，没有要求重装。
- 移除临时注册文件不会终止已经连接的主机。因此缺主机复查先终止测试主机，再发起新连接，才验证“未找到主机”的状态；没有把已有连接仍可用误判成修复失败。
- 测试结束后恢复临时注册、关闭测试 session 与测试浏览器，没有修改共享 Chrome 的原生主机配置。

扩展测试 70 项通过。Rust 完整套件 1233 项通过、0 失败、3 项忽略；候选 CLI 构建通过。连接状态只确认本机通信链路，不替代页面可读性或任务完成验证。

## 多标签与受限框架验收

候选已合入上游 1.5.107，扩展候选版本为 0.5.22，尚未发布。

新增的伴随扩展只在本地 `127.0.0.1` 测试页注入另一个扩展的 iframe。真实 Chrome 152 拒绝父标签的 `Runtime.evaluate`、`Page.getFrameTree`，连 attach/detach 也返回同样的扩展访问限制。浏览器级目标查询仍显示正确的 HTTP 页面，原生桌面 AX 也看得到测试 iframe。

这证明本场景不是目标被转移到别的标签。Chromium 的 [debugger 权限检查](https://github.com/chromium/chromium/blob/main/chrome/browser/extensions/api/debugger/debugger_api.cc) 会检查 frame 树，与实测一致；不能因此断言此前每个 detached 错误都是这个原因。

候选现将该情况报告为 `debugger_access_denied`、`retryable:false`，避免无效重连和“标签丢失”的误导。私有环境中导航约 28 秒后返回该错误，后续 snapshot 约 1.4 秒返回同一错误；`tab list` 和 `tab inspect` 均成功。导航仍会先耗尽生命周期等待，这是待改进项，不是快失败验收通过。

### 一次受干扰的测试环境

最初用普通 `--launch` 控制器创建临时浏览器，并将其 relay 登记到默认目录。后来出现非本任务的标签，扩展 reload 后控制器也将原测试页作为了后续导航目标。此轮不计通过。停止整窗操作后再次检查，控制器、临时浏览器和 profile 已退出/清理，未能保留外部标签；退出触发没有被确认，不能把原因写成已证实的用户操作或 idle 回收。

为防止重演，新增 `CHROME_USE_RELAY_DIR`，让本地主机和 CLI 使用同一个独立登记目录。新的 fixture 启动器不开调试端口，不由会自动回收浏览器的 CLI launch daemon 持有生命周期。实际核实：私有目录只有一个测试 profile，默认中继列表不包含它，进程参数中没有远程调试端口。

### 私有环境重跑

- 两会话检查通过：Beta 计数为 3、Gamma 为 0，Beta 导航后 Gamma 标题不变。另一个会话的标签没有暴露给 Beta，隔离在发现阶段生效。14 条 CLI 命令包含清理。
- 使用内置 CUA 的原生应用接口把受限测试页置于前台，再跑同一检查，仍通过。结束后原生 AX 确认前台仍是受限页；这两次不是速度对照，原生 UI 操作单独用于模拟用户切换。
- 受限页测试的成功条件是明确拒绝而不误报目标丢失，不能称作已能驱动受限页面。
- 首次脚本因 macOS socket 路径过长失败，改用短的私有 socket 目录；随后脚本错误地要求另一会话必须作为 foreign 行出现，修正为接受更早的发现隔离。两次失败保留，不能算作工具通过。
- 私有 fixture 会话、测试浏览器和本地 HTTP 服务已关闭。

## 当前候选检查与打包

- 完整 Rust 测试：1237 通过、0 失败、3 忽略。
- 扩展测试：71 通过。
- 新增私有登记目录的子进程回归，以及实际私有目录与默认目录的发现隔离检查。
- 0.5.22 ZIP 与签名 CRX 均已打包，检查了版本、所有新增模块以及私钥排除；上传 ZIP 不含 manifest key 或测试文件。扩展权限列表没有增加，伴随的受限框架 fixture 不在发布包内。
- 包仍是候选，未上传商店、未创建 Release、未替换用户真实 Chrome 的已安装扩展。

## 验收仍欠缺

- 将候选加载到可控的共享真实 Chrome 测试条件下，重测导航后继续驱动。
- 真正的 tab 替换与更多 profile 场景；本地受限框架拒绝、多 session 隔离已按上节验证。
- 已执行但回包失败场景的完整 CLI 错误输出与非重试约束验证。
- 连接状态和请求摘要已完成上述测试，还需发布后的安装版本验证。
- 固定版本、profile、页面初始状态和预热条件，交换执行顺序并重复同人对照。
- 打包、合并和发布后的版本及真实行为验证。
