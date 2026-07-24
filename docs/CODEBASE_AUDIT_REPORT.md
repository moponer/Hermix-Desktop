# Hermix Desktop 全量代码审计报告

> 审计日期: 2026-07-24
> 审计方法: 第一性原理分析 + 对抗性审查
> 审计范围: `crates/` (21 Rust crate) + `apps/desktop-tauri/src/` (React 19 前端)

---

## 目录

1. [审计方法论](#1-审计方法论)
2. [问题清单总览](#2-问题清单总览)
3. [P0 — 中断与崩溃风险](#3-p0--中断与崩溃风险)
4. [P1 — 错误边界不清晰](#4-p1--错误边界不清晰)
5. [P2 — 性能卡顿](#5-p2--性能卡顿)
6. [P3 — 非功能性注释污染](#6-p3--非功能性注释污染)
7. [P4 — 代码质量与冗余](#7-p4--代码质量与冗余)
8. [P5 — 不安全类型使用](#8-p5--不安全类型使用)
9. [P6 — 样式代码未封装](#9-p6--样式代码未封装)
10. [P7 — 前后端对接遗漏](#10-p7--前后端对接遗漏)
11. [P8 — 前端不够人性化](#11-p8--前端不够人性化)
12. [P9 — 冗余组件](#12-p9--冗余组件)
13. [优化计划](#13-优化计划)
14. [对抗性审查总结](#14-对抗性审查总结)

---

## 1. 审计方法论

### 第一性原理

代码审计的物理约束：
- **可维护性** = 文件行数 × 认知复杂度 × 注释信噪比。超过阈值后，任何修改的缺陷引入率指数上升。
- **可靠性** = 错误路径覆盖率 × 错误传播透明度。静默吞错 (`catch(() => {})`) 等价于将确定性问题转化为随机崩溃。
- **性能** = 渲染次数 × 每次渲染的 DOM 操作量。`key={i}` 和内联 style 对象使 React 的 reconciliation 退化为 O(n) 全量 diff。
- **类型安全** = `as any` 和 `unsafe {}` 的数量。每处都是一条"信任但不验证"的路径，在重构时会产生无声的类型漂移。

### 对抗性审查原则

对每个发现的问题追问："在什么场景下这个问题会导致系统崩溃或用户体验降级？"

---

## 2. 问题清单总览

| 优先级 | 类别 | 问题数 | 影响面 |
|--------|------|--------|--------|
| P0 | 中断与崩溃风险 | 6 | 生产稳定性 |
| P1 | 错误边界不清晰 | 8 | 用户体验 |
| P2 | 性能卡顿 | 12 | 交互流畅度 |
| P3 | 非功能性注释污染 | ~131处 | 可维护性 |
| P4 | 代码质量与冗余 | 10 | 可维护性 |
| P5 | 不安全类型使用 | 15 | 类型安全 |
| P6 | 样式代码未封装 | 8 | 可维护性 |
| P7 | 前后端对接遗漏 | 5 | 功能完整性 |
| P8 | 前端不够人性化 | 7 | 用户体验 |
| P9 | 冗余组件 | 4 | 可维护性 |
| **合计** | | **~106 + 131注释** | |

---

## 3. P0 — 中断与崩溃风险

### P0-1: ErrorBoundary 覆盖不足

**位置**: `apps/desktop-tauri/src/main.tsx:12`, `apps/desktop-tauri/src/App.tsx:2771`

**现状**: ErrorBoundary 仅在两处使用：
1. `main.tsx` 包裹整个 `<App />`
2. `App.tsx:2771` 包裹"主内容区"

**对抗性场景**: Sidebar 渲染崩溃 → 整个应用白屏（因为外层 ErrorBoundary 捕获后无法局部恢复）。RightSidebar 崩溃同理。SettingsView 有 1400+ 行复杂逻辑，任何一个子组件抛错都会导致全屏白屏。

**根因**: 没有按视图层级建立错误隔离区。一个桌面应用的不同面板（Sidebar / ChatView / RightSidebar / SettingsView）应彼此独立，一个面板崩溃不应影响其他面板。

**修复方案**: 在 App.tsx 中为每个顶层视图分别包裹 ErrorBoundary：
```tsx
<ErrorBoundary name="侧边栏"><Sidebar ... /></ErrorBoundary>
<ErrorBoundary name="聊天区"><ChatView ... /></ErrorBoundary>
<ErrorBoundary name="右侧栏"><RightSidebar ... /></ErrorBoundary>
<ErrorBoundary name="设置"><SettingsView ... /></ErrorBoundary>
```

### P0-2: Rust `unsafe` 块缺乏安全不变式文档

**位置**:
- `crates/hermix-tools/src/tools/storage_clean.rs:221` — `unsafe { SHFileOperationW(&mut op) }` 无 SAFETY 注释
- `crates/hermix-tools/src/tools/storage_analyzer.rs:80` — `unsafe { ... }` 无 SAFETY 注释
- `crates/hermix-cua/src/uia.rs:224,249,262,275` — 4 个 `pub unsafe fn` 无安全前提条件说明
- `crates/hermix-server/src/dashboard/migration.rs:340-573` — 16 处 `unsafe { std::env::set_var(...) }` 无 SAFETY 注释
- `crates/hermix-server/src/dashboard/status.rs:35` — `unsafe { libc::kill(pid as i32, 0) }` 无 SAFETY 注释
- `crates/hermix-server/src/lib.rs:1300,1306` — `unsafe { plugin.create_tool_handler(idx) }` 无 SAFETY 注释
- `crates/hermix-tools/src/tools/cua.rs:1019,1058,1175,1275,1278,1365,1366,1438,1439` — 9 处 unsafe 调用

**对抗性场景**: `SHFileOperationW` 的 `op` 结构体如果 `pFrom` 不是双 `\0\0` 结尾，会读取越界内存。`libc::kill` 如果 pid 来自未验证的用户输入，可以信号注入任意进程。

**修复方案**: 每个 unsafe 块必须添加 `// SAFETY: ...` 注释，说明为什么这处调用是安全的。CUA 模块的 `pub unsafe fn` 必须在文档注释中声明调用者必须满足的前提条件。

### P0-3: `panic!` / `unreachable!()` 在非测试代码中

**位置**:
- `crates/hermix-tools/src/tools/terminal.rs:340,420` — `ApprovalOutcome::Approved => unreachable!()` 在生产代码路径中
- `crates/hermix-tools/src/tools/terminal.rs:876,884,891,894` — `panic!` 在测试辅助代码中（可接受）

**对抗性场景**: 如果 approval 逻辑变更，新增了 `Approved` 变体到某个枚举但忘记更新 terminal.rs 的 match，`unreachable!()` 会导致整个 agent 进程 panic，正在执行的任务全部丢失。

**修复方案**: 将 `unreachable!()` 替换为 `return Err(...)` 或 `tracing::error!` + 优雅降级。

### P0-4: 静默错误吞没导致状态不一致

**位置** (高频):
- `apps/desktop-tauri/src/components/Sidebar.tsx:227` — `.catch(() => {})`
- `apps/desktop-tauri/src/components/SettingsView.tsx:147,430,445,630,697,958,1082,1083` — 8处 `.catch(() => {})`
- `apps/desktop-tauri/src/components/ChatView.tsx:1045,1110,1161` — 3处 `updateSessionMetadata(...).catch(() => {})`
- `apps/desktop-tauri/src/App.tsx:924` — `registerBuiltinsToBackend().catch(() => {})`

**对抗性场景**: `updateSessionMetadata` 失败时，UI 显示的模型/专家已切换，但后端未持久化。用户重开应用发现设置丢失，且无任何错误提示。`registerBuiltinsToBackend` 失败时，内置专家静默不可用，用户不知道为什么找不到某个专家。

**修复方案**: 至少添加 `console.error` + 用户可见的 toast 通知。关键操作（如 session metadata 同步）应有重试机制。

### P0-5: `serde_json::from_str(...).ok()` 丢弃解析错误

**位置**:
- `crates/hermix-server/src/lib.rs:3769,6589` — `serde_json::from_str::<SendMessageRequest>(&text).ok()`

**对抗性场景**: 客户端发送了格式错误的 WebSocket 消息，`.ok()` 静默返回 `None`，服务器不记录任何错误日志。客户端看到"消息发送无响应"，无法排查。

**修复方案**: 改为 `match serde_json::from_str(&text) { Ok(v) => Some(v), Err(e) => { tracing::warn!(...); None } }`。

### P0-6: Tauri 事件监听器泄漏风险

**位置**: `apps/desktop-tauri/src/components/BackendStatus.tsx:33`

**现状**: `listen("hermix:server-restarting", ...)` 返回的 `unlisten` 函数需要在 `useEffect` cleanup 中调用。代码中有 `const unlistenRestarting = listen(...)` 但 `listen` 是异步的，`unlistenRestarting` 是一个 Promise，不是清理函数。

**对抗性场景**: 组件卸载后，事件监听器仍然活跃，引用已卸载的 state setter，导致 React 警告或内存泄漏。

**修复方案**: 正确处理异步 unlisten：
```typescript
useEffect(() => {
  let unlisten: (() => void) | undefined;
  listen("hermix:server-restarting", (e) => { ... }).then(fn => unlisten = fn);
  return () => { unlisten?.(); };
}, []);
```

---

## 4. P1 — 错误边界不清晰

### P1-1: `console.error` / `console.warn` 替代用户可见反馈

**位置** (统计):
- `SettingsView.tsx`: 12 处 `console.error`/`console.warn`/`console.info`
- `TitleBar.tsx`: 3 处
- `Sidebar.tsx`: 1 处
- `RoomSettingsDrawer.tsx`: 2 处
- `TodoListCard.tsx`: 1 处

**问题**: 普通用户不会打开 DevTools。所有 `console.error` 应同时触发用户可见的错误提示（toast / inline error banner）。

**修复方案**: 建立统一的 `useToast()` 或全局 error notification 系统，所有 catch 块都通过该系统反馈。

### P1-2: ErrorBoundary fallback 使用硬编码颜色

**位置**: `apps/desktop-tauri/src/components/ErrorBoundary.tsx:44-118`

**问题**: fallback UI 全部使用内联 style + 硬编码颜色 (`#0f0f12`, `#2e2e34`, `#3e3e44`, `#1c1c20`)。在浅色主题下，错误页面会显示为暗色，与应用其他部分视觉不一致。

**修复方案**: 使用 CSS 变量 (`var(--color-bg-primary)` 等) 替代硬编码颜色。将 fallback 样式提取到 CSS 类。

### P1-3: `try { clearStreamError(); } catch (_) { /* swallow */ }` 模式

**位置**: `apps/desktop-tauri/src/App.tsx:1042,1122`

**问题**: `clearStreamError` 是一个状态清理函数，如果它抛错说明状态管理本身有 bug。静默吞掉使得这类 bug 永远不可见。

**修复方案**: 移除 try-catch，让 ErrorBoundary 处理；或至少记录 `console.error`。

### P1-4: `as any` 类型断言绕过类型检查

**位置**:
- `apps/desktop-tauri/src/components/ChatView.tsx:1263` — `(f as any).path`
- `apps/desktop-tauri/src/components/ChatView.tsx:1860` — `(session as any).message_count`
- `apps/desktop-tauri/src/components/ToolEventCard.tsx:223` — `(e: any)`

**问题**: `f as any).path` 绕过了 File 类型的检查，如果 `f` 不是 File 对象（如拖入文件夹），`.path` 为 `undefined` 但 TypeScript 不会报错。

**修复方案**: 定义正确的类型 (`File & { path?: string }` 或 Tauri 的 `FileEntry` 类型)。

### P1-5: `eslint-disable-next-line` 掩盖 hooks 依赖问题

**位置**:
- `apps/desktop-tauri/src/components/SettingsView.tsx:1370` — `eslint-disable-next-line react-hooks/exhaustive-deps`
- `apps/desktop-tauri/src/components/PopoverShell.tsx:97` — 同上
- `apps/desktop-tauri/src/App.tsx:199` — `eslint-disable-next-line prefer-const`

**问题**: `react-hooks/exhaustive-deps` 的禁用意味着 effect 可能依赖了过期的闭包变量，导致使用过期数据。这是 React 中最常见的 subtle bug 来源。

**修复方案**: 逐一审查每处禁用，确认是否真的需要。如果需要，添加注释说明原因；如果不需要，修复依赖数组。

### P1-6: `unwrap_or_default()` 掩盖数据缺失

**位置** (高频):
- `crates/hermix-tools/src/tools/storage_clean.rs:94,112,427` — `std::env::var("USERPROFILE").unwrap_or_default()`
- `crates/hermix-tools/src/tools/url_safety.rs:135,553` — `serde_yaml::from_str(&content).unwrap_or_default()`
- `crates/hermix-tools/src/tools/todo.rs:281,362,493` — 多处 `.unwrap_or_default()`

**问题**: `USERPROFILE` 为空字符串时，后续路径拼接会生成错误路径（如 `\AppData\Local\...`），而非报错。`serde_yaml::from_str` 解析失败时返回空结构，SSRF 防护规则静默失效。

**修复方案**: 对安全相关的解析失败应 `return Err(...)`。对环境变量缺失应提供有意义的默认值或报错。

### P1-7: WebSocket 重连缺乏指数退避策略

**位置**: `apps/desktop-tauri/src/hooks/useStreamChat.ts`

**现状**: WS 断连后有重连逻辑，但缺乏明确的指数退避。连续重连失败时可能产生请求风暴。

**修复方案**: 实现标准的指数退避重连（200ms → 400ms → 800ms → ... → 最大 30s），并在 UI 显示"正在重连"状态。

### P1-8: Rust `expect()` 在非测试代码中使用

**位置**:
- `crates/hermix-transport/src/lib.rs:994-1253` — 多处 `.expect("serialize")` / `.expect("deserialize")`（测试代码，可接受）
- `crates/hermix-tools/src/v4a_patch.rs:123-126` — `Regex::new(...).expect("...")`（生产代码）

**问题**: `v4a_patch.rs` 中的 `expect` 如果正则编译失败（理论上不会，但修改正则后可能），会导致进程 panic。

**修复方案**: 使用 `OnceLock<Regex>` 或 `lazy_static!`，在编译期保证正则有效。生产代码中 `expect` 应仅用于"编译器/语言保证不可能失败"的场景。

---

## 5. P2 — 性能卡顿

### P2-1: `key={i}` (索引作为 key) — 列表渲染性能与正确性

**位置** (统计):
- `TaskPanel.tsx:203,234` — TimelineRow / AuditRow
- `SettingsView.tsx:1436`
- `RoomSettingsDrawer.tsx:183,241`
- `RightSidebar.tsx:1336,1781`
- `PluginsView.tsx:1247`
- `MessageBubble.tsx:239,2277,2681`
- `agents/TestDialog.tsx:120`
- `AwakeningCard.tsx:124`

**对抗性场景**: 消息列表使用 `key={i}`，当中间插入/删除消息时，React 会复用错误的 DOM 节点，导致：
1. 动画错乱（旧节点的 transition 作用到新数据上）
2. 状态泄漏（input 焦点、滚动位置错位）
3. 不必要的重渲染（React 无法通过 key 判断"元素移动"，只能判断"元素变更"）

**修复方案**: 所有列表使用稳定的唯一 ID 作为 key（如 `key={item.id}` 或 `key={item.timestamp + index}`）。

### P2-2: 超大组件文件 — 认知复杂度过高

**前端** (TSX):
| 文件 | 行数 | 问题 |
|------|------|------|
| `ChatView.tsx` | 2856 | 单组件承担消息渲染、输入、滚动、拖放、模型选择、专家切换等 7+ 职责 |
| `App.tsx` | 2815 | 根组件承担路由、状态管理、副作用编排、UI 布局等全部职责 |
| `MessageBubble.tsx` | 2632 | 消息气泡承担 markdown 渲染、工具时间线、审批卡片、图片预览等职责 |
| `SettingsView.tsx` | ~1400 | 设置页承担模型管理、provider 配置、环境变量、健康检查等职责 |
| `TaskPanel.tsx` | ~930 | 任务面板内联了大量 HSL 颜色计算和状态映射 |

**后端** (Rust):
| 文件 | 行数 | 问题 |
|------|------|------|
| `hermix-server/src/lib.rs` | 8004 | HTTP 路由 + WebSocket + 业务逻辑 + 持久化 全在一个文件 |
| `hermix-agent/src/agent_loop/mod.rs` | 5956 | Agent 循环主逻辑过于集中 |
| `session_persistence.rs` | 4428 | 持久化层职责过多 |
| `compression.rs` | 4380 | 压缩逻辑过于庞大 |
| `local_agent_service.rs` | 3844 | Agent 服务层过于集中 |

**根因**: 缺乏模块拆分的纪律。当一个文件超过 1000 行，其认知复杂度使得任何修改都有引入 bug 的风险。

**修复方案**: 按职责拆分。例如：
- `ChatView` → `ChatView` (容器) + `ChatMessageList` + `ChatInput` + `ChatModelSelector` + `ChatExpertPicker`
- `lib.rs` → `router.rs` + `handlers/` + `ws.rs` + `middleware.rs`
- `mod.rs` (agent_loop) → `loop_core.rs` + `loop_stream.rs` + `loop_phases.rs` + `loop_metrics.rs`

### P2-3: 内联 style 对象每次渲染重建

**位置** (高频):
- `TaskPanel.tsx` — 20+ 处内联 `style={{ ... }}`
- `ErrorBoundary.tsx` — 全部样式内联
- `MessageBubble.tsx:242` — `style={{ maxWidth: 180, ... border: "1px solid #2e2e34" }}`
- `SpreadsheetView.tsx` — 多处内联 border style
- `TodoListCard.tsx:177` — `style={{ borderLeftColor: ... }}`

**问题**: 每次组件渲染时，`style={{ ... }}` 创建新的对象引用，React 会认为 style 发生变化，触发不必要的 DOM 更新。在流式渲染（每秒数十次更新）的场景下，这会产生可感知的卡顿。

**修复方案**: 将静态 style 提取为 CSS 类或模块常量。动态 style 使用 `useMemo` 缓存。

### P2-4: 硬编码 HSL 颜色未使用设计令牌

**位置**: `TaskPanel.tsx` — 大量 `hsl(43, 96%, 48%)`, `hsl(140, 60%, 50%)`, `hsl(0, 70%, 55%)` 等

**问题**: 颜色硬编码在组件中，无法通过主题切换统一调整。与 `styles/tokens.css` 中的设计令牌系统脱节。

**修复方案**: 将所有颜色映射到 CSS 变量（`var(--color-warning)`, `var(--color-success)`, `var(--color-danger)` 等）。

### P2-5: `setInterval` 轮询缺乏可见性优化

**位置**:
- `BackendStatus.tsx:27` — 30s 健康检查轮询
- `GroupChatView.tsx:210` — 5s agent 刷新轮询
- `ToolEventCard.tsx:537` — 1s 计时器
- `MessageBubble.tsx:2315` — 1s 审批倒计时
- `InlineApprovalCard.tsx:161` — 1s 审批倒计时
- `AwakeningCard.tsx:63` — 5s 状态检查

**问题**: 组件不可见时（如 Tab 切换、最小化窗口），`setInterval` 仍在运行，浪费 CPU 和网络。`InlineApprovalCard` 的注释说"0.1% CPU不值得优化"，但多个这样的计时器叠加后影响显著。

**修复方案**: 使用 `document.visibilityState` 或 Tauri 窗口焦点事件，在不可见时暂停轮询。或使用 `requestIdleCallback` 替代 `setInterval`。

### P2-6: `components.css` 4269 行 — 单体 CSS 文件

**位置**: `apps/desktop-tauri/src/styles/components.css`

**问题**: 所有组件样式集中在一个文件中，任何样式修改都需要在 4269 行中定位。CSS 层叠规则在大文件中变得不可预测（同名 class 冲突）。浏览器需要解析整个文件才能应用任何规则。

**修复方案**: 按组件拆分为多个 CSS 文件（`chat-view.css`, `message-bubble.css`, `settings.css` 等），或迁移到 CSS Modules / Tailwind 组件层。

### P2-7: 缺少 `React.memo` 的大型组件

**位置**:
- `MessageBubble.tsx` — 在 `ChatView.tsx:44` 中手动 memo 化，但组件本身没有默认 memo
- `ChatView.tsx` — 未 memo 化
- `RightSidebar.tsx` — 未 memo 化
- `SettingsView.tsx` — 未 memo 化

**问题**: ChatView 2856 行，每次 App re-render 都会重新执行整个 ChatView 函数体，包括所有 useMemo / useCallback 的依赖检查。

**修复方案**: 对顶层视图组件添加 `React.memo` + 自定义比较函数。

### P2-8: `useEffect` 依赖数组缺失或过宽

**位置**:
- `RoomMemberDialog.tsx:97-99` — 三个 `useEffect` 分别同步 props 到 state，每次 prop 变化都重置 state
- `SettingsView.tsx:292` — `useEffect(() => { saveModelCache(fetchedModels); }, [fetchedModels]);` 无 useCallback 依赖

**问题**: `RoomMemberDialog` 的模式会导致用户正在编辑的输入被 prop 变化覆盖。

**修复方案**: 使用 `useEffect` 同步 props → state 时，添加 "仅在 prop 真正变化时才更新" 的逻辑，或使用 `useSyncExternalStore` / 受控组件模式。

### P2-9: `setTimeout` 未清理的潜在泄漏

**位置**:
- `SettingsView.tsx:247,346,368` — `setTimeout(async () => { ... })` 在组件卸载后仍会执行
- `RoomSettingsDrawer.tsx:87,111` — 同上
- `RoomMemberDialog.tsx:180` — 同上
- `PersonaView.tsx:100` — 同上

**问题**: 组件卸载后 `setTimeout` 回调仍执行，调用 `setState` 导致 React 警告或内存泄漏。

**修复方案**: 将 `setTimeout` 返回值存入 ref，在 `useEffect` cleanup 中 `clearTimeout`。

### P2-10: `SettingsView` 中的 `setTimeout(async () => {...})` 嵌套重试

**位置**: `SettingsView.tsx:197,211,236`

**问题**: 手动实现的指数退避重试逻辑分散在三处几乎相同的代码中，且 `setTimeout` 内的 `async` 函数如果抛错不会被外层 catch 捕获。

**修复方案**: 提取为 `useRetryableFetch` 自定义 hook，统一重试逻辑。

### P2-11: 大量 `console.log/warn/error/info` 在生产构建中

**位置**: SettingsView.tsx (12处), TitleBar.tsx (3处), Sidebar.tsx (1处) 等

**问题**: 生产构建中 console 调用不会被移除（除非配置了 babel/terser 的 drop-console），影响性能并泄露调试信息。

**修复方案**: 在 Vite 配置中添加 `terser: { compress: { drop_console: true } }`，或使用日志级别控制。

### P2-12: `SpreadsheetView` 内联 border style

**位置**: `SpreadsheetView.tsx:253-254,278,297,351`

**问题**: 每个单元格的 border 使用内联 `style={{ borderBottom: "1px solid var(--color-border-subtle)" }}`。在大型表格（100+ 行 × 10+ 列）中，这会创建数千个内联 style 对象。

**修复方案**: 使用 CSS 类替代内联 border。

---

## 6. P3 — 非功能性注释污染

### 统计

| 类型 | 出现次数 | 分布 |
|------|----------|------|
| "主人" | ~110 | CSS (25), TSX (45), Rust (30), Docs (10) |
| "老大" | ~21 | TSX (5), Rust (3), Docs (8), 其他 (5) |
| 日期戳注释 (2026-06/07-XX) | ~80+ | 全代码库 |

### P3-1: CSS 文件中的"主人反馈"注释

**位置** (最严重):
- `components.css` — 15 处"主人"引用，如：
  - `/* v3 主人反馈：会话列表去掉 "几小时前 / 几天前" 时间戳 */`
  - `/* 主人反馈: 蓝/红色字符很难看, 改为图标 + 文字 */`
  - `/* 2026-07-06 主人反馈: flex: 1 把命令撑满整行 */`
- `inline-approval-card.css` — 7 处"主人"引用
- `code-view.css` — 2 处
- `base.css` — 1 处

**问题**: 这些注释记录的是设计决策的"原因"（为什么改），但用"主人反馈"这种个人化的表述。新开发者无法理解"主人"是谁，也无法追溯决策上下文。注释中混合了中英文，降低可读性。

**修复方案**: 将"主人反馈"替换为客观的设计原则描述。例如：
- `/* v3: 移除会话列表时间戳——减少视觉噪音，会话标题已足够区分 */`
- `/* 状态色统一为图标+文字 chip，避免纯色字符在浅/深色主题下对比度不足 */`

### P3-2: TSX 文件中的"主人反馈"注释

**位置** (高频):
- `ChatView.tsx` — 15 处
- `StreamingBubble.tsx` — 5 处
- `App.tsx` — 11 处
- `SettingsView.tsx` — 5 处
- `ToolEventCard.tsx` — 2 处
- `MessageBubble.tsx` — 4 处
- `PendingApprovalToast.tsx` — 4 处
- `useStreamChat.ts` — 1 处
- `useExpert.ts` — 1 处
- `useClickOutside.ts` — 1 处
- `imageCompressor.ts` — 1 处

**示例**:
```typescript
// 2026-06-17 主人反馈 "已完成几个字的设计难看"：
//   终结态不再显示任何 chip / 时间戳
```

**修复方案**: 移除"主人"引用，保留技术决策说明：
```typescript
// 终结态不显示 chip/时间戳——避免与 MessageBubble 的 meta line 冗余
```

### P3-3: Rust 文件中的"主人"注释

**位置**:
- `empty_response_handler.rs` — 8 处，如 `// 主人原话：「只有穷极一切办法没有完成任务的可能了，才会主动停止」`
- `agent_loop/mod.rs` — 3 处
- `thinking.rs` — 1 处: `//! 主人哲学"做好自己"`
- `archive.rs` — 1 处
- `run_loop.rs` — 1 处
- `approval.rs` — 1 处: `// (主人原话: "基础的命令不应该纳入权限管理中")`
- `lib.rs` (server) — 1 处
- `memory.rs` — 1 处
- `custom_providers.rs` — 1 处
- `recall_scheduler.rs` — 2 处 (测试代码)
- `yanluo.rs` — 3 处 (含测试数据)
- `embedded.rs` — 4 处 (测试代码)

**问题**: 生产代码中的"主人原话"注释将个人化的需求描述硬编码到代码中。这些注释既不是 API 文档，也不是设计文档，而是需求追溯记录。

**修复方案**: 将需求追溯移到 commit message 或 PR description。代码注释只保留"做了什么"和"为什么这样做"的技术决策。

### P3-4: 版本号注释 (v1, v2, v3...v35) 散布全代码库

**位置**: 
- `protocolAdapter.ts` — `// v33 (2026-07-06): wall-clock ISO-8601 timestamp`
- `transport/lib.rs` — `// v4 (2026-06-22): re-export`
- `StreamingBubble.tsx` — `// v18 (2026-06-17): multiple streaming placeholders`
- 等等，遍布 50+ 处

**问题**: 版本号是 Git 的职责，不应出现在代码注释中。`v33` 对读者没有任何意义——无法从 v33 推断出变更内容，必须查 Git 历史。

**修复方案**: 移除版本号前缀，保留变更原因的描述。如果变更原因已过时（如"修复了 X bug"，而 X bug 已不存在），直接删除注释。

### P3-5: 注释中的日期戳过度使用

**位置**: Rust 代码中 `2026-06-XX` / `2026-07-XX` 日期戳出现 80+ 次

**问题**: 日期戳注释的用途是"这个变更是什么时候做的"，但 Git blame 提供了更准确的信息。代码中的日期戳注释会在后续修改时变得过时（修改了代码但忘记更新注释日期）。

**修复方案**: 移除代码中的日期戳，保留变更原因的描述。日期信息通过 Git blame 获取。

---

## 7. P4 — 代码质量与冗余

### P4-1: `send_message` 与 `send_message_stream` 重复 ~300 行

**位置**: `crates/hermix-agent/src/local_agent_service.rs:1199-1203`

**现状**: 代码中已有 TODO 注释承认这个问题：
```rust
// TODO(L8): send_message and send_message_stream share ~300 lines of
// duplicated setup code (session loading, gateway safety net, image
// handling, model resolution, etc.). Consider extracting a shared
// `prepare_and_run` helper that both methods call with a
// streaming/non-streaming mode flag.
```

**对抗性场景**: 修复了一个方法中的 bug 但忘记同步到另一个方法，导致流式和非流式行为不一致。

**修复方案**: 提取 `prepare_and_run` 共享方法。

### P4-2: `url_safety.rs` 中重复的文件修改时间检查

**位置**:
- `crates/hermix-tools/src/tools/url_safety.rs:142` — `let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();`
- `crates/hermix-tools/src/tools/url_safety.rs:560` — 完全相同的代码

**修复方案**: 提取为 `fn get_file_mtime(path: &Path) -> Option<SystemTime>` 辅助函数。

### P4-3: `storage_clean.rs` 中重复的 `USERPROFILE` 获取

**位置**: `crates/hermix-tools/src/tools/storage_clean.rs:94,112,427`

**问题**: 三处相同的 `std::env::var("USERPROFILE").unwrap_or_default()` 调用。

**修复方案**: 提取为 `fn user_profile() -> PathBuf` 辅助函数。

### P4-4: `voice_mode.rs` 中重复的 `resp.text().await.unwrap_or_default()`

**位置**: `crates/hermix-tools/src/tools/voice_mode.rs:328,376,415`

**修复方案**: 提取为 `async fn fetch_error_body(resp: &Response) -> String` 辅助函数。

### P4-5: SettingsView 中的重复重试逻辑

**位置**: `apps/desktop-tauri/src/components/SettingsView.tsx:197,211,236`

**问题**: 三处几乎相同的 `for (let attempt = 0; ...)` 重试循环，仅 URL 和处理逻辑不同。

**修复方案**: 提取为 `useRetryableFetch` hook 或 `fetchWithRetry` 工具函数。

### P4-6: `api.ts` 中 `fetchJSON` 和 `fetchVoid` 共享重试逻辑但代码重复

**位置**: `apps/desktop-tauri/src/api.ts:54-73`

**问题**: 两个函数的 `withRetry` 包装和错误处理逻辑完全相同，仅返回类型不同。

**修复方案**: 合并为一个泛型函数，通过泛型参数控制是否解析 JSON。

### P4-7: Rust 文件过大导致模块边界模糊

**位置**:
- `hermix-server/src/lib.rs` (8004 行) — 路由定义、handler 实现、WebSocket 处理、业务逻辑全在一个文件
- `hermix-agent/src/agent_loop/mod.rs` (5956 行) — 循环主逻辑、状态管理、阶段处理、指标收集全在一个文件
- `hermix-agent/src/session_persistence.rs` (4428 行) — 加载、保存、消息处理、审批持久化、归档全在一个文件

**修复方案**: 按职责拆分为子模块。每个子模块不超过 800 行。

### P4-8: `FRONTEND_AUDIT_TODO_2026-07-13` 标记散布

**位置**: `hermix-tools/src/backends/todo.rs:39,46,247`, `registry.rs:1190`, `session_stamper.rs:69`, `tools/todo.rs:128,194,364,431,741`

**问题**: 这些是审计标记注释，已完成但未清理。它们增加了代码噪音，新开发者会误以为还有未完成的工作。

**修复方案**: 审计完成后移除这些标记。审计历史通过 Git 追溯。

### P4-9: `ToolEventCard.tsx` 注释说明"structurally identical"但未提取共享类型

**位置**: `apps/desktop-tauri/src/components/ToolEventCard.tsx:40`

**现状**: `// structurally identical: any change here MUST be mirrored in ...`

**问题**: 两个结构体需要手动保持同步，违反 DRY 原则。

**修复方案**: 提取为共享类型定义。

### P4-10: `ToolEventCard.tsx` 中的 `(e: any)` 事件类型

**位置**: `apps/desktop-tauri/src/components/ToolEventCard.tsx:223`

**问题**: 事件处理器使用 `any` 类型，无法获得 TypeScript 的类型检查保护。

**修复方案**: 定义正确的事件类型接口。

---

## 8. P5 — 不安全类型使用

### P5-1: Rust `unsafe` 块统计

| 文件 | unsafe 块数 | 风险等级 | 说明 |
|------|-------------|----------|------|
| `cua.rs` | 9 | 高 | UI Automation FFI 调用，无 SAFETY 注释 |
| `migration.rs` | 16 | 中 | `std::env::set_var` (Rust 2024 edition 不安全) |
| `uia.rs` | 4 | 高 | `pub unsafe fn` 无安全前提条件文档 |
| `storage_clean.rs` | 1 | 中 | `SHFileOperationW` FFI |
| `storage_analyzer.rs` | 1 | 中 | FFI 调用 |
| `status.rs` | 1 | 中 | `libc::kill` 信号检查 |
| `lib.rs` (server) | 2 | 中 | 插件 FFI |

### P5-2: TypeScript `as any` 使用

| 文件 | 位置 | 用途 | 风险 |
|------|------|------|------|
| `ChatView.tsx:1263` | `(f as any).path` | Tauri File 的 path 属性 | 中 — 非 File 对象时 .path 为 undefined |
| `ChatView.tsx:1860` | `(session as any).message_count` | Session 类型缺失字段 | 低 — 但说明类型定义不完整 |
| `ToolEventCard.tsx:223` | `(e: any)` | 事件类型 | 中 — 无类型安全 |
| `ProviderIcon.tsx:10` | `null as any` | 注释中提到的历史问题 | 低 |

### P5-3: `[k: string]: unknown` 索引签名

**位置**: `apps/desktop-tauri/src/protocolAdapter.ts:56` — `WireMessage` 类型

**问题**: `[k: string]: unknown` 允许任意额外属性，使得类型检查对 WireMessage 的额外字段无效。

**修复方案**: 使用 `Exact` 类型工具或显式列出所有可能的字段。

### P5-4: `ChatOverrides` 的 `[k: string]: unknown`

**位置**: `apps/desktop-tauri/src/hooks/useStreamChat.ts:114`

**问题**: 同 P5-3，允许任意额外属性绕过类型检查。

---

## 9. P6 — 样式代码未封装

### P6-1: `components.css` 4269 行单体文件

**位置**: `apps/desktop-tauri/src/styles/components.css`

**问题**: 所有组件样式集中在一个文件中。修改任何一个组件的样式都需要在 4269 行中搜索。CSS 层叠规则的不可预测性随文件大小指数增长。

**修复方案**: 按组件/功能拆分：
- `chat.css` (ChatView + MessageBubble + StreamingBubble)
- `sidebar.css` (Sidebar + SessionItem)
- `settings.css` (SettingsView + tabs)
- `task-panel.css` (TaskPanel + TimelineRow)
- `groupchat.css` (GroupChatView + RoomList)

### P6-2: `ErrorBoundary.tsx` 全部样式内联

**位置**: `apps/desktop-tauri/src/components/ErrorBoundary.tsx:44-118`

**问题**: 错误页面样式全部使用内联 `style={{ ... }}`，硬编码颜色值。无法通过主题切换适配。

**修复方案**: 提取为 `error-boundary.css` + CSS 变量。

### P6-3: `TaskPanel.tsx` 内联 HSL 颜色

**位置**: `apps/desktop-tauri/src/components/TaskPanel.tsx` — 20+ 处

**问题**: 所有状态颜色（pending/executing/completed/failed）以 HSL 字面量内联在 JSX 中，未使用 `tokens.css` 中的设计令牌。

**修复方案**: 在 `tokens.css` 中定义状态颜色变量，组件引用变量。

### P6-4: `SpreadsheetView.tsx` 内联 border style

**位置**: `apps/desktop-tauri/src/components/SpreadsheetView.tsx:253-254,278,351`

**问题**: 每个单元格的 border 使用内联 style，在大表格中创建大量内联对象。

**修复方案**: 使用 CSS 类 `.spreadsheet-cell { border-bottom: 1px solid var(--color-border-subtle); border-right: 1px solid var(--color-border-subtle); }`。

### P6-5: `MessageBubble.tsx` 内联 border

**位置**: `apps/desktop-tauri/src/components/MessageBubble.tsx:242`

**问题**: 图片预览的 border 使用硬编码 `#2e2e34`。

**修复方案**: 使用 `var(--color-border-primary)` CSS 变量。

### P6-6: CSS 文件中的叙事性注释

**位置**: `inline-approval-card.css` (全文), `components.css` (多处)

**问题**: CSS 注释中包含大段设计叙述（"设计意图:权限确认是一个带边界感的操作——主人要么允许要么..."），这些内容应放在设计文档中，不是 CSS 注释。

**修复方案**: 保留简短的技术说明，移除叙事性内容。

### P6-7: 带边框线组件清单

以下组件在 Tailwind className 中直接使用 border 类，未提取为可复用的样式组件：

| 组件 | border 类 | 问题 |
|------|-----------|------|
| `ThemeToggle.tsx:29` | `border border-border-primary` | 静态边框可用 CSS 类封装 |
| `TaskPanel.tsx` (多处) | `border-l`, `border-b`, `border-t`, `border-l-2` | 边框样式散布在多个 className 中 |
| `SettingsView.tsx` (多处) | `border-b`, `border-b-2`, `border-r`, `border` | 同上 |
| `SpreadsheetView.tsx` | 内联 `borderBottom`, `borderRight` | 见 P6-4 |
| `ErrorBoundary.tsx:87` | `border: "1px solid #3e3e44"` | 硬编码颜色 |
| `MessageBubble.tsx:242` | `border: "1px solid #2e2e34"` | 硬编码颜色 |

**修复方案**: 定义统一的边框样式类（`.card-border`, `.divider-border`, `.input-border`），替代散布的 Tailwind border 类。

### P6-8: `code-view.css` 含"主人"引用和版本号注释

**位置**: `apps/desktop-tauri/src/styles/code-view.css:4,107`

**问题**: CSS 文件开头有 `v6 → v7 的关键变化（主人："渲染的方式肯定要重写，给我删除重写"）` 这样的注释。

**修复方案**: 移除个人化注释，保留技术说明。

---

## 10. P7 — 前后端对接遗漏

### P7-1: 后端路由无前端调用

以下后端路由在前端代码中未找到对应的 API 调用：

| 后端路由 | 说明 | 风险 |
|----------|------|------|
| `GET /v1/protocol/schema` | 协议 schema | 低 — 可能仅用于调试 |
| `POST /v1/telemetry/client-event` | 客户端遥测 | 中 — 遥测数据缺失 |
| `GET /metrics` | Prometheus 指标 | 低 — 仅用于监控 |
| `GET /v1/sessions/{id}/lineage` | 会话血缘 | 中 — 功能未暴露 |
| `POST /v1/commands` | 命令执行 | 高 — 可能存在安全隐患 |
| `GET /api/researcher/*` (5个路由) | 研究员功能 | 中 — 功能未暴露 |
| `POST /api/dashboard/plugins/rescan` | 插件重扫 | 低 — 可能通过其他方式触发 |

**修复方案**: 逐一确认每个路由是否需要前端调用。不需要的标记为 "server-only" 并添加注释。需要的补充前端 API 调用。

### P7-2: 前端 API 路径 `/api/model/list` 无后端对应路由

**位置**: `apps/desktop-tauri/src/components/ChatView.tsx:484` — 注释提到 `GET /api/model/list`

**问题**: 后端路由列表中只有 `/api/model/info`，没有 `/api/model/list`。可能是注释过时或路由缺失。

**修复方案**: 确认实际使用的 API 路径，更新注释或补充后端路由。

### P7-3: `ChatOverrides` 类型与后端 `SendMessageRequest` 字段同步问题

**位置**: `apps/desktop-tauri/src/hooks/useStreamChat.ts:81-114` vs `crates/hermix-transport/src/lib.rs`

**问题**: `ChatOverrides` 使用 `[k: string]: unknown` 索引签名，无法在编译时检测字段名不匹配。后端新增字段时前端不会报错。

**修复方案**: 移除索引签名，显式列出所有字段。后端生成 TypeScript 类型定义（使用 `ts-rs` 或 `specta`）。

### P7-4: `TodoSnapshotItem` 三端手动同步

**位置**: `apps/desktop-tauri/src/hooks/useStreamChat.ts:7-10` — 注释明确说"keep all three in sync when adding fields"

**问题**: `TodoSnapshotItem` (TS) ↔ `TodoSnapshotItem` (transport) ↔ `TodoItem` (tools) 三个结构体手动保持同步。任何一端新增字段时，其他两端可能遗漏。

**修复方案**: 使用 `ts-rs` 或 `specta` 从 Rust 自动生成 TypeScript 类型。

### P7-5: `WireMessage` 类型使用 `[k: string]: unknown` 但实际有固定结构

**位置**: `apps/desktop-tauri/src/protocolAdapter.ts:56`

**问题**: `WireMessage` 已经定义了大部分字段（id, role, content, timestamp, tool_calls 等），但额外的 `[k: string]: unknown` 使得 TypeScript 无法检测拼写错误（如 `tool_calss` vs `tool_calls`）。

**修复方案**: 移除索引签名，使用 `Exact<T>` 工具类型确保只接受已定义的字段。

---

## 11. P8 — 前端不够人性化

### P8-1: 错误反馈仅通过 console，用户不可见

**位置**: SettingsView.tsx (12处), TitleBar.tsx (3处), Sidebar.tsx (1处), RoomSettingsDrawer.tsx (2处), TodoListCard.tsx (1处)

**问题**: 所有错误仅打印到 console，用户看不到任何反馈。例如模型列表获取失败时，用户只看到空白列表，不知道是正在加载还是出错了。

**修复方案**: 建立全局 toast notification 系统，所有 catch 块通过该系统向用户展示错误。

### P8-2: `.catch(() => {})` 完全静默失败

**位置**: SettingsView (8处), ChatView (3处), Sidebar (1处), App (1处)

**问题**: 操作失败时没有任何反馈。用户切换模型后，如果 `updateSessionMetadata` 失败，UI 显示已切换但后端未保存。重开应用后用户发现设置丢失，且无任何错误记录。

**修复方案**: 至少添加 toast 通知"设置同步失败，请重试"。

### P8-3: 流式错误信息不够具体

**位置**: `apps/desktop-tauri/src/hooks/useStreamChat.ts:739-748`

**现状**: 代码注释提到"2026-07-23 (主人反馈): phase=error/timeout 等终止态时,前端必须有具体错因透出"，并已修复。但 StreamingBubble 的错误显示仍可能被截断。

**修复方案**: 确保 StreamingBubble 的错误区域有足够的宽度和高度显示完整错误信息，并提供"复制错误"按钮。

### P8-4: 缺少加载骨架屏

**位置**: SettingsView.tsx, PluginsView.tsx, AnalyticsView.tsx 等

**问题**: 数据加载时显示空白，加载完成后突然出现内容。缺少 skeleton loading 占位符。

**修复方案**: 为数据驱动的视图添加 skeleton loading 组件。

### P8-5: 审批倒计时无视觉紧迫感

**位置**: `InlineApprovalCard.tsx`, `PendingApprovalToast.tsx`

**问题**: 审批超时倒计时仅显示剩余时间数字，没有视觉上的紧迫感递增（如颜色从黄到红、进度条收缩）。

**修复方案**: 添加进度条动画，剩余时间 < 30% 时颜色变为警告色。

### P8-6: `main.tsx` 启动屏 15s 超时后无错误提示

**位置**: `apps/desktop-tauri/src/main.tsx:43` — `setTimeout(safeDismiss, 15000)`

**问题**: 15s 后强制移除启动屏，但如果后端未启动，用户看到空白应用无任何提示。

**修复方案**: 超时后显示"后端启动超时，请检查日志或重启应用"的错误页面，而非直接移除启动屏。

### P8-7: 无全局错误恢复机制

**问题**: 当 ErrorBoundary 捕获错误后，"重试"按钮仅重置 state，不重新加载数据。如果错误是由数据问题引起的，重试会再次失败。

**修复方案**: ErrorBoundary 的重试应触发数据重新加载，或提供"重新加载应用"和"清除数据并重试"两个选项。

---

## 12. P9 — 冗余组件

### P9-1: `MessageBubble` 与 `groupchat/MessageBubble` 功能重叠

**位置**: `apps/desktop-tauri/src/components/MessageBubble.tsx` (2632行) vs `apps/desktop-tauri/src/components/groupchat/MessageBubble.tsx`

**问题**: 两个 MessageBubble 组件各自实现了消息渲染逻辑，包括头像、时间戳、内容渲染等。虽然业务场景不同（单聊 vs 群聊），但底层渲染逻辑可共享。

**修复方案**: 提取 `MessageBubbleBase` 共享组件，两个场景通过 props 定制。

### P9-2: `VoiceButton` 与 `VoicePlayer` 功能部分重叠

**位置**: `apps/desktop-tauri/src/components/VoiceButton.tsx` vs `VoicePlayer.tsx`

**问题**: 两者都处理音频播放逻辑（`useCallback` 中的 play/stop），音频上下文管理代码重复。

**修复方案**: 提取 `useAudioPlayback` hook 共享音频管理逻辑。

### P9-3: `InlineApprovalCard` 与 `PendingApprovalToast` 审批逻辑重叠

**位置**: `apps/desktop-tauri/src/components/InlineApprovalCard.tsx` vs `PendingApprovalToast.tsx`

**问题**: 两者都渲染审批按钮、倒计时、批准/拒绝逻辑。倒计时的 `setInterval` 逻辑在两处重复。

**修复方案**: 提取 `useApprovalCountdown` hook 和 `ApprovalActions` 共享组件。

### P9-4: `PolishButton` 与 `AiGenerateButton` 按钮模式重叠

**位置**: `apps/desktop-tauri/src/components/PolishButton.tsx` vs `AiGenerateButton.tsx`

**问题**: 两者都是"点击触发 AI 操作 → 显示 loading → 显示结果"的按钮模式，loading 状态管理和错误处理逻辑重复。

**修复方案**: 提取 `useAiAction` hook 或 `AiActionButton` 组件。

---

## 13. 优化计划

### 阶段一: 紧急修复 (1-2 周)

| 优先级 | 任务 | 预估工时 | 影响面 |
|--------|------|----------|--------|
| P0-1 | 为所有顶层视图添加 ErrorBoundary | 2h | 全应用稳定性 |
| P0-2 | 为所有 unsafe 块添加 SAFETY 注释 | 4h | 代码安全 |
| P0-3 | 替换 terminal.rs 中的 unreachable!() | 1h | 稳定性 |
| P0-4 | 关键路径的 .catch(() => {}) 添加错误处理 | 4h | 用户体验 |
| P0-5 | serde_json 解析失败添加日志 | 1h | 可观测性 |
| P0-6 | 修复 BackendStatus 的事件监听泄漏 | 1h | 内存安全 |
| P1-1 | 建立全局 toast notification 系统 | 4h | 用户体验 |
| P2-1 | 修复所有 key={i} | 2h | 性能+正确性 |

### 阶段二: 代码质量提升 (2-4 周)

| 优先级 | 任务 | 预估工时 | 影响面 |
|--------|------|----------|--------|
| P3 | 全量清理"主人/老大"注释 (~131处) | 8h | 可维护性 |
| P3 | 移除版本号和日期戳注释 | 4h | 可维护性 |
| P4-1 | 提取 send_message/send_message_stream 共享代码 | 4h | 可维护性 |
| P4-2~4 | 提取重复辅助函数 | 4h | 可维护性 |
| P4-5 | 提取 SettingsView 重试逻辑 | 2h | 可维护性 |
| P1-4~5 | 修复 as any 和 eslint-disable | 4h | 类型安全 |
| P2-3 | 内联 style 提取为 CSS 类 | 8h | 性能 |
| P2-4 | 硬编码颜色迁移到 CSS 变量 | 4h | 可维护性 |
| P6-1 | 拆分 components.css | 8h | 可维护性 |

### 阶段三: 架构优化 (4-8 周)

| 优先级 | 任务 | 预估工时 | 影响面 |
|--------|------|----------|--------|
| P2-2 | 拆分超大组件文件 (ChatView, App, MessageBubble) | 16h | 可维护性 |
| P4-7 | 拆分超大 Rust 文件 (lib.rs, mod.rs) | 24h | 可维护性 |
| P7-3~5 | 引入 ts-rs/specta 自动生成 TS 类型 | 8h | 类型安全 |
| P9 | 合并冗余组件 | 8h | 可维护性 |
| P2-7 | 添加 React.memo 到顶层组件 | 4h | 性能 |
| P2-5 | setInterval 添加可见性优化 | 4h | 性能 |
| P2-11 | 配置生产构建移除 console | 1h | 性能 |

### 阶段四: UX 提升 (持续)

| 优先级 | 任务 | 预估工时 | 影响面 |
|--------|------|----------|--------|
| P8-1 | 所有 console.error 接入 toast | 4h | 用户体验 |
| P8-4 | 添加 skeleton loading | 8h | 用户体验 |
| P8-5 | 审批倒计时视觉优化 | 4h | 用户体验 |
| P8-6 | 启动屏超时错误提示 | 2h | 用户体验 |
| P8-7 | ErrorBoundary 重试增强 | 4h | 用户体验 |

---

## 14. 对抗性审查总结

### 场景 1: 大规模会话切换

**问题**: 用户快速切换 10+ 个会话时，每个会话触发：
- `useStreamChat` 的 WebSocket 连接/断开
- `ChatView` 的消息列表重渲染（2856 行组件）
- `RightSidebar` 的上下文预览刷新（9 个 useEffect）
- `key={i}` 导致 React 全量 diff

**预期表现**: 明显卡顿，可能触发 React "Maximum update depth exceeded"。

**根因**: 组件未 memo 化 + key={i} + 超大组件 + useEffect 过多。

### 场景 2: 后端 sidecar 崩溃后重启

**问题**: `BackendStatus.tsx` 的事件监听器泄漏（P0-6），重启后多个监听器同时触发，导致重复健康检查请求风暴。

**预期表现**: CPU 飙升，网络请求堆积，可能触发后端限流。

**根因**: 异步 `listen()` 的 cleanup 不正确。

### 场景 3: 长 Agent 循环中的上下文膨胀

**问题**: `send_message` 和 `send_message_stream` 的 300 行重复代码（P4-1），如果只修复了其中一个方法的上下文处理 bug，另一个方法会在长循环中导致上下文无限膨胀。

**预期表现**: 50+ 轮对话后，LLM 上下文超限，API 报错 400。

**根因**: 代码重复导致修复不同步。

### 场景 4: 浅色主题下的错误页面

**问题**: `ErrorBoundary` 的 fallback 使用硬编码暗色颜色（P1-2, P6-2），在浅色主题下显示为暗色块。

**预期表现**: 用户在浅色主题下遇到错误时，看到一个不协调的暗色错误页面。

**根因**: 样式硬编码未使用 CSS 变量。

### 场景 5: 前后端类型漂移

**问题**: `TodoSnapshotItem` 三端手动同步（P7-4），`ChatOverrides` 使用索引签名（P7-3），`WireMessage` 使用索引签名（P7-5）。

**预期表现**: 后端新增字段后，前端不报编译错误，运行时静默忽略新字段，功能不可用。

**根因**: 缺乏自动类型生成机制。

---

## 附录: 文件大小统计

### 前端 TSX/TS (Top 10)

| 行数 | 文件 |
|------|------|
| 2856 | ChatView.tsx |
| 2815 | App.tsx |
| 2632 | MessageBubble.tsx |
| 2091 | builtinExperts.ts |
| ~1400 | SettingsView.tsx |
| ~1200 | RightSidebar.tsx |
| ~1100 | api.ts |
| ~930 | TaskPanel.tsx |
| ~900 | useStreamChat.ts |
| ~800 | protocolAdapter.ts |

### CSS

| 行数 | 文件 |
|------|------|
| 4269 | components.css |
| ~400 | code-view.css |
| ~350 | inline-approval-card.css |
| ~100 | inline-thinking-dot.css |
| 50 | base.css |

### Rust (Top 10, 非测试)

| 行数 | 文件 |
|------|------|
| 8004 | hermix-server/src/lib.rs |
| 5956 | hermix-agent/src/agent_loop/mod.rs |
| 4428 | hermix-agent/src/session_persistence.rs |
| 4380 | hermix-agent/src/compression.rs |
| 3844 | hermix-agent/src/local_agent_service.rs |
| 3266 | hermix-focus/src/embedded.rs |
| 3123 | hermix-tools/src/backends/browser.rs |
| 2684 | hermix-gateway/src/gateway.rs |
| 2382 | hermix-agent/src/run_loop.rs |
| 2246 | hermix-agent/src/provider.rs |

---

*本报告基于 2026-07-24 的代码快照生成。所有问题均通过静态分析 + 模式搜索 + 第一性原理推导发现。*
