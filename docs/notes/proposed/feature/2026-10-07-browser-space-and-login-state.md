# Decision Note: 浏览器 Space 与登录态（N2 立项）

Status: proposed — 承接 [批次 N 立项](../architecture/2026-10-01-agent-browser.md) 的 N2。
10-04 批次评审把它保留为唯一可独立实施的一档：不依赖已拒的 PTC 执行器，收益明确，
且不扩大攻击面（不开heredoc 执行面）。

## Decision

立项两个能力，合为一个交付：

1. **Space = 隔离的浏览器工作区**。独立模式下是独立 profile 进程，attach 模式下是
   专属 tab 组。语义对齐 ego-browser 的 `useOrCreateTaskSpace('name')`。
2. **attach 用户日常浏览器**（`--remote-debugging-port` 一次性引导）。登录态自然
   继承，agent 只动专属 tab，绝不碰用户已开的 tab。

## 为什么现在做

**登录态是当前最实际的缺口。** 今天的浏览器自动化由 `cdp_bootstrap` 拉起浏览器。
实测（2026-10-07，`env_browser_cua.rs::t20/t21`）修正了这里原先的说法：

- bootstrap 起的**不是临时 profile**，而是 Hermix 专有的持久 profile
  `~/.hermix/runtime/browser-profile`（命令行显式 `--user-data-dir=`）。
- 该 profile 里的 cookie **跨浏览器进程重启保留**：设置 → 优雅关闭 → 重新拉起 →
  再次访问同一站点仍然带上 cookie（t20 端到端验过）。
- 因此「每次都要重新登录」不成立；**真正的缺口是首次**：它不继承用户日常 Edge 的
  登录态，所以每个站点第一次仍要人工过一遍登录。

所以 Space 要解决的从「重登」收缩为「首次登录 + 任务之间互相污染」。后者是真的：
今天一次自动化任务留下的 tab、cookie、页面状态全部带到下一次，
`useOrCreateTaskSpace` 让「这次任务的浏览器世界」与「上一次」隔离。

## Alternatives considered

- **不 attach，自建登录态接管** —— 否决：等于自己实现一套 cookie 注入 + CSRF 处理，
  比继承用户已登录的浏览器脆弱得多，且用户无法审查自己在哪个站点交出了什么。
- **放开 CDP 端口到 0.0.0.0 以支持远程** —— **否决，且是硬边界**。CDP 端口等于
  浏览器全权控制权（读cookie、执行任意页内 JS、发起任意请求）。绑 127.0.0.1 是
  本机安全敏感面，不为便利性放宽。ego-lite 未披露其安全设计，也不构成放宽的理由。
- **引入 Playwright/Puppeteer 托管浏览器** —— 否决（同批次 N1 评审）：现有 CDP 底座
  已具备所需能力，且 ego 明言其读者是 agent 而非人类开发者。换个库不会让 snapshot
  更小，只会让依赖栈更重。
- **把 Space 做成完整的多任务调度器** —— 否决，超出本篇。Space 只解决「隔离」，
  「排队/并发/优先级」是调度问题，属另一议题（见 automation 批次）。

## Consequences

- 获得：登录态天然继承，常规网站自动化不必每次人工过一遍；任务间状态隔离。
- 代价：attach 要求用户配合一次重启浏览器（带调试端口）。这是明确的用户摩擦，
  不可消除——继承登录态本质上要求浏览器在带调试端口的进程里运行。
- 风险：用户日常浏览器里的所有登录态对agent 可见。这是能力也是责任：agent 能用
  用户已登录的身份操作站点，**审批链（Dangerous 档）与写操作提示必须如实反映这一点**，
  不能让用户以为agent 在一个隔离环境里。
- 风险：Space 状态泄漏（任务结束未清理）→ 会话表 TTL 兜底，与 N1 同。
- 前置：`cdp_bootstrap` 的拉起流程需要提供「引导用户带端口重启」这一路径。这是 N2 的
  第一个可交付项，其余部分依赖它。

## Reintroduction conditions

若将来决定做跨设备/远程浏览器控制，**不得**以本篇的 attach 实现为跳板放宽端口绑定
——那是另一个决策，必须独立评估暴露面。

若登录态继承被证实在实践中不可靠（例如目标站点每会话刷新 token 且要求人工干预），
本篇的 attach 分支应退回纯独立 profile 路线，并如实告诉用户「这个站点需要手工登录」，
而不是用注入手段绕过。