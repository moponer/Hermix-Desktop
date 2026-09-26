# 工作台对话栏产品级重设计方案 v1

> 配套 `WORKBENCH_MODULE_DESIGN.md`：那份讲代码模块、UI 工程纪律；这份讲**对话栏**——它是用户与工作台唯一的对话入口，也是 LLM 唯一知道"现在在哪个工作台里"的窗口。
> 本文只做方案设计，不含实现。

---

## 〇、当前对话栏的根本问题（主人 2026-09-21 指出）

主人对截图的两个判断：

1. **「右侧对话栏跟主对话看起来一样，很怪」** —— 视觉/功能完全无差异化，用户感受不到「工作台专属」。
2. **「右侧对话栏是否知道当前工作台的基础信息和代码落入哪里？提需求后能熟练开发吗？」** —— LLM 收不到工作台上下文，**基本不知道**当前是哪个工作台、文件在哪、有哪些已有能力、要遵循什么标准。主人说"开发吧"——其实**不能**。

这两个判断从现场探查都成立。下面是根因和方案。

---

## 一、现状剖析

### 1.1 视觉与功能：右侧对话栏 = 主对话

| 维度 | 当前实现 | 用户感受 |
|---|---|---|
| 渲染组件 | `<WorkbenchShell>` 里直接渲染宿主那份 `chatNode`（`<ChatView>` 实例） | 跟主对话一模一样 |
| 顶部 chrome | 主对话的标题行 / 工具栏 | 没有"工作台"标识 |
| 模型选择 | 主对话的全局 model picker | 跟主对话共用一个 LLM 配置 |
| 输入框 placeholder | "输入消息..." | 看不出是"跟工作台对话" |
| 工作台专属操作 | **没有** | 用户提需求后无"应用到工作台"按钮或状态反馈 |

### 1.2 LLM 上下文：右栏对话跟主对话的 system prompt **完全相同**

`WorkbenchView.open()` 只做了：
- `createSession(...)` 建会话
- `workbenchEnsureSession(id, sessionId)` 写 manifest.session_id
- `updateSessionMetadata(sessionId, { workbench_id })` 给会话盖戳
- `onSelectSession(sessionId)` 切 activeSessionId

**没有**任何一处把工作台上下文（manifest / 代码目录 / 模块索引 / importmap / 开发标准）注入 LLM 的 system prompt。

结果：用户在工作台右栏问「这个工作台的文件结构是啥」，LLM 答「我不知道」—— 因为它确实不知道。LLM 拥有的只有 24 个 `workbench_*` 工具的 schema，不知道：
- 当前工作台叫什么、id 是什么、依赖是什么
- 代码落在 `~/.hermix/workbenches/<id>/code/`
- 已有模块清单（避免重复造轮子）
- 已安装的 vendor 依赖
- workbench skill 规定的骨架、契约、单文件行数上限、命名规范、importmap 维护方式

### 1.3 工作台 skill 现状

`~/.hermix/skills/software-development/workbench/SKILL.md`（或类似路径）存在，但**没有自动注入到工作台会话的 system prompt**。LLM 收不到这份契约。

主人截图里 LLM 回答"一个空工作台，你随便取个名字"——其实**没有遵循 skill 规定的"先列区域、等用户确认再动手"流程**，正是因为 skill 没注入。

---

## 二、设计目标

| 目标 | 验收 |
|---|---|
| **T1 视觉差异化** | 用户看到右栏能立刻知道"这是工作台对话，不是主对话"；产品级差异，不只是换个色 |
| **T2 上下文透明** | LLM 在工作台对话中**自动知道**当前是哪个工作台、文件结构、已有能力、开发标准 |
| **T3 上下文实时** | 工作台代码改动（agent 加了文件、删了文件）后，下一轮 LLM 调用看到的是最新状态 |
| **T4 不污染主对话** | 工作台上下文**只注入** `session.workbench_id IS NOT NULL` 的会话；主对话 system prompt 不变 |
| **T5 错误友好化** | 工作台对话栏内部错误（孤儿 session、PATCH 失败、上下文拼装失败）**不堆 UI**；走 toast / 自动降级 |

---

## 三、方案 A：右侧对话栏视觉/功能差异化

### A.1 抽 ChatCore，把 chrome 留给宿主

当前 `<ChatView>` 把"消息列表 + 输入 + 工具调用渲染"和"会话标题 + 切换 + 新建"两件事缠在一起。重构：

```
ChatView (主对话)
├── <ChatCore>             ← 抽出：消息列表 / 输入框 / 工具调用渲染 / 流式状态
│   └── props: messages, onSend, onStop, streaming, model, ...
└── 主对话 chrome (标题、模型选择器、会话切换器、新建按钮、侧栏入口)
    └── props: sessionTitle, onChangeTitle, ...

WorkbenchChat (工作台对话)
├── <ChatCore variant="workbench">    ← 同一份 ChatCore
│   └── props: + workbenchContext (manifest 摘要 / 当前文件 / 当前动作)
└── 工作台 chrome
    ├── 顶部 chip: 「工作台：<name>」+ 简要描述
    ├── 模型选择器：复用，缩到右上角
    ├── 输入框 placeholder: 「告诉工作台要加什么 / 改什么」
    ├── 底部状态条：「已应用 N 个文件 / 上次改动时间」
    └── 「应用到工作台」按钮（agent 回复结束后可点一下确认）
```

**关键点**：
- `<ChatCore>` 是纯渲染逻辑组件，**不分主对话/工作台**
- chrome 是**独立的两套**：主对话 chrome + 工作台 chrome
- 工作台 chrome **不再显示**「会话标题」「切换会话」「新建对话」按钮——工作台只属于一个工作台，这些都是噪音
- 工作台 chrome **新增**「应用到工作台」按钮 + 「已应用 N 个文件」状态条

### A.2 工作台专属 chrome 字段

```tsx
<WorkbenchChat
  workbenchId={manifest.id}
  workbenchName={manifest.name}
  workbenchDeps={manifest.deps}
  lastChange={lastChange}        // 来自 workbench://changed 事件
  chatCoreProps={...}            // 透传给 ChatCore
/>
```

> 修订 2026-09-21：`appliedFiles` / "应用到工作台"按钮**删除**——agent 改完代码经 `workbench://changed` 自动热更新，按钮是冗余 UI（§九问 4 已定：先不做）。`lastChange` 状态条保留，足够给用户"生效了"的反馈。

### A.3 风险

- **R1：抽 ChatCore 工作量不小**。`<ChatView>` 有约 2000 行（含消息渲染、工具调用、压缩、imagemarker 等），要把"消息流"和"会话 chrome"拆干净。建议**先抽接口边界**（找出 props 列表），再逐步迁移，**不一次重写**。
- **R2：模型选择器是否全局统一**。当前是全局 model picker（host 状态）。工作台对话栏复不共用？**建议共用**——换模型是用户级偏好，不是会话级。但放在工作台 chrome 里要缩小、放进顶部 chip 区。

---

## 四、方案 B：LLM 工作台上下文自动注入

### B.1 触发条件

仅当请求会话 `workbench_id IS NOT NULL` 时启用。`workbench_id IS NULL`（普通对话）**不注入任何工作台相关内容**。

### B.2 注入位置（修订 2026-09-21：sidecar 字符串追加 → hermix-agent directions）

**原文方案**：在 `hermix-server` 新建 `workbench_context.rs`，在 `send_message_stream` 入口把工作台上下文**字符串追加**到 system prompt。

**修订**：不要在 server 层开第二条 prompt 拼装路径。已核对的代码现状：

- system prompt 拼装已集中在 `hermix-agent/src/prompt_assembly.rs`（`build_system_prompt` + `directions.rs` 的 `SystemDir`/`DirRoute` 机制，含 per-turn 审计）；
- `hermix-transport/src/lib.rs:226` 的 `SendMessageRequest` **已有 `workbench_id: Option<String>`** 字段，请求一路带进 agent 层；
- `hermix_plugin::workbench` 已导出 `read_manifest` / `module_index` / `format_module_index` / `vendor_index`，拼装原料现成。

正确做法：在 `directions.rs` 新增 `workbench.context` 的 `SystemDir`，route 条件为 `workbench_id.is_some()`（route_ctx 需透传该字段），内容用上述现成函数拼装。防泄漏测试落在 hermix-agent（有 workbench_id 的会话 prompt 含 `## WORKBENCH CONTEXT`；无则不含），比在 server 层测日志可靠。

```
[系统提示词（原有，经 prompt_assembly 正常分层）]
---
## WORKBENCH CONTEXT（workbench.context direction，仅 workbench 会话）
### 工作台信息
- id: workbench-1789972414513
- name: 网址收藏
- 依赖（vendor）: [dayjs@1.11.10, sortablejs@1.15.0]
### 代码目录
~/.hermix/workbenches/workbench-1789972414513/code/
### 已有模块（format_module_index 输出）
- src/utils/format.js → formatDate, formatRelative, formatNumber
...
### 已装 vendor（vendor_index 输出）
- dayjs → /_vendor/dayjs/index.js
...
---
[conversation history]
```

注意：module_index / vendor_index 反映**最新**代码状态（见 B.3），不属于 `build_stable_layer` 的稳定层，必须走 per-turn 评估的 directions 路径。

### B.3 拼装时机（修订 2026-09-21：删除缓存体系，每轮重拼）

**原文方案**：manifest 按 workbench id 缓存（10s TTL）+ module_index 写盘后失效（mtime 校验）+ vendor_index 按索引 mtime 校验。

**修订**：**全部删除，每轮重拼。** 理由：

- `module_index()` 就是扫一个本地小目录（`workbench.rs:133`），`read_manifest` 读一个小 JSON，vendor 同理——全部是毫秒级本地磁盘操作；
- 每轮重拼使"缓存陈旧导致 LLM 用旧 module_index 写重复功能"（原 R4）这个风险**直接消失**，而不是被缓解；
- 上下文实时性（T3）自动成立：agent 写完文件，下一轮 directions 评估时扫到的就是新目录，无需失效钩子。

不需要事件监听、不需要 mtime 比对、不需要 TTL。为毫秒级本地扫描建缓存体系是负收益。

### B.4 skill 注入（修订 2026-09-21：删除"全文 vs 摘要"与关键词路由，对齐 MODULE_DESIGN §13）

**原文方案**：首轮拼 skill 全文、后续拼摘要 + 按用户问题关键词路由注入对应章节，"匹配不上就拼摘要"。

**修订**：**整节废弃，直接复用既有机制。** 两个事实：

1. **实际体量**：`~/.hermix/skills/workbench/SKILL.md` 全文 190 行（约 2-3K tokens），不是原文假设的"5K+ tokens"。为 190 行文档建一套关键词→章节路由，是为不存在的问题造机制。
2. **通道已设计并核对过源码**：`WORKBENCH_MODULE_DESIGN.md` §13.2 第 3 层——工作台 session 的 `AgentOverrides.skills` 恒为 `["workbench"]`，`prompt_assembly.rs:166` 会把 SKILL.md 全文 inline 进系统提示。右侧对话栏的 session 本来就只为该工作台服务，skill 常驻零意图识别成本。

因此 B 方案的注入内容收敛为两件正交的事，互不重复：

| 内容 | 通道 | 时机 |
|---|---|---|
| 开发标准 / 骨架契约 / lint 规则（skill 全文） | `AgentOverrides.skills = ["workbench"]`（MODULE_DESIGN §13 层 3） | 稳定，随 session 常驻 |
| 动态状态（manifest / module_index / vendor_index） | `workbench.context` direction（本方案 B.2） | 每轮重拼，永远最新 |

skill 负责"怎么写"，context 负责"现在有什么"，分工清晰，无需路由、无需摘要。

### B.5 实施位置（修订 2026-09-21：server 层 → hermix-agent 层）

| 层 | 改什么 |
|---|---|
| `crates/hermix-agent/src/directions.rs` | 新增 `workbench.context` 的 `SystemDir`，route 条件 `workbench_id.is_some()`；拼装用 `hermix_plugin::workbench` 的 `read_manifest` / `format_module_index` / `vendor_index`（hermix-agent 需引入 hermix-plugin 依赖，或经 route_ctx 注入闭包，取依赖方向更干净者） |
| `crates/hermix-agent/src/agent_loop/` | `route_ctx`（collect_directions 的路由上下文）透传 `workbench_id: Option<String>`，来源是 `SendMessageRequest.workbench_id`（`hermix-transport/src/lib.rs:226`，字段已存在） |
| skill 常驻 | 前端 `WorkbenchView.open()` / server 侧为 workbench 会话的 `AgentOverrides.skills` 恒置 `["workbench"]`（MODULE_DESIGN §13 层 3，非新代码路径） |
| `crates/hermix-server/src/session_persistence/schema.rs` | 不变（workbench_id 字段已存在） |
| 新增测试（hermix-agent） | ① workbench 会话 system prompt 含 `## WORKBENCH CONTEXT` 且含 module_index 实内容；② 普通会话 prompt **不含**该段（防泄漏，钉死 T5）；③ agent 写入新文件后，下一轮 prompt 的 module_index 包含新文件（T3） |

### B.6 风险

- **R3：token 成本**。砍掉缓存与路由后，每轮动态注入只剩 manifest + module_index + vendor_index（小工作台几百 tokens，随模块数线性增长）；skill 全文 2-3K tokens 常驻。工作台对话比普通对话贵是**结构性的且值得的**——没有这些它就是不能开发。原"给用户关闭注入的开关"随之撤销：关掉 = 工作台不能开发，那不是设置项是自废功能。
- **R5：注入泄漏到主对话**。这是产品 bug。防线在 hermix-agent 层的 route 条件（`workbench_id.is_some()`），配 B.5 测试 ② 钉死。方向修正后防线比原来的 server 字符串追加更靠近唯一拼装点，主对话路径天然不含该 direction。
- **R6（新增，对抗性审查）：workbench_* 工具集的会话隔离**。原文档只管了"上下文注入"，没管"工具作用域"。若 24 个 `workbench_*` 工具在**普通主对话**也注册，主对话里的 LLM 可以在没有工作台上下文的情况下对 `~/.hermix/workbenches/<id>/code/` 写代码——绕过整个 B 方案，且写出来的代码不走"知道自己模块索引"的纪律。**落地时必须确认：`workbench_*` 工具只在 `workbench_id IS NOT NULL` 的会话注册进 toolset**；与 R5 同级别的边界泄漏问题，同一组测试钉死。

---

## 五、方案 C：空态画布视觉整合（主人 2026-09-21 第三次追问）

> 主人原话：「还有一个工作台的画布在没有工作台的时候的显示？纯白色与项目主题割裂感严重。」

主人这一刀切中了**当前空态真正的产品问题**——前两节 A + B 都在谈对话栏，**画布本体**反而被忽略了。本节跟 A、B 独立、必须单独立项。

### C.1 根因（已经现场验证）

查 `apps/desktop-tauri/src/styles/` 现状：

| 层 | 背景色来源 | light 主题实际值 |
|---|---|---|
| `.pane-main` | `var(--theme-bg-chat, var(--color-bg-primary))` | `#faf9f5`（暖米 Parchment） |
| `.wb-canvas` | `var(--color-bg-primary)` | `#faf9f5`（同色） |
| `.wb-empty` | **未设 background** → 继承 `.pane-main` | `#faf9f5` |
| `.wb-empty-prompt` | `var(--color-bg-card)` | **`#faf9f5` ← 跟底色同色！** |
| `.wb-input` | `var(--color-bg-input)` | **`#faf9f5` ← 跟底色同色！** |
| `.wb-btn` | `var(--color-bg-primary)` | **`#faf9f5` ← 跟底色同色！** |
| `.wb-canvas-empty` ("工作台入口未就绪") | **未设 background** | 继承 .wb-canvas |

**真相**：
- **token 值是对的**（暖米），**不是"白色"**
- 但 **prompt / input / button 全部跟底色同值** + **边框极弱**（`--color-border: #d1cdc0` Border Warm）→ **视觉上完全没层次**
- 主人看截图的「白」≈「空态里没看到任何东西区分出卡片/输入框/按钮」—— 整片 **"漂在空白里"**
- "跟项目主题割裂" = **主人看不到主题色应有的"发丝边框 + 三层 surface 明度阶梯"**

### C.2 设计目标

| 编号 | 目标 | 怎么做 |
|---|---|---|
| **C-T1** | 空态整体融入当前主题 | 容器背景 **不另起**——继承 `.pane-main` |
| **C-T2** | 内容块必须有「框感」 | prompt / input / button 用 `--color-bg-elevated` 或 `--color-bg-card-hover` 跟底色**拉开 1-2 级明度**，配 `--color-border-primary` 发丝边框 |
| **C-T3** | 错误不堆底部 | 改 toast 或 input 下方 inline；不污染空态主体 |
| **C-T4** | 删"复制 PROMPT"块 | 主人上轮已确认这块是噪音，A 节也建议删 |
| **C-T5** | 工作台"未就绪"占位视觉对齐 | `.wb-canvas-empty` 跟 WorkbenchEmpty 走同一套 token 层级 |

### C.3 新空态布局（合并 A 节"重设计"）

```
┌──────────────────────────────────────────────┐
│                                              │
│       工作台                                  │ ← h2 17px 暖灰黑
│                                              │
│   给它起个名字，做什么用稍后跟它聊     │ ← lead 13px 暖灰
│                                              │
│   ┌──────────────────────────────────┐       │ ← container:
│   │ [ 起名 input            ] [新建]  │       │   bg-elevated + border-primary + radius-lg
│   └──────────────────────────────────┘       │   跟底色差 1-2 级明度
│                                              │
│   例：项目管理 · 联系人 · 记账               │ ← text-tertiary 微弱
│                                              │
│   💡 右栏对话里跟它说你要啥                  │ ← 可选 chip（tertiary）
│                                              │
└──────────────────────────────────────────────┘
```

要点：
- **一个 input + 一个 button**，Enter 即建，**零教学**
- **container 用 token 层** —— bg-elevated + border-primary，跟底色有明显但微妙的区分
- **删掉"复制 PROMPT"块** + 删掉"两步"分块
- **错误**走 toast + input 下方 inline（不堆底部）
- **三词示例**用 `--color-text-tertiary` 不抢眼

### C.4 token 校验与可能改动

**问题**：现在 `--color-bg-card / input / primary / elevated` 都是 `#faf9f5`（同色）—— **根本没有"elevated"**

**最小修法**（**推荐**）：
- 在 `tokens.css` 把 `--color-bg-card` 改成 `#f4f2eb`（**降 2-3 级明度**），跟 `--color-bg-primary` 拉开
- 在 `tokens.css` 把 `--color-bg-elevated` 改成 `#ffffff` 或更亮一档（**升 2-3 级明度**），给"凸起"层用
- 这是**全 token 共享改动**，**必须全局扫一遍影响面**：
  - Sidebar 的卡片（`bg-card`）
  - ChatView 的卡片
  - Settings / Agents 面板
  - 主题切换（rainforest / sunset / dark）也要走同 token，**统一偏移**

**保守方案**（**先做**）：
- **不动 token**，只用现有 token + `--color-border-primary` 发丝边框 + 微妙的 `--color-bg-hover` 做容器背景
- 视觉效果弱一些（hover 色跟 primary 同明度），但**零回归**

**激进方案**（**后做**）：
- 修 token，让 card / elevated / primary 三层有真实差异
- 需要主人拍板"暖米要保持整体温度感、不能像传统 gray 那样分 5 层"

### C.5 工作台"未就绪"占位统一

`.wb-canvas-empty` ("工作台入口未就绪") 现在用：
```css
.wb-canvas-empty {
  display: flex; align-items: center; justify-content: center;
  width: 100%;
  color: var(--color-text-secondary); font-size: 13px;
}
```

**问题**：
- 无 background、无 border、无 icon
- 在 `.wb-canvas` 里**单独**渲染（iframe 还没就绪时）
- 跟 WorkbenchEmpty 视觉**完全不一致** —— 两个空态两种风格

**新设计**：
- **同一个 token 层级**（border + bg-card）
- 同样用 typography token
- 中心一个 icon（lucide `package-open` 之类）+ 文字"工作台入口未就绪"

### C.6 主题切换稳定性

- rainforest（暗色）：`--color-bg-primary` = `#121a16`，**空态继承 pane-main 即可**
- sunset（暗色）：`--color-bg-primary` = `#26211b`，同上
- **不要**为暖米主题硬编码任何颜色 —— **全部走 token**

### C.7 风险

| 编号 | 风险 | 缓解 |
|---|---|---|
| **C-R1** | 改 `--color-bg-card / elevated` 影响全站（Sidebar / ChatView / Settings） | **保守方案先做**（仅用现有 token + 边框）；激进方案要做**全 token 影响面扫描** + Playwright 截图回归 |
| **C-R2** | WebView2 缓存让主人看不到新色 | 验证时让主人**硬刷或清缓存**（`chrome://settings` 站点数据清空）；确认是 dev 窗口而非装版 |
| **C-R3** | 装版 vs dev 混淆 | 主人截图前先 `netstat -ano | grep 1420`（之前提过的"秒判方法"）—— 不连 :1420 就是装版 |
| **C-R4** | token 改了 rainforest / sunset 主题出问题 | 在三个主题下**全部截图回归** |

---

## 六、方案 D：工作台运营对话（主人 2026-09-21 追加）

> 主人要求：右栏对话**不只是开发工具**——用户在对话里可以直接**完成工作台内的业务工作**
> （"把这周的台账记一下"、"每天早上 8 点把昨日数据汇总发我"），可能是即时指令也可能是定时任务。
> 定位：右栏 LLM/agent **深度绑定当前工作台**，对它的一切了如指掌——既是开发者，也是**运营者**。

### D.1 定位：开发对话 vs 运营对话

同一个 session、同一个 LLM，两类意图共存：

| | 开发对话（方案 B 覆盖） | 运营对话（本方案新增） |
|---|---|---|
| 典型问句 | "加个导出 Excel 功能" | "把今天收到的 3 张发票录进去" |
| 改动对象 | `code/`（写文件 → lint → 热更新） | `data/`（经网关读写业务数据） |
| 生效方式 | `workbench://changed` 热更新 | 数据即时生效，画布自己刷新 |
| 上下文需求 | 代码结构（module_index / skill） | **业务数据现状**（表结构、当前数据摘要、未完成事项） |
| 执行通道 | `workbench_write` 等 24 个工具 | 网关读写 + agent 推理（见 D.3） |

关键洞察：**开发用的是"眼睛"（代码结构），运营用的是"手"（数据通道）**。B 方案已给眼睛，本方案给手。

### D.2 运营上下文注入（B.2 的运营面扩展）

`workbench.context` direction 在拼装时**追加运营段**（同一次拼装，不新增路径）：

```
### 业务数据现状（运营上下文）
- 数据库: data/workbench.db，业务表:
  - ledger(id, date, amount, category, note) — 128 行，最近: 2026-09-20 晚餐 -45
  - categories(id, name, budget) — 12 行
- 待办/未完成: 无（或从数据推导，如"3 条分类无预算"）
- 数据操作规则: 只经 wb.db 网关，schema 迁移走开发对话，运营对话不改表结构
```

拼装规则（沿用 B.3 决策，无缓存）：

- `db_query` 查 SQLite master + 各表 `COUNT(*)` + 每表**末 3-5 行**样本——轻量查询，毫秒级；
- 大表**不全量注入**：行数超阈值（如 500）只给 schema + count + 样本，具体数据 agent 需要时用工具现查；
- 数据是易变态，**每轮重拼**与 module_index 同批，无失效问题。

### D.3 运营执行的通道：agent 直操作数据

**原则：运营动作 = agent 用现有网关工具读写 `data/`，不生成新代码、不碰 `code/`。**

| 动作 | 通道 | 说明 |
|---|---|---|
| 录入/修改业务数据 | `workbench_gateway::db_exec`（已落地，参数化，写前有约束） | agent 组参数化 SQL，同 LLM 写 SQL 的一致纪律 |
| 查询/汇总/统计 | `db_query`（只读） | 复杂统计让 agent 写 SELECT，网关只读连接天然安全 |
| 文件类工作台数据（如导入 CSV） | `fs_read` / `fs_write` 网关 | 已有原子写 |
| 需要新算法（如列统计） | 不在运营对话写代码；缺能力走**开发对话**加模块或 `wb-tools` | 边界：运营不含 `write_code_file` 调用 |

**边界铁律**：运营对话中 agent **不得**调用 `workbench_write`（写代码）。若用户请求实际需要改代码（"帮我每天自动导出"需要新模块），agent 应**转开发意图**，在对话里明说"这需要加个功能，我来改代码"——两类意图可在一个对话里交替，但每次工具调用只属于一类。

### D.4 定时任务

**复用既有基础设施，不造新调度器。** 项目已有 `hermix-cron`（`crates/hermix-cron/`）与 `hermix-automation`（自动化触发）。

| 需求 | 机制 |
|---|---|
| "每天 8 点汇总昨日数据" | 注册 hermix-cron 任务：目标 = 向该工作台 session 投递一条**系统注入的用户消息**（如 `[定时任务] 执行"每日汇总"`），走正常 agent loop，agent 用 D.3 通道执行 |
| 任务声明 | agent 在对话中与用户确认后，调 `cron_create`（含 prompt + schedule + session 绑定） |
| 可见性 | 右栏对话里可问"我有哪些定时任务"，agent 查 cron 列表回答；取消 = `cron_delete` |
| 执行记录 | 每次触发在 session 里留下正常消息记录，用户回看即审计 |

**对抗性审查（会崩的场景）**：

| 风险 | 缓解 |
|---|---|
| 定时触发时工作台数据被用户并发操作 | db_exec 已参数化 + SQLite 单写锁，天然串行；汇总类只读更无冲突 |
| cron prompt 里携带大量数据 → ctx 膨胀 | 触发 prompt 只带**意图**不带数据（"[定时任务] 每日汇总"），数据由 agent 现查——零上下文膨胀原则 |
| 静默失败（定时任务跑了但没人知道） | 触发即写 session 消息；连续失败 N 次在右栏置顶提示 |
| 用户删除工作台但 cron 残留 | 删工作台时级联清理其 session 绑定的 cron 任务（落实施清单） |

### D.5 与 B 方案的关系

D 不是独立管道，是 B 的扩展：同一个 `workbench.context` direction 多拼一段运营上下文（D.2），同一批 toolset 多开放网关读写工具（D.3），同一个 session 接收 cron 投递（D.4）。**先落 B，D 在其上加厚**——没有 B 的注入框架，D 无处安放。

---

## 七、组合方案（推荐）

**方案 A + B 为主干，C 同步，D 在 B 验收后加厚**：

- 方案 A 让用户**感受到**这是工作台对话（产品级差异）
- 方案 B 让 LLM **真的**能开发工作台（功能级差异）
- 方案 C 让空态**融入主题**（视觉收尾，随 A 同步，纯 CSS）
- 方案 D 让 LLM **替用户干活**（运营级差异——录数据、跑定时任务，右栏成为工作台的"全职员工"）

只做 A 不做 B：UI 漂亮了，LLM 还是不能干正事——**形似神不似**。
只做 B 不做 A：LLM 能干活了，但用户看着右栏以为还是主对话——**主仆不分**。
只做 B 不做 D：LLM 只会改代码，用户说"把今天的账记一下"它还得现学——**能建不会用**。
D 依赖 B 的注入框架，**必须后于 B 落地**（见 §六 D.5）。

---

## 八、实施步骤（建议顺序）

### 阶段 1：方案 B 落地（优先级最高）

LLM 没上下文 = **当前工作台根本不能用**。先做这个。（实施位置已修订为 hermix-agent 层，见 B.5）

1. **skill 常驻（最先做，零新代码路径）**：workbench 会话的 `AgentOverrides.skills` 恒置 `["workbench"]`（MODULE_DESIGN §13 层 3）。这一步单独落地就已经解决"LLM 不遵循 skill 流程"的大半问题。
2. `crates/hermix-agent/src/directions.rs` 新增 `workbench.context` SystemDir
   - route 条件 `workbench_id.is_some()`，route_ctx 从 `SendMessageRequest.workbench_id`（字段已存在）透传
   - 内容拼装：`hermix_plugin::workbench` 的 `read_manifest` + `format_module_index` + `vendor_index`，每轮重拼（无缓存，见 B.3）
   - 拼装失败**降级**：跳过该 direction，不阻断请求
3. 工具集隔离确认（B.6 R6）：验证 `workbench_*` 工具只在 workbench 会话注册进 toolset；不是则补上，配测试钉死
4. 测试（hermix-agent）
   - workbench 会话 system prompt 含 `## WORKBENCH CONTEXT` + module_index 实内容
   - 普通会话 prompt **不含**该段（防泄漏，钉死 R5）
   - agent 写入新文件 → 下一轮 prompt 的 module_index 含新文件（T3，无需任何失效逻辑）
5. 主对话零回归：跑 `cargo test --workspace`

### 阶段 2：方案 A 落地（视觉差异化）

1. 抽 `<ChatCore>`（消息流 + 输入 + 工具渲染）
   - 从 `<ChatView>` 拆出 props 列表（最小化依赖）
   - 保留所有 message / tool / streaming 逻辑
2. 改 `<ChatView>` 为 `<ChatView chatNode={<ChatCore ... />} + 主对话 chrome>`
3. 新建 `<WorkbenchChat chatNode={<ChatCore variant="workbench" ... />} + 工作台 chrome>`
4. `<WorkbenchShell>` 改成渲染 `<WorkbenchChat>` 而非 `chatNode`
5. UI 测试（人工或 playwright）：右栏跟主对话**视觉一眼能区分**

### 阶段 3：错误处理清理（顺带做）

1. 拿掉 `<WorkbenchEmpty>` 底部的 `<p className="wb-error">`
2. 改用全局 `toastError`（已有 toast 系统）
3. `WorkbenchView.open()` 失败时**自动降级**：再走一次 createSession + bind，还失败才 toast

### 阶段 4：方案 D 落地（运营对话，依赖阶段 1）

前提：阶段 1 的 `workbench.context` direction 框架已在运行。D 是在 B 上加厚，见 §六 D.5。

1. **运营上下文段**（D.2）：`workbench.context` 拼装时追加"业务数据现状"——查 SQLite master + 各表 COUNT + 末 3-5 行样本；大表超阈值只给 schema + count。与 module_index 同批每轮重拼，无缓存
2. **网关工具开放**（D.3）：确认 workbench 会话 toolset 含 `db_query` / `db_exec` / `fs_read` / `fs_write` 网关工具；skill 增补运营边界（运营不写代码，改表结构转开发意图）
3. **定时任务**（D.4）：
   - cron 触发 = 向绑定 session 投递 `[定时任务] <意图>` 系统消息，走正常 agent loop（prompt 只带意图不带数据，防 ctx 膨胀）
   - agent 可用 `cron_create` / `cron_delete` 管理任务；删除工作台时级联清理绑定 cron
   - 连续失败 N 次右栏置顶提示
4. **测试**：
   - 运营上下文段：建含数据表的工作台 → prompt 含表 schema + count；空库降级不报错
   - 边界：运营对话调 `workbench_write` 被拒/被引导转开发
   - cron：注册 → 触发 → session 收到消息 → 删除级联生效
5. 回归：`cargo test --workspace` + `cargo test -p hermix-parity-tests`

---

## 九、待主人确认（2026-09-21 主人授权"看着办"，以下全部定案）

1. **方案 A + B + C 同时做还是分批？** **定案：分阶段同一批推进**——阶段 1（B）先，阶段 2（A）次之，阶段 3（C）与 A 同步（纯 CSS + token 改动，工期最短）；阶段 4（D）在阶段 1 验收后启动。
2. ~~**LLM 上下文 token 成本**：要不要给用户开关？~~ **已撤销（修订 2026-09-21）**：砍掉缓存与路由后每轮动态注入只剩几百 tokens，skill 常驻是工作台能开发的前提。关闭注入 = 工作台不能开发，不是设置项，不提供开关（见 B.6 R3）。
3. ~~**workbench skill 全文 vs 摘要**~~ **已撤销（修订 2026-09-21）**：SKILL.md 实测 190 行（2-3K tokens），且 skill 常驻通道已由 MODULE_DESIGN §13 层 3 设计定案——工作台会话 `AgentOverrides.skills` 恒为 `["workbench"]`。无需摘要、无需路由（见 B.4）。
4. ~~**「应用到工作台」按钮**~~ **已定：不做**。`workbench://changed` 自动热更新已提供"生效"反馈，按钮是冗余 UI；A.2 chrome 字段已同步删除该占位，保留 `lastChange` 状态条。
5. **空态重设计**（A 节 + C 节提议）：**定案：采纳合并版**——一个 input + 一个 button，Enter 即建，零教学；删"复制 PROMPT"块与"两步"分块；错误走 toast + inline。
6. **方案 C 的 token 改动路线**：**定案：保守先做**——不动 token，只用现有 token + `--color-border-primary` 发丝边框 + `--color-bg-hover` 做出框感，零回归。激进方案（改 `--color-bg-card / elevated` 明度阶梯）不做预告，等保守版上线后主人看实际效果再决定是否立项。
7. **方案 C 的 token 改动主题影响**：随第 6 条定案，当前**无 token 改动、无三主题回归工作量**。若将来走激进路线，届时 rainforest / sunset / dark 三主题截图回归是立项代价的一部分，默认接受。
8. **空态视觉锚点的具体措辞**：**定案：先用现文案**"给它起个名字，做什么用稍后跟它聊"——文案是最便宜的迭代项，上线后按主人实际语感再调；三词示例先保留"项目管理 · 联系人 · 记账"。

---

## 十、关联文档

- `WORKBENCH_MODULE_DESIGN.md` — 工作台代码模块、UI 工程纪律
- `ARCHITECTURE_REFACTOR_PLAN.md` — 整体架构演进
- `.workbuddy/memory/MEMORY.md` 「会话归属 / 工作台对话隔离」节 — 数据层归属设计
- `.workbuddy/memory/2026-09-21.md` — 2026-09-21 当日调试记录（含孤儿 session 根因）