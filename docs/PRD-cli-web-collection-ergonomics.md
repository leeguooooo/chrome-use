# PRD: CLI Web 数据采集体验优化（以小红书场景为例）

- 文档版本: v0.1
- 状态: Draft
- 作者: Codex
- 日期: 2026-03-04

## 1. 背景与问题

在使用 `agent-browser` CLI 执行「小红书宠物博主采集（100 条）」时，当前流程可完成任务，但存在明显的可用性与稳定性痛点：

1. 网络层可观测性不足，响应体抓取不稳定，需注入脚本劫持。
2. 分页采集依赖手工 `scroll down + wait`，重复劳动且易漏数据。
3. 结构化导出缺少一站式命令，需要 `eval` 二次解析。
4. 页面交互依赖文本选择，页面文案变动后脆弱。
5. 反爬失败时缺少可解释的自动回退策略。
6. 用户对“可抓字段”预期不清（例如搜索接口无联系方式）。
7. 长会话缺少快照与断点续抓机制。

## 2. 目标与非目标

## 2.1 目标

1. 将常见采集链路从“脚本拼接”降为“CLI 原生命令组合”。
2. 让关键动作具备可观测性（日志）和可恢复性（快照/续跑）。
3. 降低站点轻微改版、反爬限制带来的失败率。

## 2.2 非目标

1. 不承诺绕过平台强风控或登录体系。
2. 不在本期实现完整通用爬虫 DSL。
3. 不默认抓取平台未公开展示的隐私字段。

## 3. 目标用户与核心场景

1. 增长/运营: 按关键词采集账号基础数据并导出 CSV。
2. 测试/研发: 复现抓取问题，定位请求失败原因。
3. AI Agent 工作流: 在 CLI 内稳定执行“搜索 -> 翻页 -> 提取 -> 导出”。

## 4. 需求范围与优先级

## 4.1 P0

1. `network capture` 增强模式（可过滤、可落盘 response body）。
2. `scroll-collect` 自动滚动采集（按页数或直到无新增）。
3. `extract` / `extract-to` 结构化导出（JSON/CSV）。

## 4.2 P1

1. 语义选择器与 fallback 链（role/aria/data/text）。
2. 401/403/406 智能回退（页面触发 + 回包监听）。
3. 可抓字段矩阵与二段式采集文档提示。

## 4.3 P2

1. `session snapshot` + `crawl resume` 断点续抓。

## 5. CLI 方案设计

## 5.1 网络捕获增强

命令草案:

```bash
agent-browser network capture --match '/api/sns/web/v1/search/usersearch' --save ./out.ndjson
agent-browser network capture --domain edith.xiaohongshu.com --method POST --save ./xhs_usersearch.ndjson
```

参数:

- `--match <regex>`: 按 URL 正则过滤。
- `--domain <host>`: 按域名过滤。
- `--method <GET|POST|...>`: 按方法过滤。
- `--status <code|range>`: 按状态过滤。
- `--save <path>`: NDJSON 输出文件。
- `--include-body <request|response|both>`: 控制 body 输出范围。
- `--max-body-bytes <n>`: 单条 body 截断阈值。

NDJSON 记录结构:

```json
{
  "ts": "2026-03-04T10:00:00.123Z",
  "session_id": "sess_abc",
  "request_id": "req_123",
  "method": "POST",
  "url": "https://edith.xiaohongshu.com/api/sns/web/v1/search/usersearch",
  "status": 200,
  "duration_ms": 312,
  "request_headers": {"content-type": "application/json"},
  "request_body": "{...}",
  "response_headers": {"content-type": "application/json"},
  "response_body": "{...}",
  "truncated": false
}
```

## 5.2 自动滚动采集

命令草案:

```bash
agent-browser scroll-collect --until no-new-items --max-steps 200 --idle-rounds 3
agent-browser scroll-collect --pages 20 --wait-ms 1200
```

行为:

1. 每轮执行滚动与等待。
2. 基于 DOM 项数量或网络新增请求判断“是否有新增”。
3. 达到停止条件后输出结束原因。

输出示例:

```text
step=1 new_items=15 total_items=15
step=2 new_items=15 total_items=30
...
stop_reason=no-new-items idle_rounds=3 total_items=135
```

## 5.3 结构化提取与导出

命令草案:

```bash
agent-browser extract --from network --match usersearch --fields 'name,fans,note_count,red_id'
agent-browser extract-to --from network --match usersearch --fields 'name,fans,note_count,red_id,url' --format csv --out ./users.csv
```

参数:

- `--from <network|dom|eval>`: 数据源。
- `--match <pattern>`: 来源过滤（URL/事件名）。
- `--query <JMESPath|JSONPath>`: 自定义提取表达式。
- `--fields <a,b,c>`: 字段映射快捷写法。
- `--dedupe-by <field>`: 去重键。
- `--limit <n>`: 限制条数。
- `--format <json|ndjson|csv>`: 输出格式。
- `--out <path>`: 文件输出路径。

## 5.4 语义选择器与回退链

命令草案:

```bash
agent-browser click --selector 'role=tab[name="用户"]' --fallback 'aria=用户,text=用户'
agent-browser find --selector 'data-testid=user-tab' --fallback 'role=tab[name="用户"],text=用户'
```

策略:

1. 主选择器失败后按 fallback 顺序重试。
2. 日志打印每次尝试与失败原因。

## 5.5 反爬失败自动回退

命令草案:

```bash
agent-browser request replay --on-status 401,403,406 --fallback page-action
```

策略:

1. 直接请求失败后自动回退到页面行为触发。
2. 自动复用 UA/Referer/Cookie Jar。
3. 捕获最终有效响应并给出“回退成功/失败”日志。

## 5.6 会话快照与断点续抓

命令草案:

```bash
agent-browser session snapshot save ./snapshots/xhs-20260304.json
agent-browser crawl resume --snapshot ./snapshots/xhs-20260304.json --out ./users.csv
```

快照最小字段:

- 当前 URL
- 关键词/筛选参数
- 已抓 user_id 集合摘要（可哈希分片）
- 分页进度（page/scroll step）
- 导出配置（fields/format/out）

## 6. 错误码设计（草案）

- `AB_NET_CAPTURE_BODY_UNAVAILABLE` (1001): 响应体不可用（被浏览器策略阻断或已释放）。
- `AB_SCROLL_TIMEOUT_NO_PROGRESS` (1101): 滚动超时且无新增。
- `AB_EXTRACT_QUERY_INVALID` (1201): 提取表达式语法错误。
- `AB_EXTRACT_OUTPUT_FAILED` (1202): 导出失败（权限/路径不可写）。
- `AB_SELECTOR_NOT_FOUND` (1301): 主选择器与 fallback 全部失败。
- `AB_REQUEST_BLOCKED_406` (1406): 请求被风控拦截，且回退链路失败。
- `AB_RESUME_SNAPSHOT_INVALID` (1501): 快照损坏或版本不兼容。

要求:

1. CLI 退出码与错误码可映射。
2. 错误输出提供 `hint`（下一步建议命令）。

## 7. 日志与可观测性

默认人类可读，开启 `--log-format json` 输出结构化日志。

JSON 日志字段:

- `ts`
- `level`
- `session_id`
- `command`
- `event`
- `step`
- `url`
- `status`
- `error_code`
- `message`
- `hint`

示例:

```json
{"ts":"2026-03-04T10:11:22.123Z","level":"INFO","command":"scroll-collect","event":"step","step":12,"new_items":15,"total_items":180}
{"ts":"2026-03-04T10:13:01.001Z","level":"WARN","command":"request replay","event":"fallback","status":406,"message":"direct request blocked, fallback to page-action"}
```

## 8. 文档与帮助信息更新要求

当功能落地时，需要同步更新以下位置（按仓库规范）：

1. `cli/src/output.rs`（`--help`、示例、环境变量）
2. `README.md`（命令选项、样例）
3. `skills/agent-browser/SKILL.md`（Agent 工作流）
4. `docs/src/app/`（新增/更新 MDX 页面，表格使用 HTML `<table>`）
5. 对应源码内联注释

## 9. 验收用例（首批）

1. `network capture` 能稳定保存目标接口完整 request/response body。
2. 设置 `--max-body-bytes` 后被截断记录带 `truncated=true`。
3. `scroll-collect --pages 5` 精确执行 5 轮并退出。
4. `scroll-collect --until no-new-items` 在连续空增量 N 轮后退出。
5. `extract-to ... --format csv` 产出可打开 CSV 且列名正确。
6. `extract --dedupe-by user_id` 去重结果稳定。
7. selector 主规则失败时，fallback 生效并成功点击。
8. 对 406 场景触发自动回退并成功捕获有效响应。
9. 回退失败时返回 `AB_REQUEST_BLOCKED_406` 且提供 hint。
10. `session snapshot save/load` 前后任务可恢复。
11. `crawl resume` 不重复导出已抓 ID。
12. `--log-format json` 日志字段完整，便于机器消费。

## 10. 里程碑建议

1. M1（1 周）: `network capture` + `scroll-collect`。
2. M2（1 周）: `extract-to` + selector fallback。
3. M3（1 周）: 406 回退链路 + 文档补全。
4. M4（1 周）: snapshot/resume + 稳定性打磨。

## 11. 风险与缓解

1. 平台策略变化导致规则失效。  
缓解: 增加站点适配层与策略开关，保留回退日志。
2. 响应体过大带来内存与 IO 压力。  
缓解: 流式写入 NDJSON + 截断阈值。
3. 通用提取表达式学习成本高。  
缓解: 提供字段模板与场景 presets。

## 12. 开放问题

1. `extract` 表达式标准优先 JSONPath 还是 JMESPath？
2. `session snapshot` 是否需要加密（含 cookie 元信息）？
3. 是否提供站点模板（如 `preset xiaohongshu-user-search`）以降低上手成本？
