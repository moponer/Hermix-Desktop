# 对话中的任务列表 / Todo 前后端专项排查

> **排查时间**：2026-07-13 上午（主人追问）
> **范围**：对话流里 LLM 通过 `todo` 工具创建 / 更新 / 完成任务的整条链路
> **覆盖文件**：`apps/desktop-tauri/src/{components,hooks,api}.{ts,tsx}` + `crates/hermix-server/src/lib.rs` + `crates/hermix-tools/src/{tools,backends}/todo.rs` + `crates/hermix-transport/src/lib.rs`
> **方法**：人工逐行 Read + 全栈 Grep 验证每个契约

---

## 链路全景图（不工作之前 / 之后）

```
                    LLM (per turn)
                        │
                        ▼
       ┌── agent_loop.run_turn() ──┐
       │  (call todo 工具)         │
       └────────────┬──────────────┘
                    ▼
       ┌── TodoHandler.execute() ──────────────┐
       │ hermix-tools/src/tools/todo.rs        │
       │  ├─ backend.create/update/list...     │  ← FileTodoBackend (todos.json)
       │  └─ publish(TodoChange { session_id })│  ← broadcast channel
       └────────────┬──────────────────────────┘
                    ▼
       ┌── hermix-server todo_event listener ──┐
       │ lib.rs:3699-3752                      │
       │  ├─ filter by session                 │  ✓ 已做
       │  ├─ todo_change_to_stream()           │  ✓ 已做
       │  └─ ❌ originating_message_id = None  │  ❌ 硬编码
       └────────────┬──────────────────────────┘
                    ▼
       ┌── WS send: TodoGroupStarted / TodoItemUpserted / TodoItemRemoved │
       │ ❌ 缺 TodoSnapshot                                                    │
       └────────────┬──────────────────────────────────────────────────────────┘
                    ▼
       ┌── useStreamChat.ts:786-830 ─────────────────────────────────────┐
       │  ✓ 处理 todo_group_started / todo_item_upserted / todo_item_removed │
       │  ❌ 不处理 todo_snapshot                                                │
       │  ❌ onTodoChange 不按 sessionId 二次过滤                              │
       └────────────┬──────────────────────────────────────────────────────────┘
                    ▼
       ┌── App.tsx:1740-1808 onTodoChange ──┐
       │   ├─ group_started: 走 TodoGroup accumulator  │
       │   ├─ upsert: 走 TodoGroup.upsert    │  ✓ 已做
       │   └─ remove: 走 TodoGroup.remove     │
       │   ❌ session 切换不清 todoGroups            │
       │   ❌ onTodoChange 不过滤 sessionId           │
       └────────────┬────────────────────────┘
                    ▼
       ┌── ChatView.tsx:1923 `<div className="todo-strip">` ──┐
       │     {todoGroups.map((g) => <TodoListCard …/>)}         │  ← 全部堆在顶部
       │     ❌ 没用 anchorMessageId，不锚到 assistant message  │
       └────────────────────────────────────────────────────┘
```

---

## P0 — 三处死链（必修）

### P0-T1 TodoGroupStarted 永远拿不到 message_id

**症状**：用户让 LLM 拆解一个计划 → LLM 调 `todo.create` 多次 → 主人看到的卡片全堆在消息流顶部，不挨着产生这个计划的 assistant message。

**根因 1（后端硬编码）**：
```rust
// crates/hermix-server/src/lib.rs:3706-3710
// v1: originating_message_id left as None so new group cards
// append at the end of the message stream. A future patch
// will thread the active assistant message_id through from
// agent_loop.
let originating_message_id: Option<String> = None;  // ❌
```

todo_change_to_stream 收到 `originating_message_id = None` 透传，前端拿到的 event 永远 `message_id: null`。

**根因 2（架构层缺桥）**：
`crates/hermix-agent/src/agent_loop/mod.rs` 整个文件 grep `originating_message_id` / `active_message_id` / `current_message_id` 零匹配。todo.rs 通过 `SessionStamper::set_active_session` 设的是 **session_id**，不是 **message_id**。agent_loop 在 LLM 调工具时根本没有"当前 assistant message_id"这个引用。

**根因 3（前端定义但不用）**：
```ts
// apps/desktop-tauri/src/components/ChatView.tsx:873-878
todoGroups?: Array<{
  groupId: string;
  groupTitle: string | null;
  anchorMessageId: string | null;  // ✅ 定义
  itemsById: Record<string, TodoSnapshotItem>;
}>;

// 行 1917-1921 注释明确说 "future patch can map anchorMessageId -> message index"
// 但代码永远渲染在 `.todo-strip` 顶部（行 1923）
```

**修复方案**（三段同步改，**单 PR 失败则失败**）：
1. **后端 todo.rs**：把 `TodoChangeKind::GroupStarted` 增加 `message_id: Option<String>` 字段；TodoHandler 维护 `assistant_message_id_of_origin` 通过 SessionStamper 接口
2. **后端 agent_loop**：在每个 LLM `text` / `tool_call` 块开始前，把当前 assistant message 的 id 写到 todo handler 的 SessionStamper（同步 set_active_message_id 调用）
3. **后端 hermix-server/lib.rs:3710**：把硬编码 None 替换为从 todo.rs SessionStamper 读到的 `assistant_message_id`，再透传到 todo_change_to_stream
4. **前端 ChatView.tsx**：渲染时按 anchorMessageId 找对应 assistant message index，把 TodoListCard 插入到该消息之后（用 stable fragment + array splice）

**验收**：
- LLM 拆解任务后产生的 plan 卡片出现在产生它的 assistant message 紧邻下方
- 滚动到对应消息时锚点稳态，无锚点丢失
- reload session 后 anchorMessageId 仍能正确解析（从数据库读 messages）

---

### P0-T2 TodoSnapshot 事件前端完全不处理

**症状**：
- 主人 reload session（或打开已有 todo 的 session）→ 看不到任何已存在的 todo 卡片，直到 LLM 再调一次 `todo` 工具才会出现
- 多客户端打开同一 session，一个客户端点 checkbox → 另一个客户端看不到（todo 工具走 HTTP broadcast，其它 client 不在 WS 路径上）
- WS 重连后（例如网络断 5 秒）→ 已存在的 todo 全部消失，要等下一次 tool call 才能恢复

**根因**：
```rust
// crates/hermix-transport/src/lib.rs:547-556
/// Snapshot of the full task list. Sent on (a) session start so
/// the front-end can hydrate without a separate HTTP fetch, and
/// (b) after every LLM-driven todo tool call so any late joiner
/// catches up. Front-end uses this as the authoritative full
/// state — applies upserts/removes lazily via TodoItem*.
TodoSnapshot {
    session_id: String,
    items: Vec<TodoSnapshotItem>,
}
```
后端定义得很清楚，但前端：
```ts
// apps/desktop-tauri/src/hooks/useStreamChat.ts:786-830 范围内只有三个 if：
if (evtType === "todo_group_started") { ... }
if (evtType === "todo_item_upserted") { ... }
if (evtType === "todo_item_removed") { ... }
// ❌ 完全缺 if (evtType === "todo_snapshot") { ... }
```

**修复方案**：
1. useStreamChat.ts:786 之后加 `if (evtType === "todo_snapshot")` 分支
2. 把 backend 的 `items: Vec<TodoSnapshotItem>` 映射成 App.tsx 的多个 group_started + upsert 事件，统一灌入 setTodoGroups
3. WS connect 后**主动发请求**触使后端补刀（否则后端只在 tool call 后才发 snapshot）
4. App.tsx 在 active session 切换时**清空 todoGroups**（见 P1-T4），然后等 TodoSnapshot 推过来 hydrate

**验收**：
- 重启应用 → 切到已有 todo 的 session → 立刻看到完整 todo 卡片
- WS 断开 5 秒后 reconnect → todo 卡片重新出现
- 两台 desktop 同时打开 sessionA → 一台修改 → 另一台通过 TodoSnapshot 同步

---

### P0-T3 listTodos HTTP 端点 0 前端调用方

**症状**：主人按 Ctrl+R 重载应用 → 切回历史 session → todo 卡片全空（因为不只缺 TodoSnapshot，**且没有 React 端调用 listTodos**）。

**根因**：
```ts
// apps/desktop-tauri/src/api.ts:301-336
// `listTodos` powers the TodoPanel's initial hydration. `patchTodo`
// is what the user's manual checkbox click calls...
export interface TodoItem { ... }
export async function listTodos(): Promise<{ todos: TodoItem[]; total: number }> {
  return fetchJSON(`/v1/todos`);
}
export async function patchTodo(...) { ... }
```

注释说"powers TodoPanel hydration"，但 TodoPanel 早被废弃（TodoListCard.tsx:11 "Replaces the v1 global TodoPanel"）。

Grep 调用方：
- `listTodos` → 0 调用方
- `patchTodo` → 2 调用方（TodoListCard.tsx:96 + App.tsx:2124）

后端 lib.rs:1902 有 `route("/v1/todos", get(list_todos))`，但**前端没拉过**。

**修复方案**：
1. 不要做 listTodos HTTP 兜底（违反铁律 H：避免 dual source；还要解决 todos 是 **per-session** 不是 per-user，listTodos 返回的是全局，反人类）
2. **改用 TodoSnapshot WS 同步**（修 P0-T2）就够用
3. 如果坚持保留 HTTP 兜底，至少把 listTodos 标记 `@deprecated since YYYY-MM-DD — use WS TodoSnapshot` 并写进 useStreamChat.ts:786 块的注释

**验收**：
- 主人 reload session → TodoListCard 仍能恢复（不需要 listTodos）
- 删 api.ts:323 的 listTodos（孤儿代码）或者正式标注 deprecated

---

## P1 — 数据 / 状态错误

### P1-T4 切 session 后 todoGroups 不清，stale 数据泄漏

**症状**：用户切 session：A 有 3 个 todo，B 有 1 个 todo → 切 B → 看到 4 个 todo（A 的 3 个 + B 的 1 个 混堆在 B 顶部）。

**根因**：
```ts
// apps/desktop-tauri/src/App.tsx:1740-1808
onTodoChange: (change) => {
  setTodoGroups((prev) => { ... })  // ❌ 不判断 sessionId
}
```
只在 WS event 写 todoGroups，session 切换后无 cleanup hook。

**修复方案**：
1. App.tsx 加 `useEffect(() => { setTodoGroups([]); todoGroupsByIdRef.current.clear(); }, [activeSessionId])` —— 切 session 时立即清
2. useStreamChat.ts onTodoChange 调用前加 `if (change.sessionId !== activeSessionId) return;`，双保险（不应信任后端）
3. WS reconnect 时也调一次 setTodoGroups([]) + 等新 TodoSnapshot

**验收**：
- 在 sessionA 有 todo，切到 sessionB → 看不到 sessionA 的 todo
- 切回 sessionA → TodoSnapshot 重新 hydrate 或 WS rehydrate 触发

---

### P1-T5 PATCH /v1/todos/:id 不传 group_id，可能丢群组归属

**症状**：用户点 checkbox → 调 `patchTodo(item.id, { status })` → 后端 `update` 更新 status → `publish(TodoChangeKind::Upsert(item))` 重发整个 item → 前端 todoGroups[?].itemsById[id] = item。

但 `group_id` 不变，理论上不会丢群组——除非：
- 用户当前 active session 切换时 todoGroups 被清
- 后端 item.group_id 为空（legacy 数据，前端向后兼容）

看 api.ts:329 `patchTodo` 接口允许 `status / title / priority`，**不允许 group_id**。

后端 `dispatch("todo", { action: "update", id, status, title, priority })` 也不带 group_id。OK 这条比较安全，但仍有遗留风险：

**验证**：
- 如果 LLM 把 group 1 的 task 移到 group 2（理论上不应该发生，但 schema 没禁止）
- 当前 API 不支持 × —— 是 design 不是 bug。放过。

### P1-T5 修正：实际可观察风险是 TodoHandler `snapshot_after_mutation` 的二次 IO

每次 LLM `todo` 工具调用都要重新 `list()` 整个 todos.json，可能在大量 todo 列表时拖累。

**根因**：
```rust
// crates/hermix-tools/src/tools/todo.rs:204-218
async fn snapshot_after_mutation(&self, target_id: Option<&str>) -> Option<...> {
    let list_json = self.backend.list(None).await.ok()?;  // ❌ 每次 mutate 都重新 list 全集
    ...
}
```
每次 create/update/depend/record_execution 都要 O(N) 序列化全集。

**影响**：100 个 todo 列表，单次 list 序列化 + parse 大约毫秒级，对实时 streaming 影响微弱。但端点 `/v1/todos` 同样依赖 `dispatch("todo", {action: "list"})` 走 list_backend.list 全集。看起来 O(N) 是必然接受的设计约束。

**候选修复**（非阻塞）：
- FileTodoBackend 内部维护 IndexedMap `id -> item`，snapshot_after_mutation 直接按 id 取，省去 list 再 filter
- 写一个 `backend.get(id)` 方法，给 snapshot 用
- 重构 FileTodoBackend 加 `Arc<RwLock<HashMap<String, TodoItem>>>` 缓存层

**优先级**：低，因为实测性能暂不痛。如果 LLM 频繁 create/update 拖累才考虑。

---

## P2 — 设计债

### P2-T6 TodoHandler `announced_groups: HashSet` 进程重启后失效

**症状**：服务重启后，第一个 todo 工具调用会重新发 GroupStarted。前端 App.tsx:1750 已 dedup（`if (todoGroupsByIdRef.current.has(change.groupId))`），无 bug。但意味着 frontend state 跟 backend state 不一致，重启期间主客户端重连时会收到"重复"事件。

**修复**：保留 dedup 即可。注释明确说这是缓存行为，无须修。

### P2-T7 TodoChangeKind 用 serde_json::Value 而非 DTO

**根因**：`crates/hermix-tools/src/tools/todo.rs:18-21`：
> The wire format is `TodoSnapshotItem` (defined in `hermix_transport`) — but we keep it as `serde_json::Value` here to avoid hermix-tools depending on hermix-transport (that would form a cycle hermix-server → hermix-tools → hermix-transport).

`todo_change_to_stream` 在 hermix-server 层手工做 `Upsert(item) → TodoSnapshotItem { id, title, ..., group_id, group_title }` 字段拷贝（lib.rs:4918-4930）。字段如果未来加一两个，会漏改；加几个新字段就踩坑。

**修复**：
- 让 hermix-tools 直接定义 `TodoSnapshotItem`（在 todo.rs 模块顶部），hermix-transport 引用 hermix-tools 而不是反过来（破除层级倒置）
- 或者用 serde convert 字段自动映射
- 现阶段不动（不在主人当下的优先级）

---

## 与既有清单的连接

本专项发现与 `docs/FRONTEND_AUDIT_2026-07-13.md` 的关联：

| 已列项 | 本专项新发现 |
|---|---|
| P0（其他） | + P0-T1（TodoGroup 无 message_id）+ P0-T2（TodoSnapshot 漏监听）+ P0-T3（listTodos 孤儿） |
| P1-5 production console.log | `useStreamChat.ts:1190 [diag #N] onerror → retry` 仍透传 |
| P2-1 0 单元测试 | todo 链路 0 测试：`backend/list round-trip` / `snapshot_after_mutation` / `TodoChange broadcast` / `App.tsx setTodoGroups dedup` |

---

## 验收门槛（同 FRONTEND_AUDIT）

1. ✅ `cargo check --workspace --all-targets` 通过
2. ✅ `cargo test --workspace --lib` 全绿 + 新增 todo 链路 ≥ 4 项回归
3. ✅ `pnpm tsc --noEmit` 0 errors
4. ✅ 主人在实机 reload 一个有 5+ todo 的 session，能看到完整卡片并锚到原 assistant message
5. ✅ WS 断开 5 秒 reconnect 后 todo 重新出现，无 stale 数据

---

## 修复优先级建议

| 顺序 | 修复 | 工作量 | 风险 |
|---|---|---|---|
| 第一 | P0-T3：删 listTodos 或 deprecated 注释 | 0.5h | 极低 |
| 第二 | P0-T2：接 TodoSnapshot 监听 + session 切时清空 | 4h | 中（要写 dedup + group 推断） |
| 第三 | P1-T4：session 切清 todoGroups + double check sessionId filter | 1h | 低 |
| 第四 | P0-T1：thread message_id 跨 server / agent_loop / todo 三层 | 8h | 高（要协调三 crate 的接口） |
| 滚动 | P2-T6 / T7：refactor + 加单测 | 8h | 中 |

P0-T1 是真正的设计债，但实现成本最高，要三 crate 协同。前三项先做能让 70% 的用户痛点消失。

---

## 附录：未覆盖的潜在风险（建议未来复审）

- `crates/hermix-tools/src/backends/todo.rs:51` 错误处理吞错：`std::fs::read_to_string(&file_path).ok().and_then(...)`。todos.json 损坏时直接以空 Vec 启动，主人可能察觉不到数据丢。
- hermix-server/src/lib.rs:1904 `route("/v1/todos/{todo_id}", patch(patch_todo).delete(delete_todo))` 没有 auth / scope check。HTTP 端点能直接动 todos.json。
- todos.json 没有版本字段，未来 schema migration 时没标记点。