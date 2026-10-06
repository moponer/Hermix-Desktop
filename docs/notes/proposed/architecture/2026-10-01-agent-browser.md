# Decision Note: 智能体浏览器（批次 N 立项，2026-10-01 二稿）—— 核心 = 浏览器能力的 PTC 化

Status: implemented — N0 + N1 已交付；N2（Space + 登录态）另立本篇 [N2 立项笔记](../feature/2026-10-07-browser-space-and-login-state.md)。N1 走**声明式步骤数组**（`PtcStep` serde 枚举，`5438a20e` + `1a07df29` 120s 预算 + 4 测试）而非本篇设想的 JS heredoc——无 Node 依赖、每步风险可静态判定。开工清单见 [开工清单](../process/2026-10-04-work-order-cua-and-automation.md)。

**2026-10-07 实测更正**：本篇把「语义 snapshot 100-400 token/页」列为核心指标，
但它从第一天起就是**推算**——N0 探针自列「精确 token 测量」为遗留项（见
[webview2-cdp-probe](../implemented/architecture/2026-10-01-webview2-cdp-probe.md)），
一直没做，于是整条路线的卖点悬空。本次用 Edge CDP 在四个真实页面实测，结论：

| 页面 | compact 后 | ego 目标 100-400 |
|---|---|---|
| github.com/login | 105 token | ✅ |
| wikipedia 文章 | 114 token | ✅ |
| 新闻首页 | 212 token | ✅ |
| example.com（只读页） | 656 token | ❌ 1.6× |

**目标基本达成**——交互密集页全部落进带内，比本篇推算的「未压缩 1.8K-3.7K」好得多
（实测未压缩仅 165-656）。唯一超标的是纯只读页面，且那里的正确解不是继续压，
而是保底不压：测量同时暴露 compact 会在交互元素稀少的页面上把正文清空
（example.com 3533 字符 → 86，七种语言正文全丢），已修（`bc8f4e41`）。

测量同时发现 30-50% 的 snapshot 字符是无信息 filler 行（`- none` / `- paragraph`
这类既无 name、无状态、非交互的结构占位），已在同一提交省去。**先省掉噪音再谈
压缩比**，否则分母本身是虚的。

原文「1.8K-3.7K token/页」一句连同其数字一并以此处实测取代。

## 背景

用户调研 ego-lite 并指出：它的本质是把浏览器能力封装成 **agent 可直接调用的
类型化 JS 函数**，让 agent 发挥最擅长的本事——写代码，把多步骤任务组合成
单次输出。官方文档（lite.ego.app ego-browser 页 + 产品介绍）证实：

- **单次往返执行**：`ego-browser nodejs <<'EOF'` —— 导航/观察/操作/提取/
  过滤/聚合/输出写在**一段 Node.js heredoc** 里一次投递。不进「调两个命令、
  看结果、再调两个命令」的循环。内部基准对比 Vercel agent-browser：
  快至 3.45×，token 显著更省，任务越复杂差距越大。
- **helpers 预注入作用域**（无需 import）：`snapshotText` / `click('@92')` /
  `fillInput` / `openOrReuseTab` / `useOrCreateTaskSpace` / `js`
  （=Runtime.evaluate）/ `elementEval` / `drainEvents` / `cliLog` / `cdp`。
- **语义 Snapshot 是真壁垒**：深度定制 Chromium，页面以紧凑无障碍树呈现，
  每元素带 `[ref=N, loc=...]`，**典型页面 100-400 token**（对比塞原始 HTML
  的 token 墙）；ref 自动注册 refMap，`loc=` 提供跨轮次稳定选择器。
- **Space**：任务状态在 Space 内存活；共用 Chrome 登录态，不打扰用户标签页。
- **Experience**：`learnings/` 按站点沉淀经验，每次 helper 调用自动读取。
- ego-browser 明确「不是 Playwright/Puppeteer 替代品，读者是 LLM agent」。

**这正是 dsh 笔记里 PTC（Programmatic Tool Calling）模式在浏览器域的落地**
——见批次 F-3 立项笔记。一稿漏掉了这层连接，本稿全面修正。

## Decision

立项浏览器 PTC 化，三阶段。hermix 已有的资产：CDP 全栈底座（ChromeManager
+ CdpClient 双向 WS + 事件订阅 ~1500 行）、`browser_*` 工具面（navigate/
click/type/snapshot + 风险分级）、fail-closed 网络边界、Windows 原生。

**N1 浏览器 PTC 运行时（本批核心，取代一稿的「attach 登录态」优先级）**
- 新增 `browser_ptc` 工具（或复用 F-3 PTC 的执行器形态）：agent 写一段
  **Node.js heredoc** 一次投递，执行器在**预注入 helpers 的作用域**里跑，
  只回传 `cliLog` 收集的结果——单次往返替代 N 次 tool call 循环
- helpers 从现有 BrowserBackend 方法自动生成（navigate/click/type/snapshot
  一一对应），再加 PTC 必备三件：`wait*`（等待语义进代码）、`js()`
  （页内求值逃生舱）、`cliLog`（结构化输出收集）
- 语义 snapshot 强化：a11y 树 + ref 编号（现有 snapshot 已有雏形），对齐
  「100-400 token/页」目标——**这是 token 成本的根**，优先级高于一切花活
- 执行隔离：程序级总时长预算 + 静默超时（复用 D.4 drain 语义）；页内
  `js()` 过 CDP 执行，与 F-2 ACL 沙箱的受限令牌正交复用
- 审批语义：heredoc 里**每次真实动作**（click/type/navigate）仍过既有
  审批链——PTC 不是免审批通行证；纯观察类（snapshot/js 读）走 Safe 通道

**N2 Space + 登录态（一稿的 attach 内容降级为 N2）** —— 10-04 评审保留为唯一可立项
一档（不依赖已拒的 PTC 执行器）。攻击面边界与现状账见
[N2 立项笔记](../feature/2026-10-07-browser-space-and-login-state.md)：
最关键的一条是 hermix 的浏览器自动化今天仍**每次需要人工过一遍bootstrap 拉起**，
而 CDP 端口只绑 127.0.0.1、本机敏感面不得放宽。

**N3 独立分发（抽离路径）**
- 新 crate `hermix-browser`：CDP 底座 + 工具后端 + PTC 执行器提升为独立
  crate，对 hermix-core 的依赖收敛为错误/类型（或内联解耦）
- 双形态：a) hermix 内部 `browser` toolset 照旧；b) CLI/MCP server 壳
  （`hermix-browser serve`），任何 agent 框架可接——对标 ego 的
  `npx skills add` 分发模式
- Experience（站点经验沉淀）进 N3 尾部：`learnings/{domain}.json`，
  helper 调用自动读取，作为可观测数据而非黑盒

## Alternatives considered

- **维持逐调用工具面（现状）** —— 否决：正是 ego 文档批判的「调-看-再调」
  循环；每步 LLM 在环，token 与时延双输。
- **只抄 Space/登录态（一稿方案）** —— 否决：那只是入口体验，不是能力
  代差；一稿已按此推翻。
- **内嵌 Playwright/Puppeteer** —— 否决：引 Node 依赖栈换来的能力
  hermix CDP 层已具备；且 ego 明言它的读者是 agent 而非人类开发者，
  Playwright 的 API 形状（闭包/参数通道）对 heredoc 投递反而不友好。
- **自研 Chromium 定制（ego 的 snapshot 路线）** —— 分两步：N1 先用 CDP
  a11y 树 + ref 编号逼近（CDP 的 Accessibility domain + DOMSnapshot 就是
  官方 Chromium 内核能力，**不需要 fork 内核**）；只有当 token 压缩比实测
  不达标时，才评估定制内核（大概率不需要——CDP 已暴露所需原始数据）。
- **Rust 内嵌 JS 引擎跑 heredoc** —— 否决：F-3 PTC 已定 Node 侧车方案，
  浏览器 PTC 复用同一执行器（都是「写代码调工具」，执行器完全同构），
  不为第二个功能再造第二套。

## Consequences

- 获得：多步浏览器任务单次往返（token/时延双降，ego 实测 2.5-3.45×）；
  语义 snapshot 压到 100-400 token/页；与 F-3 PTC 共享执行器与心智模型；
  Windows 原生 + 安全面全套（ego-lite 未披露安全设计，macOS-only）。
- 放弃：全屏 TUI 类场景本来就不支持（无变化）；attach 模式要求用户配合
  一次重启浏览器。
- 代价：N1 ~1.5 周（执行器复用 F-3 后实际更少）、N2 ~1 周、N3 ~3 天；
  helpers 面一旦发布就是 agent 依赖的契约，演进要按稳定 API 对待。
- 风险：heredoc 里 `js()` 是任意页内执行——审批面上它比 click 更敏感，
  归 Dangerous 档（与 terminal 写命令同级）；Space 状态泄漏（任务结束
  未清）→ 会话表 TTL 兜底。
- 排序：**N1 依赖 F-3 PTC 执行器**（共用，不等从头造）；N1 → N2 → N3；
  M 批次（目录国内化）与 F-1（事件溯源）优先级不变，N 排其后。
