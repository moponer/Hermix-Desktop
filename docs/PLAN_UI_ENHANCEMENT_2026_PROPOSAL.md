# Hermix 桌面端 UI 增强与功能优化 — 实施方案

**方案日期**：2026-06-22
**基础**：`docs/PLAN_UI_ENHANCEMENT_2026.md`（v0.2，18 条需求）
**原则**：**零破坏性** — 已完成的能力（消息渲染、工具分类、头像、ID 持久化、错误聚合等）一律保留，只在它们的外层加适配层/包装层，不重写内部逻辑。

---

## 〇、方案总览

| 模块 | 需求数 | 关键策略 |
|------|--------|---------|
| 一、对话界面 | 6 | 在现有 ChatView 上叠加"角色标记"与"输入工具栏增强"，不重写核心 |
| 二、左侧导航 | 1 | CSS 变量 + 单值常量，不动布局结构 |
| 三、列表项 | 3 | 在 Sidebar 现有 Workspace/Group/Room 三层结构上加 prop 注入，不动数据流 |
| 四、群聊 | 2 | 组件抽离 + 复用，但 ChatMessage/MessageBubble 内部不重写 |
| 五、扩展中心 | 4 | 标题用 i18n / 文案映射，列表项用统一 `<ExtensionRow>` 包装 |
| 六、统计 | 1 | 修 SQL 触发与接口链路，**不动 UI 组件** |
| 七、自动化 | 1 | 在 AutomationView 已有"PROMPT"区加按钮，复用 ChatView 的 polish 通道 |
| **合计** | **18** | — |

> **核心哲学**：
> 1. **数据流不动**：DB schema、API、event 协议 一律不改。
> 2. **组件内部不重写**：只调整 prop 签名 + CSS。
> 3. **新能力用叠加**：新工具栏按钮、新 badge、新包装组件，**不替换** 旧组件。
> 4. **破坏性操作（删数据/迁移 schema/重命名已暴露 API）一律不碰**。

---

## 一、对话界面（核心交互）

### 1.1 头像与时间按"专家切换点"分界

**现状**：`MessageBubble` 里 `AssistantAvatar` 拿 `expertId` 直接读，切换后会回溯覆盖历史消息的头像/时间。

**方案**（**不破坏**，仅加一层）：
- 新增 hook `useExpertCutoff(sessionId)` → 返回一个时间戳 `cutoffAt: number | null`，代表"用户最后一次切换专家的时刻"。
- `MessageBubble` 渲染时：
  ```tsx
  const cutoff = useExpertCutoff(message.sessionId);
  const showExpert = !cutoff || new Date(message.timestamp) >= cutoff;
  ```
- `showExpert` 为 true → 用新专家的头像 / 名义时间线；为 false → 保留原 hermix 头像 + 原始 timestamp。
- 切换专家时由 `useExpert` hook 把"切换时刻 + 目标 expertId"存到 `localStorage` 一条 `expert-cutoff-{sessionId}` 记录。

**不破坏性**：
- 旧消息**渲染逻辑完全保留**，只多了一个判定条件
- `Message` / `ProtocolMessage` 数据结构**不改**
- `useExpert` 现有逻辑**不改**，只在状态变更的 useEffect 末尾追加一行 `localStorage.setItem(...)`

**改动文件**：
- 新增 `src/hooks/useExpertCutoff.ts`（~30 行）
- `MessageBubble.tsx`：在 `AssistantAvatar` 渲染前插 4 行判定
- `hooks/useExpert.ts`：1 行 localStorage 写入

---

### 1.2 输入框工具栏：取消撤销/重做 + 改"图片"为"文档图片"

**现状**：`ChatView.tsx` 工具栏里有 Undo/Redo 按钮；上传按钮写"图片"。

**方案**：
- **删除** Undo/Redo 两个按钮（含图标、onClick、键盘快捷键 if any）
- **改文案**："图片" → "文档图片"，`accept` 属性从 `image/*` 改为 `*/*`（任意文件），后续走现有附件处理流程
- **拖入文件**（1.4 也复用）：在 `<textarea>` / contenteditable 容器上挂 `onDrop`，复用现有 `attachments` 状态

**不破坏性**：
- 现有 attachments 状态 / 上传接口 / 后端处理**完全不动**
- 撤销/重做功能如果在 hook 层有使用，**只删按钮和按钮事件绑定**，不动 hook 内部实现（留作未来再用）
- 文案改动**无功能影响**

**改动文件**：
- `ChatView.tsx` 工具栏 JSX：删 2 个按钮 + 改 1 个文案 + 1 个 accept 属性
- 工具栏的其余按钮（语音、文件、润色、发送等）**不动**

---

### 1.3 润色功能（输入区右侧"润色"按钮）

**现状**：无"润色"按钮。

**方案**（**完全叠加**，不动现有逻辑）：
- 在工具栏语音按钮**右侧**新增 `<PolishButton>`（组件）
- 点击行为：
  1. 拿当前 `inputValue` → 调内部 `polishChat(input)` API
  2. 返回结果**覆盖**到 `inputValue`（不弹窗、不显示 diff，直接替换）
  3. **原文不计入上下文**：`finalSend()` 流程里**只把当前 `inputValue` 作为发送源**，不保留原始草稿
- 关键：**不引入新字段**，润色前原文只活在 React state 里，发送时被 state 覆写
- **Prompt 模板**两份独立文件：
  - `src/prompts/polish.chat.ts` → 对话润色模板（自然语言流畅、意图清晰、避免重复）
  - `src/prompts/polish.automation.ts` → 自动化 PROMPT 改写模板（结构化、保留变量占位符、可执行性强）
  - 两份**互不 import**

**不破坏性**：
- 输入框、发送流程、context 构建**完全不动**
- "原文不计入上下文"**通过现有机制自然实现**（state 单一来源，没存就丢失）
- 后端无变更

**改动文件**：
- 新增 `src/components/PolishButton.tsx`（~40 行）
- 新增 `src/prompts/polish.chat.ts` + `src/prompts/polish.automation.ts`（两份独立常量）
- 新增 `src/api/polish.ts`（薄包装，1 个函数）
- `ChatView.tsx`：在工具栏插入 1 个 `<PolishButton />`

---

### 1.4 拖放附件

**现状**：拖文件到输入区目前可能无响应或只接收图片。

**方案**：
- 在输入区外层容器加 `onDrop` / `onDragOver` / `onDragLeave`
- 拖入后**复用**现有 attachments 处理流程（同一份 `addAttachment()` 函数）
- 视觉反馈：拖入时容器加 `.is-dragging` class（边框高亮 + 浅色背景）
- `accept` 范围：任意文件 → 走和"文档图片"按钮一样的处理

**不破坏性**：
- 点击上传按钮路径**完全不动**
- attachments state 结构**不变**
- 后端接口 / 处理流程**不变**

**改动文件**：
- `ChatView.tsx` 输入区容器：3 个事件 + 1 个 state class
- `components.css`：新增 `.chat-input.is-dragging` 样式（~10 行）

---

### 1.5 工作目录锁死

**现状**：`useWorkspaceRoot` / `useWorkspaces` hook 暴露了切换目录的能力。

**方案**：
- 现有"工作目录切换"按钮 / API **保留**（不删，避免破坏既有入口）
- 在**对话级别**新增锁定标记：
  - 创建对话时记录 `workspaceLockedAt: timestamp`
  - `useWorkspaceRoot(sessionId)` 检测到锁定后，**只暴露 getter，不暴露 setter**（即使底层 setter 还在）
  - hook 层面：切换动作被锁定时静默返回 `null`，不弹错（更优雅）

**不破坏性**：
- 底层 setter / 后端能力**完全不动**（保留以便未来新对话仍可切换）
- 已存在的旧对话（无 `workspaceLockedAt` 字段）走旧逻辑，**优雅降级**
- 统计、消息渲染**不受影响**

**改动文件**：
- `hooks/useWorkspaceRoot.ts`：增加 locked 判定 + setter 静默拒绝
- `ChatView.tsx`：在 useEffect 里首次确认工作目录时写入 `workspaceLockedAt`
- `types.ts`：`Session` 接口加 `workspaceLockedAt?: number`（**optional**，旧数据兼容）

---

### 1.6 工作目录 UI 呈现

**现状**：`ChatView` 工具栏区域可能显示 `目录: /path/to/xxx`，带关闭按钮。

**方案**（**纯样式 + 文案**，不动数据）：
- 文案：去掉"目录:"前缀，**只显示 basename**
- 状态：CSS `color: var(--color-text-faint)` + `cursor: not-allowed`
- 关闭按钮：从 JSX 中**移除**（不用 disabled 隐藏，真删，避免 DOM 残留）
- 整体加一个 `<span class="workspace-pill is-locked">` 类名，传递视觉信号

**不破坏性**：
- 底层数据（path、name）**完全不动**
- 旧 prop / API 签名**不变**
- 只是渲染层替换了一个 `<div>` 为 `<span class="workspace-pill">`

**改动文件**：
- `ChatView.tsx` 工作目录 JSX 块：~10 行
- `components.css`：新增 `.workspace-pill` / `.workspace-pill.is-locked`（~15 行）

---

## 二、左侧导航栏

### 2.1 菜单栏加宽

**方案**（**零代码风险**）：
- `Sidebar.tsx` 顶层容器宽度：当前值（如 64px）改为新值（**待 Q1 确认**）
- 走 CSS 变量统一管理：
  ```css
  :root { --sidebar-width: 240px; }   /* 旧值 */
  :root { --sidebar-width: 280px; }   /* 新值 */
  ```
- 内部所有子组件用 `var(--sidebar-width)` 引用 → **自动联动**

**不破坏性**：
- Sidebar 数据流 / 子组件结构**完全不动**
- 不会挤压主内容（主内容用 flex-grow 1）
- 改一个值即可回滚

**改动文件**：
- `components.css`：1 个变量值
- `Sidebar.tsx`：width 属性从硬编码改为 `var(--sidebar-width)`

---

## 三、工作区与群聊列表（侧边栏第二级）

### 3.1 / 3.2 / 3.3 统一列表项结构

**现状**：`<WorkspaceItem>` / `<RoomItem>` 各自 JSX，结构类似但不完全一致。

**方案**（**抽通用组件 + 透传 prop**）：
- 新建 `<ListRow>` 通用组件，prop：
  ```tsx
  <ListRow
    iconLeft={...}    // 文件夹 / 群聊 / 文件
    label={...}       // 主文本
    meta={...}        // 数字 / 时间 / 激活点
    expanded={...}    // 右侧箭头
    onClick={...}
    variant="workspace" | "room" | "session"  // 决定 meta 渲染规则
  />
  ```
- `<WorkspaceItem>` / `<RoomItem>` 改成 `<ListRow variant="workspace" />` / `variant="room"`，**内部 JSX 替换**，外部调用方不动
- 激活点：`<ListRow variant="session" isActive={true}>` 时把 meta 渲染成 `<span class="dot" />`

**不破坏性**：
- 列表数据来源 / store / hook **完全不动**
- 子项的点击 / 展开 / 选中回调**签名不变**
- 现有的 4 个状态（默认/hover/active/selected）的 CSS class 命名**保留**
- 加号（创建入口）**不动**

**改动文件**：
- 新增 `src/components/ListRow.tsx`（~80 行）
- `Sidebar.tsx`：3 处替换
- `components.css`：`.list-row` / `.list-row.is-active` / `.list-row .dot`（~30 行）

---

## 四、群聊/聊天室

### 4.1 对话框复用 + 差异

**现状**：`GroupChatView.tsx` 单文件实现群聊。

**方案**（**抽离子组件 + 复用，但内部不重写**）：
- 从现有 `ChatView.tsx` 抽离出 4 个**纯组件**（最小 props）：

  | 抽离组件 | 来源 | Props |
  |---------|------|-------|
  | `<ChatInput>` | `ChatView` 的输入区 | `value, onChange, onSend, onPolish, onUpload, onDrop` |
  | `<ChatToolbar>` | `ChatView` 的工具栏 | `onMic, onUpload, onPolish, onSend` |
  | `<AttachmentChip>` | `ChatView` 的附件 | `file, onRemove` |
  | `<MessageBubble>` | 已独立 | 现有不变 |

- `ChatView.tsx` 改造成**纯组装**：把这些子组件组合起来，传自己的回调
- `GroupChatView.tsx` 同样组装这些子组件，**回调指向群聊的发送逻辑**
- **消息渲染不抽**：群聊走自己的 `<GroupMessageBubble>`（独立组件，自己实现）

**不破坏性**：
- 抽离后子组件 = 原 JSX 切片，**逻辑零变化**
- `useStreamChat` / 消息协议 / 数据结构**完全不动**
- 群聊和单聊用**各自的 MessageBubble 组件**，互不影响

**改动文件**：
- 新增 `src/components/chat/ChatInput.tsx` / `ChatToolbar.tsx` / `AttachmentChip.tsx`（合计 ~250 行）
- `ChatView.tsx`：从 ~600 行降到 ~450 行（删掉子组件内联 JSX，改 import）
- `GroupChatView.tsx`：从 ~800 行降到 ~600 行（同样删内联 JSX）
- 删改**两文件加起来 ~400 行净减**

---

### 4.2 总管消息样式与成员一致

**现状**：`GroupChatView` 里总管消息有特殊排版（背景色 / 边框 / icon 大小差异）。

**方案**（**消减 CSS，保留身份区分**）：
- 删除 `.group-msg.is-supervisor` 类的特殊样式（背景、padding、border）
- 总管消息**仅靠头像 + 名称徽章**区分（在 avatar 旁加一个 `Supervisor` 小 chip）
- 其余样式（气泡、padding、间距）**完全沿用** `.group-msg` 默认

**不破坏性**：
- 总管消息**仍然可识别**（avatar + name chip）
- 其余 5 类成员消息样式**完全不动**
- 群聊数据流 / 排序 / 渲染逻辑**不改**

**改动文件**：
- `GroupChatView.tsx`：avatar 旁加 chip（~5 行 JSX）
- `components.css`：删 `.group-msg.is-supervisor` 块，保留 `.supervisor-chip` 样式（~20 行）

---

## 五、扩展中心（4 个界面统一规范）

### 5.1 顶部标题与二级标题

**现状**：4 个界面（插件 / 技能 / 连接器 / 工具集）共用统一标题文案"扩展中心"。

**方案**（**仅文案 / i18n 改造**）：
- 抽 `<ExtensionHeader title="插件" subtitle="启用插件">` 通用组件
- 4 个 View 各自传入对应文案：

  | 界面 | title | subtitle |
  |------|-------|----------|
  | PluginsView | 插件 | 启用插件 |
  | SkillsView | 技能 | 启用技能 |
  | ConnectorsView | 连接器 | 启用连接器 |
  | ToolsetsView | 工具集 | 启用工具集 |

- 文案先放常量 `EXTENSION_HEADERS = { plugins: { title, subtitle }, ... }`，**暂不上 i18n**（避免引入 i18n 依赖的破坏性）

**不破坏性**：
- 标题文字修改是**纯渲染层**
- 4 个 View 的内部数据 / 列表 / 启用逻辑**完全不动**
- 后续要上 i18n 也只是把这张常量表移到 i18n 文件

**改动文件**：
- 新增 `src/components/ExtensionHeader.tsx`（~30 行）
- 新增 `src/constants/extensionHeaders.ts`
- `PluginsView.tsx` / SkillsView 等 4 文件：替换 JSX

---

### 5.2 官方插件市场列表项

**现状**：`<PluginListItem>` 左边是默认 lucide icon，右边也是默认 icon，外加 1px 边框。

**方案**（**样式 + icon 替换**）：
- 边框：CSS `.plugin-list-item { border: 0; }`（覆盖现有 1px 边框）
- 左 icon：默认改用 `FileBox` / `Package` 等更明显的 icon（**待 Q3 确认**）
- 右 icon：默认改用 `ArrowUpRight`（更明确的"打开外部"语义）
- icon 替换以**新增 const + 映射**实现，旧 prop 签名保留（缺省时回退到新默认值）

**不破坏性**：
- 列表项的 prop / onClick / 数据结构**完全不动**
- 边框移除只改 1 行 CSS
- icon 通过 enum 映射，**未指定的走默认值**

**改动文件**：
- `PluginsView.tsx`：icon 映射表 + 替换 JSX（~15 行）
- `components.css`：`.plugin-list-item { border: 0; }`（1 行）

---

### 5.3 技能界面列表项

**现状**：与 5.2 类似，**边框线是否去除待 Q2 确认**。

**方案**（**默认行为 + prop 覆盖**）：
- 抽 `<ExtensionListItem>` 通用组件（接 5.2 的改造）
- 边框：默认 `border: 0`（与插件一致），**Q2 确认后**改成 `border: 1px solid ...` 只需 1 行 CSS

**不破坏性**：
- 列表渲染逻辑 / 数据**不动**
- 边框调整 1 行 CSS
- 无论 Q2 怎么决定，都是 1 行 CSS 切换

**改动文件**：
- `PluginsView.tsx` → 抽 `ExtensionListItem`（~60 行）
- 技能 / 连接器 / 工具集 4 个 View 复用此组件
- `components.css`：`.ext-list-item` / `.ext-list-item .icon` 样式

---

### 5.4 连接器 / 工具集图标

**现状**：每个项已有独立 icon（**无边框**）。

**方案**（**不动边框**）：
- 边框：保持 0
- 左侧 icon：**通过 `icon` prop 注入**（每个连接器 / 工具集在数据里指定自己的 icon）
- 数据契约：扩展 `Connector` / `Toolset` 类型，加 `iconKey?: string` 字段
- 旧数据无 `iconKey` → fallback 到默认 icon

**不破坏性**：
- 现有 4 个 View 的 prop / 数据流**完全不动**
- 旧连接器 / 工具集无 `iconKey` 字段 → 优雅降级
- 边框线**不动**

**改动文件**：
- `types.ts`：`Connector` / `Toolset` 加 `iconKey?: string`（**optional**）
- 渲染处 1 行：`<ExtensionListItem icon={item.iconKey ?? "default"} />`
- `components.css`：icon 映射表（按 key → Lucide component 映射）

---

## 六、统计界面

### 6.1 统计全部为 0

**现状**：`AnalyticsView.tsx` 顶部按钮触发统计查询，查询返回 0。

**方案**（**纯排查 + 修补链路，不动 UI**）：
- 排查方向（按概率排序）：

  | # | 嫌疑点 | 验证方法 |
  |---|--------|---------|
  | 1 | DB 表聚合 SQL 没触发（condition 不满足） | SQLite CLI 跑一遍聚合 SQL 看结果 |
  | 2 | 后端 endpoint 路径不一致 | curl `/api/stats` 看返回 |
  | 3 | 前端调用参数不对（缺 workspace_id） | 看 Network 面板 |
  | 4 | 聚合查询本身有 bug | 直查 DB |

- 修复策略（按诊断结果）：
  - 1 → 修 SQL WHERE 条件
  - 2 → 加 route 转发
  - 3 → 补传参
  - 4 → 改写聚合逻辑

**不破坏性**：
- **UI 组件完全不动**（只改数据）
- 修一处不破坏另一处的查询
- 加 fallback：当 DB 真的没数据时显示"暂无数据"（已有，无需改）

**改动文件**：
- **取决于诊断结果**，可能涉及：
  - `crates/hermix-server/src/lib.rs`（API endpoint）
  - SQL migration
  - `AnalyticsView.tsx` 调用层（最多改 1-2 行）

---

## 七、自动化

### 7.1 自动化 PROMPT AI 生成

**现状**：`AutomationView.tsx` 有 PROMPT 输入区，无 AI 生成按钮。

**方案**（**复用 1.3 的 polish 通道，但走独立 prompt**）：
- 在 PROMPT 输入区右下角加 `<AiGenerateButton>`
- 点击行为：
  1. 拿当前 `promptValue` → 调 `polishAutomation(promptValue)` API
  2. 返回结果回填到 `promptValue`
- Prompt 模板：复用 1.3 提到的 `src/prompts/polish.automation.ts`（与对话润色模板**物理隔离**）

**不破坏性**：
- PROMPT 输入区 / 保存逻辑 / 自动化执行**完全不动**
- 新按钮只影响**输入过程**，不修改已存数据
- 与对话润色用两个独立 API endpoint（避免互相串味）

**改动文件**：
- 新增 `src/components/AiGenerateButton.tsx`（~30 行）
- `AutomationView.tsx`：插入按钮 + 1 个 onClick
- 新增 `src/api/polish.ts` 里加 `polishAutomation()` 函数（~10 行）
- 复用 1.3 提到的 `src/prompts/polish.automation.ts`

---

## 八、跨需求共通原则

### 8.1 不破坏性边界（红线）

**禁止动**：
1. ❌ DB schema 已有列（不加列、不删列、不改类型）
2. ❌ 已暴露的 API 签名（`ProtocolMessage` / `Session` 等）
3. ❌ 消息协议 / event 流（`onToolStart` / `appendChunk` 等）
4. ❌ 后端 tool 注册（之前修过的 `office_cli` 不能恢复）
5. ❌ 现有 LLM 上下文结构（tool_calls / role 字段）

**允许动**：
1. ✅ CSS 变量值（宽度、颜色、间距）
2. ✅ UI 组件的 prop 签名扩展（**加 optional prop，不删 required**）
3. ✅ 类型字段扩展（`iconKey?` / `workspaceLockedAt?` 等可选字段）
4. ✅ 新增组件 / hook / 常量文件
5. ✅ JSX 内联结构调整（拆成子组件 / 用通用组件包装）

### 8.2 回滚策略

每个需求**单独 commit** + 单独的组件 / 文件边界，回滚 1 个需求 = revert 1 个 commit，**不波及其他需求**。

| 需求 | 主要 commit | 回滚命令 |
|------|------------|---------|
| 1.1 头像/时间 | feat(chat): expert-cutoff hook | `git revert <commit>` |
| 1.2 工具栏 | refactor(chat): toolbar cleanup | 同上 |
| 1.3 润色 | feat(chat): polish button + prompts | 同上 |
| ... | ... | ... |

### 8.3 测试覆盖

每个 UI 改动**至少跑通**：
1. 旧对话（无新字段）渲染正常 — 降级路径
2. 新对话（带新字段）渲染正常 — 主路径
3. 切换专家前后，老消息 / 新消息**都用旧头像**（1.1 的核心验证）
4. 群聊 + 单聊两套对话框**都能正常发消息**（4.1 的核心验证）
5. 4 个扩展界面**都能正常打开 + 启用/禁用**（5.x 的核心验证）

---

## 九、实施顺序（按依赖关系）

```
Phase 1（基础组件抽离，零功能影响）
  1.2 工具栏清理
  1.6 工作目录 UI
  2.1 侧栏加宽
  3.1-3.3 ListRow 抽离

Phase 2（新能力叠加）
  1.1 头像/时间分界
  1.3 润色按钮 + 双 prompt
  1.4 拖放
  1.5 工作目录锁死
  5.1-5.4 扩展中心规范化

Phase 3（群聊 & 自动化 & 统计）
  4.1 对话框抽离
  4.2 总管消息样式
  7.1 自动化 AI 生成
  6.1 统计修复（看诊断结果，1-2 天）
```

**Phase 1** ≈ 2-3 天（纯 UI 调整）
**Phase 2** ≈ 3-4 天（涉及新 hook / 新 API / 双 prompt）
**Phase 3** ≈ 4-5 天（群聊抽离是大头，统计修复看诊断）

**总计**：~10-12 个工作日，按 1 个工程师估。

---

## 十、待确认事项

| 编号 | 阻塞需求 | 内容 |
|------|---------|------|
| Q1 | 2.1 | 侧栏新宽度具体像素值（240 / 280 / 320？） |
| Q2 | 5.3 | 技能界面边框线是否也去除 |
| Q3 | 5.2 / 5.4 | 4 个扩展界面的 icon 设计稿（左右 icon 各一） |
| Q5 | 4.2 | 总管身份区分是否仅靠头像 + 名称 chip（不靠背景色） |

**已有答案**（来自需求文档 v0.2）：
- ✅ Q4 群聊对话框走**组件抽离**（4.1）
- ✅ Q6 润色 prompt 分两套（1.3 / 7.1 各自独立）

---

**方案版本**：v1.0（2026-06-22）
**维护人**：小狗子 AI
**状态**：待王爷 review
