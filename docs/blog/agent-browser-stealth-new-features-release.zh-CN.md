# agent-browser-stealth 新功能发布

这次我们对 `agent-browser-stealth` 做了一次完整升级：  
从“标签分组辅助”升级为“可控制、可观测、可编排”的 AI 浏览器控制台。

## 这次升级解决了什么问题

过去插件主要解决会话隔离和分组管理。  
在真实自动化场景中，仍有三个核心缺口：

1. 缺少侧边栏直接控页能力
2. 缺少执行过程可观测性（console/network/DOM）
3. 缺少可复用流程（workflow/shortcut/schedule）

本次发布一次性补齐这三块。

## 新增能力一：侧边栏浏览控制

现在你可以在 side panel 直接完成页面控制：

- `open / back / forward / reload`
- 按 CSS 选择器执行 `click / fill / press`
- 标签页切换与关闭
- 直接运行快捷指令（slash shortcut）

这让“流程启动”和“人工微调”可以在同一界面完成。

## 新增能力二：开发者观测能力（Developer Signals）

我们新增了调试信号面板，避免黑盒执行：

- 页面 `console` 事件（含 error/warn）
- `fetch/xhr` 网络事件
- 命令历史
- DOM 状态快照（文本预览、交互元素、mutation 摘要）

当流程失败时，可以快速判断是页面结构变化、网络问题还是动作配置问题。

## 新增能力三：Workflow 自动化体系

插件现在支持完整自动化闭环：

- 录制：`Start -> Stop -> Save`
- 回放：运行 workflow
- 复用：绑定 slash shortcut
- 调度：`daily / weekly / monthly / yearly`

调度任务会记录 `lastRunAt / nextRunAt`，并把执行结果写入活动流，便于排查和审计。

## 已完成实测：workflow + abs 接管流程

我们验证了一个非常实用的流程：

1. 先用 workflow 进入目标页面
2. 在关键节点停顿（checkpoint）
3. 让 AI 使用 `abs` 完成上传/填写等细操作
4. 停在发布前，等待人工确认

这个模式已经在小红书发布场景中跑通：  
进入发布页、上传图片、填写标题正文，且默认不自动点击“发布”。

## 兼容性与稳定性改进

这次还修复了几个关键稳定性问题：

- 修复 side panel `activeTab` 空值崩溃
- 增加面板状态归一化兜底，兼容热更新与旧返回结构
- 修复侧边栏渲染的转义问题，避免内容显示异常

## 总结

`agent-browser-stealth` 现在不只是“分组插件”，而是一个真正可用于生产流的浏览器执行层：

- 可控：侧边栏直接控页 + 分段执行
- 可观测：console/network/DOM 全链路可见
- 可复用：workflow + shortcut + schedule

如果你在做 AI 浏览器自动化，这次升级能直接提升流程稳定性与迭代效率。
