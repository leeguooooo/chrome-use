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
- 初次 Rust 编译因磁盘空间不足终止，没有得到测试结论。仅清理此工作树的构建产物后，已关闭调试符号与增量缓存、单任务重新编译；单元结果待完成后更新。
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

1. 原生主机不存在时，扩展弹窗曾显示 Connected；直接连接探针返回 `Specified native messaging host not found`。需要区分 port 对象创建与真正连接确认。
2. `--observe` 的请求列表完整输出 data URL，包括长 SVG 和 base64 图片。购物页面的小动作因此产生大量无关输出，需增加有界摘要。
3. 加购改变购物车 accessible name 后，引用变更需要显式跟随观察结果；错误文案却概括为每次 snapshot 都重新编号，与稳定引用说明不一致。

## 验收仍欠缺

- 将候选加载到可控的共享真实 Chrome 测试条件下，重测导航后继续驱动。
- 自动化的受限 OOPIF 实例、真实 tab 替换、不同 profile 和多个 session 隔离验收。
- 已执行但回包失败场景的完整 CLI 错误输出与非重试约束验证。
- 修复连接状态误报和观察输出膨胀。
- 固定版本、profile、页面初始状态和预热条件，交换执行顺序并重复同人对照。
- 打包、合并和发布后的版本及真实行为验证。
