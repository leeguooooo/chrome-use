# agent-browser 与 agent-browser-stealth：能力差异与选型

本文给出 `agent-browser` 与 `agent-browser-stealth` 的技术差异、适用场景和升级验证步骤。

项目地址：[leeguooooo/agent-browser](https://github.com/leeguooooo/agent-browser)

---

## 1. 定位差异

- `agent-browser`：标准浏览器自动化能力
- `agent-browser-stealth`：在标准自动化能力基础上，增加反检测与高风控场景稳定性能力

---

## 2. 核心能力对比

| 维度 | agent-browser | agent-browser-stealth |
| --- | --- | --- |
| 自动化基础能力 | 支持 | 支持 |
| 指纹一致性治理 | 基础 | 多层（launch/CDP/init-script） |
| 高风控站点稳定性 | 一般 | 更高 |
| 会话连续性（附着现有浏览器） | 支持 | 支持，默认附着策略更明确 |
| Cloudflare/Turnstile 回归工具 | 无专用脚本 | `check:turnstile-testkey` |

---

## 3. Cloudflare/Turnstile 相关能力（v0.15.2-fork.2+）

### 3.1 挑战链路保护

- 同源 worker 注入保留
- 跨域 challenge worker 不做注入改写
- 降低 challenge worker 执行异常概率

### 3.2 导航等待策略

`open/navigate` 支持：

- `--wait-until load`
- `--wait-until domcontentloaded`
- `--wait-until networkidle`

挑战页建议优先 `domcontentloaded`，减少 `load` 阶段超时误判。

### 3.3 确定性回归

提供官方 test key 回归脚本：

```bash
pnpm run check:turnstile-testkey
```

通过特征：输出 `XXXX.DUMMY.TOKEN.XXXX`。

---

## 4. 适用场景

优先使用 `agent-browser-stealth` 的场景：

1. 目标站点存在挑战页/验证码/限流
2. 自动化链路对稳定性要求高
3. 需要长期回归验证与版本门禁

使用 `agent-browser` 的场景：

1. 低风控站点
2. 以基础自动化能力验证为主

---

## 5. 升级验证步骤

```bash
# 1) 检查版本
agent-browser -V

# 2) 关闭旧 daemon，避免版本漂移
agent-browser --session default close

# 3) 运行确定性回归
pnpm run check:turnstile-testkey

# 4) 可选：真实站点回归
agent-browser --wait-until domcontentloaded open https://www.anyviewer.com/cloudflare.html
```

如果启用域名白名单（`AGENT_BROWSER_ALLOWED_DOMAINS`），需包含 `challenges.cloudflare.com`。

---

## 6. 结论

`agent-browser-stealth` 适用于高风控与稳定性敏感场景；`agent-browser` 适用于标准自动化场景。  
选型建议按目标站点风控强度与回归要求决定。

