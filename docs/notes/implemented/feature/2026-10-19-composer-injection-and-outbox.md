# Decision Note: Composer 输入注入与待发队列（outbox）

Status: implemented — 2026-10-19 当日落地：注入总线、会话级 outbox、发送失败恢复

## 实现落点（2026-10-19）

| 模块 | 落点 |
|---|---|
| 注入总线 | `src/lib/composerInjection.ts`：`replace / append / at-caret` 三种模式；无订阅者时排队，ChatView 挂载时回放 |
| 草稿 | ChatView 内 `drafts: Record<sessionKey,string>`，切会话隔离，400ms 防抖写 `hermix.composer.drafts.v1` |
| 预填 / 语音 / 增强 | 全部改走注入总线（不再 `setInput(prev + text)` 拼接） |
| 引用 / 编辑重发 | 助手消息「引用」注入 `> 段落`；用户消息「编辑」把原文注入并聚焦 |
| Outbox 数据 | `QueuedMessage` 增 `id / sessionId / overrides / status / attempts / error / createdAt`；入队时快照模型、权限、领域、专家、工具、技能、工作区 |
| 排空 | 只取当前会话队首；用快照 overrides；`drainingRef` 单飞；失败置 `failed` 停在队首等用户处理（不再按文本前缀重试 3 次后静默丢弃） |
| 立即发送 | 流式中移到队首 + toast「当前回复结束后发送」，不再取出再放回、静默无响应 |
| 去重 | 删除"文本相同不入队"；改为提交侧 500ms 同文本窗口（连发两次「继续」合法） |
| 失败恢复 | `onSendMessage` 返回 boolean；失败把 text + 图片 + 路径 + 文档放回输入区（用户已输入则追加） |
| 队列 UI | 默认折叠为「待发送 N 条」一行；展开后每行有状态点（等待/发送中/失败） |
| 持久化 | `hermix.outbox.v1`；带 `File` 句柄的条目（图片/文档）不落盘，只持久化文本与磁盘路径附件 |

**未做**：从某条消息分支（需后端 parent_id）；清空队列的二次确认；跨设备同步。

## Decision

把 Composer 从「一个受控 textarea + 一个全局数组」升级成三段有明确契约的交互层：

1. **输入注入**：单一 `InjectionRequest` 通道，队列化、可定位插入，支持引用 / 编辑重发 / 预填 / 语音追加。
2. **待发队列**：从 App 全局 `QueuedMessage[]` 改为**按会话的 outbox**，入队时快照发送参数，出队单飞（single-flight），失败可重试且不丢内容。
3. **发送后流程**：输入框在首次服务端确认前不清空（或失败必恢复），用户气泡有 `sending / sent / failed` 三态，失败可重试 / 编辑。

## 现状审计

### 1. 输入注入链路

| 事实 | 位置 | 影响 |
|---|---|---|
| 输入状态是 `useState("")`，与 session 无关 | `ChatView.tsx:417` | 切会话时草稿串台：A 会话写一半切到 B，B 的输入框里是 A 的内容 |
| 只有一条一次性预填通道 | `ChatView.tsx:454-467`（`pendingPrefill`） | 连续两次预填（工作流 + 应用创建）互相覆盖；只能替换全文，不能插到光标处 |
| 附件注入同样是单槽 | `App.tsx:174` `pendingFileAttachment` | 同上；第二次点击会覆盖第一次 |
| 语音 / 润色直接字符串拼接 | `Composer.tsx:523,526`（`setInput(p.input + text)`） | 光标位置丢失、不能撤销、并发注入会丢字 |
| 没有"引用选中内容 / 编辑上一条"的注入 API | 全仓无调用点 | 这两项交互无处落地 |

结论：现有实现是「组件内 state + 两个字符串 props」，不是链路。多来源注入必然互相覆盖。

### 2. 消息队列（现状）

| 事实 | 位置 | 影响 |
|---|---|---|
| 队列存在 App 顶层，全局一份 | `App.tsx:172` | 会话隔离缺失 |
| `QueuedMessage` 只有 text / images / files | `types.ts:368-374` | 没有 `sessionId / id / createdAt / overrides / status` |
| 自动排空取 `activeSession.model` | `App.tsx:3711-3716` | **A 会话入队的消息，切到 B 后会用 B 的模型发到 B 会话** |
| 排空只带 model，不带 permissionLevel / domain / expert / tools / skills / workspace | 对比 `ChatView.tsx:945-982` 的直发 overrides | 队列消息与直发消息**行为不一致**（权限、专家、领域静默丢失） |
| 「立即发送」在 streaming 时先弹出再原样插回、直接 return | `App.tsx:3574-3579` | 点击**没有任何反馈**，看起来是坏键 |
| 「立即发送」不传 overrides | `App.tsx:3581-3588` | 用后端默认模型发送，与用户当前选择不一致 |
| 入队按 text 完全相同去重 | `App.tsx:3776-3780` | 连发两次「继续」第二次被静默吞掉 |
| 排空失败重试 3 次，key = 前 60 字符 | `App.tsx:3717-3727` | 不同消息同前缀会共用计数；3 次后仅 `console.warn`，用户无感知 |
| 队列不持久化 | — | 刷新 / 崩溃即丢 |
| 清空无确认 | `Composer.tsx:268-273` | 误点即丢全部 |

### 3. 队列的视觉呈现

- 结构是「队列标题行（队列中 N 条 + 清空）+ 最多 8rem 高的列表」，压在 Composer 上方（`Composer.tsx:262-292`），与输入区形成两块等高权重面板，窄窗口下直接吃掉 120px 以上。
- 每一行：序号 + 正文前 80 字 + 悬浮才出现的编辑/立即发送/删除（`ChatIndicators.tsx:216-247`）。触屏与键盘用户看不到操作，`title` 是唯一说明。
- 没有状态表达：等待中 / 正在发送 / 失败三态长得一样；没有入队时间，也没有"第几条"的视觉重量。
- 出队是瞬时的，列表高度跳变；入队也没有任何确认（尤其 Enter 快速连按时）。
- 「停止」按钮的 tooltip 写「插话：停止当前回复并发送队列中的 N 条消息」(`Composer.tsx:596-598`)，但点击只停止；实际只会排空队首一条，文案与行为不符。

### 4. 发送后流程

| 事实 | 位置 | 影响 |
|---|---|---|
| 点击发送即清空输入与附件 | `ChatView.tsx:930-933` | 发送失败后原文与附件全部丢失，只能靠「重试上一条」 |
| 乐观插入 user + assistant 占位 | `App.tsx:2084-2099`（`flushSync`） | 这部分是对的，保留 |
| 失败只把错误写进 assistant 占位 | `App.tsx:3516-3534` | user 气泡仍显示成"已发送"，与服务端实际状态可能不一致 |
| 失败不回滚、不恢复草稿 | — | 用户要重新输入，附件要重新选 |
| 队列路径无确认 | `ChatView.tsx:934-941` | 用户不知道自己刚发的是"进队列"还是"已发送" |

## 问题清单（按严重度）

**P0（正确性）**
1. 队列跨会话泄漏：A 的消息发到 B，且用 B 的模型。
2. 队列消息丢失上下文参数（权限 / 领域 / 专家 / 工具 / 技能 / 工作区）。
3. 「立即发送」在 streaming 时静默无效；空闲时又不带模型 overrides。
4. 发送失败后草稿（含附件）不可恢复。

**P1（数据与交互契约）**
5. 输入状态不按会话隔离。
6. 注入通道是单槽字符串，多来源互相覆盖。
7. 失败重试 key 用文本前缀；3 次后静默丢弃。
8. 相同文本被去重，合法的重复指令发不出去。

**P2（视觉与可达性）**
9. 队列面板与输入区争夺垂直空间，缺少折叠形态。
10. 状态不可见（等待 / 发送中 / 失败），操作仅 hover 可见。
11. 入队 / 出队 / 清空均无反馈与过渡。
12. 「停止」文案承诺了它不做的事。

## 改造范围

### A. 输入注入链路

新增类型（`types.ts`）：

```ts
type InjectionMode = "replace" | "append" | "at-caret";
interface InjectionRequest {
  id: string;
  source: "prefill" | "quote" | "edit" | "voice" | "polish" | "file";
  text: string;
  mode: InjectionMode;
  /** quote / edit 的定位信息，用于失败恢复与撤销 */
  anchor?: { messageId?: string; selectionStart?: number; selectionEnd?: number };
  focus?: boolean;
}
```

- ChatView 持有**注入队列**（不是单个 prop），按序消费：`replace` 覆盖、`append` 追加、`at-caret` 在当前 selection 处插入并把光标放到插入末尾。
- 现有 `pendingPrefill` / `pendingFileAttachment` / `VoiceButton` / `EnhanceButton` 全部改走该通道。
- 草稿按会话隔离：`drafts: Record<sessionKey, { text; images; files; updatedAt }>`，切会话保存 / 恢复，localStorage 防抖持久化（键 `hermix.draft.<sessionId>`，欢迎态用 `welcome`）。
- 为消息层提供 `onInsertQuote(message)` 与 `onEditMessage(message)`：前者生成 `> 引用` 片段，后者把原文注入并把光标放末尾。

### B. Outbox（会话化队列）

`QueuedMessage` 扩展为：

```ts
interface OutboxItem {
  id: string;
  sessionId: string;
  text: string;
  attachments?: { images?; attachedFilePaths?; attachedFiles? };
  /** 入队时快照，排空时原样使用 */
  overrides: ChatOverrides;
  status: "queued" | "sending" | "failed";
  attempts: number;
  error?: string;
  createdAt: string;
}
```

- 存储：单个数组 + 每条带 `sessionId`（App 内 `useState`，渲染与排空都按会话过滤）；持久化到 localStorage。带 `File` 句柄的条目无法序列化，不落盘。
- 排空：每个会话 single-flight；流结束后取该会话队首，**使用快照 overrides 发送**，不读 `activeSession.model`。失败置 `status: "failed"` 并保留（不再按文本前缀计数），提供重试 / 编辑 / 删除。
- 「立即发送」：
  - 空闲 → 立即按快照发送；
  - streaming → 移到队首并 toast「当前回复结束后发送」；
  - 可选「停止并插话」需二次确认，明确它会打断当前回复。
- 去重：删除文本相等判断，改为 500ms 内同文本视为重复提交（防连按 Enter）。
- 清空：>1 条时二次确认或提供撤销 toast。

### C. 发送后流程

- 用户气泡加投递态：`sending`（发送中）→ `sent`（收到服务端首个事件）→ `failed`（错误 / 超时）。
- 发送失败：气泡标「未送达」并给「重试 / 编辑」；**同时把 text + 附件恢复到 Composer**（若用户已在输入则追加到草稿而不是覆盖）。
- 入队成功给 toast（「已加入队列 · 第 2 条」），不再静默。
- 「停止」tooltip 改为「停止生成」；若要"停止并发送队列"另设按钮与确认。

### 视觉规范（P2 一并落地）

- 队列默认折叠为**一行摘要**：`待发送 2 条 · 展开`（高 28px），≥4 条或展开后显示列表，列表最高 6rem 且内部滚动。
- 每行：状态点（等待=中性 / 发送中=accent 脉冲 / 失败=error）+ 文本 + 右侧「⋯」菜单（立即发送 / 编辑 / 删除）；操作不再依赖 hover。
- 入队 / 出队用 120ms 高度与透明度过渡（`grid-template-rows` 或 max-height 的替代实现，禁用 width/height 动画）。
- 与 Composer 之间只保留一条分隔线，不再用双层卡片。

### 明确不做

- 消息树 / 从某条消息分支：需要后端 parent_id 协议，归独立立项。
- 队列跨设备同步：无后端字段，不做。

## 评估标准

1. **会话隔离**：A 会话入队 2 条 → 切 B：B 输入为空、队列为空；切回 A：原文与 2 条队列原样恢复。
2. **注入定位**：连续两次注入（引用 + 语音）后文本顺序正确、光标在最后一次插入末尾；`replace` 不污染其他会话草稿。
3. **立即发送**：空闲时发送使用的模型 / 权限 / 领域与入队快照完全一致（可用调试日志断言）；streaming 时点击有 toast，队列顺序不变，不丢条目。
4. **失败恢复**：断网发送 → 气泡显示「未送达」、Composer 恢复原文与附件；重试成功后不出现两条相同用户消息。
5. **重复指令**：连续发送两次「继续」→ 队列中两条都在。
6. **视觉**：队列 ≤3 条时默认折叠高度 ≤ 32px；每行状态可见；操作可用键盘 Tab 到达；展开/收起无布局跳动。
7. **持久化**：重启应用后待发队列与草稿仍在（按会话）。
8. **回归门禁**：`tsc --noEmit` 0 错误、`check:design` clean；手测矩阵（发送 / 排队 / 停止 / 切会话 / 断网 / 重启）全过。

## Alternatives considered

- **只修模型与去重，不重构队列**：改 4 行即可，但跨会话串台与失败丢稿仍在，属治标。
- **把队列下沉到后端**：需要新协议与持久化表，收益是跨设备；当前需求不涉及，先把前端 outbox 做对。
- **输入状态提升到 App**：能让注入更直接，但 ChatView 是会话级组件，提升后每次按键都会重渲染 App（含侧栏/右栏），代价大于收益。改为会话级 draft map 更稳。
- **发送后立即清空 + 失败靠「重试上一条」**（现状）：实现简单，但失败即丢稿，是本次要消除的主要痛点。

## Consequences

- 队列与草稿都变成会话级数据，存储键增加；需要一次性的清理策略（会话删除时清 outbox / draft）。
- 注入队列让 ChatView 多一个消费 effect，必须防止同一请求被消费两次（用 id 去重）。
- 「发送中不清空」会改变现有手感：输入框在首帧前仍显示原文，需要 loading 态避免用户误以为没发出去。
- 视觉收敛后队列面板变成一行摘要，展开才占空间；需要同步更新 Composer 的键盘快捷键说明。
- 带 File 句柄的队列条目（图片 / 文档）不跨重启保留：浏览器无法序列化 File；文本与磁盘路径附件会保留。
