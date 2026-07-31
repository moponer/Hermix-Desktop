# Hermix Desktop 前端优化方案

> 审查范围：`apps/desktop-tauri/src/`（React 19 + Tailwind 4 + Vite，117 个 .ts/.tsx 文件）
> 审查方式：三个隔离探查 agent 并行扫描结构 / 样式 / 冗余，主 agent 复核关键铁律违例。
> 结论基调：**架构分层思路是清晰的**（hooks / utils / acui 已分离），但「巨型文件 + 集中式状态 + 单聊群聊双套平行实现 + 样式被内联和 dark 变体绕过」是主要技术债。

---

## 一、问题地图（严重程度总览）

| 维度 | 评级 | 核心证据 |
|---|---|---|
| 文件体量 | 差 | `App.tsx` 3010 行 / `ChatView.tsx` 2982 / `MessageBubble.tsx` 2753 / `data/builtinExperts.ts` 2686 |
| 状态管理 | 差 | `App.tsx` 约 77 处 `useState`，约 129 个内联 `handle*`，无 zustand/Context 收敛 |
| 设计一致性 | 中（有 P0 bug） | `dark:` 变体机制冲突；`AwakeningCard` 蓝紫违例；281 处内联 style |
| 封装复用 | 中 | 双 `MessageBubble`、弹窗各自重写 overlay、重复类型定义 |
| 冗余代码/注释 | 好（轻） | 死代码极少；2 处重复函数、2 处废弃 API、少量调试 log |

---

## 二、设计不统一（P0 — 视觉/主题 bug）

### 2.1 `dark:` 变体机制冲突（**真 bug，最高优先**）
- 现象：全仓无 `@custom-variant dark` 定义，但 `tokens.css / components-helpers.css / ChatView.tsx / AgentAvatar.tsx / MarkdownBody.tsx / ThemeToggle.tsx / CompactionStatusBanner.tsx` 共 7 文件使用 `dark:` 类。
- 根因：Tailwind v4 默认 `dark:` = `@media (prefers-color-scheme: dark)`（媒体查询）。项目实际靠给 `html+body` 挂 `.dark` 类做主题切换（见 `tokens.css:126` 的 `.dark` 块）。两套机制并存 → 这些 `dark:` 样式只在「系统级暗色」下生效，**点主题开关不触发**，与 `.dark` 类开关打架。
- 修复（一行，加在 `globals.css` 顶层）：
  ```css
  @import "tailwindcss";
  @custom-variant dark (&:where(.dark, .dark *));
  ```
  之后所有 `dark:` 类会正确跟随 `.dark` 类，与现有 `.dark` 块统一。

### 2.2 `AwakeningCard` 蓝紫铁律违例（**实锤**）
- 位置：`src/acui/builtin/AwakeningCard.tsx:84 / 96 / 110 / 127 / 129`
- 违例：`bg-purple-500/[0.06]`、`border-purple-400/20`、`text-purple-300`、`bg-purple-300`、`bg-purple-400/70` —— 直接撞「禁用 slate/blue/indigo/violet/purple」铁律。
- 修复：改用 token 语义色（与 `tokens.css` 的 `--color-accent` 同族）。示例：
  ```tsx
  // 前
  className="bg-purple-500/[0.06] border-purple-400/20"
  // 后
  className="bg-accent/[0.06] border-accent/20"
  // 图标
  className="text-accent shrink-0"          // 原 text-purple-300
  className="bg-accent shrink-0"            // 原 bg-purple-300
  className="flex-1 min-w-[6px] bg-accent/70" // 原 bg-purple-400/70
  ```
  （`--color-accent` 在 tokens.css 已定义为 slate 中性 info 色，天然无蓝紫观感。）

### 2.3 281 处内联 `style={{` 绕开设计系统
- 覆盖 52 个文件，重灾区：`TaskPanel.tsx`(25) / `MessageBubble.tsx`(16) / `ChatView.tsx`(15) / `RoomMemberDialog.tsx`(13) / `Sidebar.tsx`(6)。
- 原则：**静态尺寸/颜色/间距收归 token 类或组件 CSS**；仅「动态计算值」（如进度条宽度、按 agent hue 上色）保留内联。
- 建议：先扫一遍，把纯静态内联（如 `style={{ padding: 12 }}`、`color: '#fff'`）替换为已有 token；无法用语义化的补到 `tokens.css`。

### 2.4 动态 hue 调色板含临界蓝相（195）
- 位置：`TaskPanel.tsx:677` 的 `SAFE_HUES`、`groupchat/types.ts:96` 的 `AGENT_HUE_PALETTE`、`RankBadge.tsx:18/41` 兜底 `195`。
- 问题：hue `195` 属青蓝（cyan-blue），临界违例。团队注释已显式规避 220(蓝)/268(紫) 并重映射为 150/340，却漏了 195。
- 修复：把 `195` 重映射为中性/暖色（如 `150` 翠绿或 `175` 青绿），并统一 `RankBadge` 兜底到同一安全集。

### 2.5 备选皮肤重复定义风险
- `src/styles/themes/` 下有 `light / rainforest / sunset / dark` 4 个备选皮肤。需核查是否存在与 `tokens.css` 重复的硬编码值，统一收敛进 token 变量，避免改主色要改两处。

---

## 三、不人性化（UX / 健壮性）

| 问题 | 位置 | 建议 |
|---|---|---|
| `window.alert` 用于「删除会话失败」 | `App.tsx:1091` | 桌面应用改 Toast（项目已有 `ToastContainer`） |
| `alert(...)` 通用弹窗 | `AutomationView.tsx:354` | 同上，统一 Toast / 内联错误条 |
| 临时启动诊断 `console.info` dump localStorage | `App.tsx:416 / 431 / 443` | 已标记「临时」，生产移除或转 `lib/diag.ts` |
| 错误处理内联 | `App.tsx` 内 `localizeStreamError()` | 迁 `hooks/useErrorLocalization` 或 `utils/errors.ts` |

---

## 四、未封装 / 架构债

### 4.1 上帝组件 `App.tsx`（3010 行）
- 约 77 处 `useState/useRef/useMemo/useCallback` + 约 129 个内联 `handle*`；会话 / UI / 审批状态全部堆在根。
- 路线：引入轻量状态库（zustand）或在 `App` 外套 `Context`，把会话态、审批态、UI 态外置；`App` 只做组合与编排。

### 4.2 同样超大的 `ChatView.tsx`(2982) / `MessageBubble.tsx`(2753)
- 以 **400 行为拆分红线**：发送逻辑 → `hooks/useChatSession`；审批逻辑 → `hooks/useApproval`；错误本地化 → `utils/errors.ts`。

### 4.3 单聊 / 群聊双套平行实现
- 双 `MessageBubble`：`components/MessageBubble.tsx` vs `components/groupchat/MessageBubble.tsx`，各自一套卡片体系。
- 重复类型：`components/agents/types.ts` vs `components/groupchat/types.ts`；`agents/storage.ts` vs `agents/providerConfig.ts`。
- 路线：抽共享 `ChatMessage` 渲染层（presentational）供单聊/群聊复用；差异通过 props/插槽注入。统一消息基座后两者 diff 只剩「群聊元数据条」。

### 4.4 弹窗 / 抽屉未统一基座
- 多数弹窗（`RoomMemberDialog` / `PlanConfirmationDialog` / `RoomSettingsDrawer` / `AppDetailModal`）各自重写 overlay 与动画。
- 公共基座 `ImperativeDialog.tsx:12` 目前仅 `GroupChatView.tsx` 复用。
- 路线：**所有弹窗/抽屉强制基于 `ImperativeDialog`**（overlay + 进出动画 + 焦点陷阱一处实现），业务只填内容。

### 4.5 视图切换分支蔓延
- `App.tsx:304` `useState<NavPage>` + 12 个 `*View` 条件渲染。视图继续增多时建议集中式 `navStore` 或轻量路由，避免 `if/else` 蔓延。

---

## 五、冗余代码与注释（轻量，但可顺手清）

| 类型 | 位置 | 处理 |
|---|---|---|
| `formatBytes` 重复 | `utils/imageCompressor.ts:251`(export) vs `components/InlinePathPreview.tsx:47`(本地) | 合并到 `utils/format.ts`，删本地副本 |
| `formatDuration` 重复 | `utils/time.ts:20`(export) vs `components/MessageBubble.tsx:586`(本地，已 import 其他 time 工具却自留) | 复用 `utils/time.ts`，删本地 |
| `agentHsl` ≈ `agentHue` | `TaskPanel.tsx:672` vs `groupchat/types.ts:101` | 确认是否合并（TaskPanel 用 SAFE_HUES 禁蓝紫，属有意分歧，**先确认**） |
| 废弃 API `openSettingsDirectory` | `desktopBridge.ts:140` 无调用方 | 删除 |
| 废弃 API `listTodos` | `api.ts:338` `@deprecated` 且调用方已消失 | 确认无外部引用后移除 export |
| 临时调试 log | `App.tsx:1265 / 1313`（approval REST） | 转 `lib/diag.ts` 或删 |
| 迁移日志 | `storageCompat.ts:131` | 评估是否保留（一次性迁移后多可删） |
| 冗余注释 | `App.tsx:370`(`// 初始化执行一次` 配 `onResize()`)、`Sidebar.tsx:223`(`// TODO: wire to API delete room`) | 自解释注释删；TODO 要么实现要么建 issue |
| 死代码注释 | 全仓 | **基本无需处理**（已核查，极少成片注释代码，是好事） |

> 死代码注释、备份文件（`*.old/bak/v1/v2`）、`debugger` 均零命中 —— 这部分的代码卫生习惯其实不错，不用过度清理。

---

## 六、执行路线（按收益排序）

**P0 — 视觉/主题铁律（半天内，风险低、收益高）**
1. `globals.css` 加 `@custom-variant dark` 一行，修主题开关失效。
2. `AwakeningCard` 5 处 purple → accent 语义色。
3. hue `195` 重映射进安全集。
4. 跑一遍 `pnpm tsc --noEmit` + 目测暗/亮主题切换验证。

**P1 — 架构/封装（按模块渐进，不一次性大改）**
5. 抽 `useChatSession` / `useApproval` / `utils/errors.ts`，先拆 `App.tsx` 最胖的三块。
6. 统一弹窗基座 `ImperativeDialog`，新弹窗一律基于它。
7. 抽共享 `ChatMessage` 渲染层，收敛单聊/群聊双实现。
8. 引入 zustand 或 Context 外置会话/审批态，消除 prop-drilling。

**P2 — 冗余清理（随 PR 顺手）**
9. 合并 `formatBytes` / `formatDuration`；删 `openSettingsDirectory` / `listTodos`。
10. `alert()` → Toast；清临时诊断 log；删自解释注释与陈旧 TODO。

> 建议 P0 单独一个 commit（纯样式/铁律），P1 按子模块分批，避免巨型 diff 难 review。每个 PR 带 `cargo check` + `pnpm tsc --noEmit` 证据（符合 AGENTS.md CI gate 要求）。
