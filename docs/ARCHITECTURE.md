# Hermix Desktop 系统架构文档

> ⚠️ **DEPRECATED — 2026-06-19**  
> 本文档已被 v4 三件套替代:
> - **`docs/ARCHITECTURE_BENCHMARK.md`** (事实基线,50 大类 / 234 功能点,实测 grep+Read)
> - **`docs/ARCHITECTURE_REFACTOR_PLAN.md`** (目标架构 + 17 项 P0-P2 + 12 决策点)
> - **`docs/MIGRATION_TODO.md`** (8 阶段 5.5 周迁移计划)
>
> **事实失实**:
> - 写"21 个 crates" → 实际 22(漏 hermix-environments)
> - 写"~146K 行" → 实际 179,624 行(+33K)
> - 漏 `hermix-gateway` 整个 crate(29,531 行 / 25 文件)
> - 漏 `hermix-acp` / `hermix-voice` / `hermix-config/managed_gateway`
> - 漏 26 通道 / 21 平台 / 23 dashboard 子模块 / 8 memory plugin
>
> 保留此文档仅作为历史参考。**新决策请改三件套,不要改这里**。删除 PR 待主人批准。

---

**版本**: 1.0  
**日期**: 2026-06-18  
**作者**: Senior Developer (高级开发工程师)  
**依据**: 代码事实分析(非脑补)

---

## 一、系统概览

Hermix Desktop 是一个 **Windows-only** 的便携式 AI 助手,采用:

- **前端**: Tauri v2 + React 19 + Tailwind 4 + Vite
- **后端**: Rust 21 个 crates (~146K 行代码)
- **数据**: SQLite (嵌入式,无需外部 DB 服务)
- **构建**: MSVC 工具链(Git Bash 需先加载 `cargo-env.sh`)

---

## 二、架构分层(真实状态)

```
┌─────────────────────────────────────────────────────────────┐
│                    前端层 (Frontend)                        │
│  Tauri v2 外壳 + React 19 UI + Tailwind 4 样式           │
└─────────────────────────────────────────────────────────────┘
                          ↓ (IPC: Commands/Events)
┌─────────────────────────────────────────────────────────────┐
│                   后端核心层 (Rust Crates)                 │
│  hermix-agent (Agent Loop)                                │
│  hermix-intelligence (工具路由)                            │
│  hermix-focus (L1 记忆)                                   │
│  hermix-tools (30+ 工具)                                  │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│                  运行时 & 服务层                             │
│  hermix-runtime (TickLoop 心跳)                           │
│  hermix-server (HTTP/WS API)                              │
│  hermix-cli (CLI 入口)                                    │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│                    数据持久化层                              │
│  sessions.db (L2 会话, ⚠️ 无事务保护)                    │
│  memory.db (L1 长期事实, ✓ 有 FTS5 + 向量)               │
│  sessions/*.md (文件系统缓存, ⚠️ 时间戳文件名)             │
└─────────────────────────────────────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────────┐
│                      外部接口                                │
│  LLM Providers (OpenAI/Anthropic/Qwen/...)                │
│  MCP Servers (hermix-mcp)                                 │
│  Skill System (hermix-skills)                              │
│  Desktop Automation (enigo/xcap/arboard)                  │
└─────────────────────────────────────────────────────────────┘
```

---

## 三、Crate 依赖关系(核心链)

```
hermix-cli / hermix-server (二进制入口)
        ↓
hermix-agent (Agent Loop 引擎)
        ↓
hermix-intelligence (工具路由 + Thinking 模式)
hermix-focus (L1 记忆 + FocusStack)
hermix-tools (30+ 工具实现)
        ↓
hermix-core (Trait 定义 + 错误类型 + 消息类型)
hermix-config (配置管理 + 路径解析)
```

**完整 21 Crates 清单** (见 `Cargo.toml:9-31`):

| Crate | 职责 | 依赖 |
|-------|------|------|
| `hermix-agent` | Agent Loop 引擎 | core, config, focus, intelligence, tools |
| `hermix-intelligence` | 工具路由 + Thinking | core, config |
| `hermix-focus` | L1 记忆 + FocusStack | core |
| `hermix-tools` | 30+ 工具实现 | core, config |
| `hermix-config` | 配置管理 | core |
| `hermix-core` | Trait 定义 + 错误类型 | - |
| `hermix-server` | HTTP/WS API + Dashboard | agent, config |
| `hermix-cli` | CLI 二进制入口 | agent, config |
| `hermix-runtime` | TickLoop 自适应心跳 | agent, config, core |
| `hermix-gateway` | 网关(未深入分析) | - |
| `hermix-mcp` | MCP Server 连接 | core |
| `hermix-skills` | Skill 系统 | core |
| `hermix-cron` | 定时任务 | core, config |
| `hermix-environments` | 环境管理 | core, config |
| `hermix-acp` | ACP 协议 | core |
| `hermix-eval` | 评估框架 | core |
| `hermix-telemetry` | 遥测/日志 | core |
| `hermix-parity-tests` | Python↔Rust 对齐测试 | core |
| `hermix-bus` | 事件总线 | core |
| `hermix-voice` | 语音接口 | core |
| `hermix-transport` | 传输层 | core |

---

## 四、数据流架构(真实调用链)

### 4.1 用户消息 → LLM 响应

```
用户输入 (React UI)
    ↓
Tauri IPC Command
    ↓
hermix-agent AgentLoop.run()
    ↓
1. 加载上下文:
   - 读取 sessions.db (L2 会话历史)
   - 调用 MemoryRouter.recognize() (L1 长期事实)
   - 加载 skills (hermix-skills)
   - 加载 workspace context files
    ↓
2. 构造 messages 数组 (系统提示 + 历史 + 当前输入)
    ↓
3. 调用 LLM Provider (OpenAI/Anthropic/...)
    ↓
4. 解析响应:
   - 如果是普通文本 → 返回前端显示
   - 如果是工具调用 → 执行工具 (hermix-tools)
     ↓
     并发执行所有工具调用
     ↓
     结果追加到 messages
     ↓
     回到步骤 3 (继续调 LLM)
    ↓
5. 完成一轮,持久化:
   - persist_session_with_summary() → sessions.db
   - save_session_log() → sessions/*.md
   - save_trajectory() → trajectories/*.json
   - FocusStack.drain_conclusions() → memory.db
```

### 4.2 持久化写入(3 个独立调用,无事务协调)

**⚠️ 致命问题**: 这 3 个写入是独立的,任何一步失败都不会回滚其他两步。

```
AgentLoop 完成一轮
    ↓
调用 persist_session_with_summary()
    ↓
1. Connection::open() ← 每次都拿新连接!
2. UPSERT sessions 表
3. flush_messages_to_session_db()
   a. DELETE FROM messages WHERE session_id = ? ← 危险!
   b. 逐条 INSERT INTO messages
4. Connection 关闭
    ↓
调用 save_session_log()
    ↓
1. 生成文件名: format!("{timestamp}-{session_id}.md") ← 每次都新文件!
2. std::fs::write() ← 不在事务里

调用 save_trajectory()
    ↓
1. 生成文件名: format!("{timestamp}-{session_id}.json") ← 同上
2. std::fs::write()
```

---

## 五、记忆架构(L1/L2/L3)

### 5.1 L1 长期事实记忆 (hermix-focus)

**设计**: 基于 FocusStack 的 5 状态帧栈 + 向量搜索

**SQLite Schema** (`embedded.rs:127-141`):

```sql
CREATE TABLE frames (
    id              TEXT PRIMARY KEY,
    payload         TEXT NOT NULL,       -- JSON 编码的 T
    state           TEXT NOT NULL,       -- Created/Pushed/Kept/Returned/Cleared
    salience        REAL NOT NULL,       -- [0.0, 1.0]
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    parent_id       TEXT,                -- 树形结构(单向)
    tags            TEXT NOT NULL,       -- JSON 数组
    pin_count       INTEGER NOT NULL,
    touch_count     INTEGER NOT NULL,
    embedding       BLOB,                -- 向量(可选)
    embedding_dim   INTEGER,
    embedding_model TEXT
);

CREATE VIRTUAL TABLE frames_fts USING fts5(
    id UNINDEXED,
    payload_text,
    tags_text,
    tokenize = 'unicode61 remove_diacritics 2'
);
```

**读取路径**:

```
AgentLoop 开始新 turn
    ↓
MemoryRouter.recognize(query)
    ↓
并发调用所有 backend:
  - EmbeddedMemoryBackend.recognize()
    1. FTS5 全文搜索 (如果可用)
    2. 向量余弦相似度 (如果 query.embedding 有值)
    3. 按 salience 重排序
    ↓
去重 + 按 salience 重排序
    ↓
返回 top-K MemoryHit
    ↓
格式化为 <memory-context> 块,注入系统提示
```

**架构问题**:

| 问题 | 严重程度 | 后果 |
|------|----------|------|
| 反向链接缺失 (没有 source_session/source_turn 列) | 高 | 无法从"用户问的这句话"反查到"哪几条 frame 是因此生成的" |
| 向量搜索性能 (embedded.rs:36 注释说"dim-indexed scan is the primary cosine path until we upgrade to usearch HNSW") | 高 | 全表扫描! |
| FTS5 可选 (初始化失败就静默跳过) | 中 | 降级到 LIKE 搜索 |
| parent_id 单向 (只能从父查子) | 中 | 不能查"这个 frame 的上下文是什么" |

### 5.2 L2 会话级记忆 (session_persistence.rs)

**设计**: SQLite sessions.db + 文件系统缓存(markdown + JSON)

**SQLite Schema** (`session_persistence.rs:127-148`):

```sql
CREATE TABLE sessions (
    id                TEXT PRIMARY KEY,
    model             TEXT NOT NULL,
    platform          TEXT NOT NULL,
    created_at        TEXT NOT NULL,
    updated_at        TEXT NOT NULL,
    title             TEXT,
    message_count     INTEGER NOT NULL,
    system_prompt     TEXT,
    compressed_summary TEXT
);

CREATE TABLE messages (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id    TEXT NOT NULL,
    role          TEXT NOT NULL,  -- 'system'|'user'|'assistant'|'tool'|'approval'
    content       TEXT,
    tool_call_id  TEXT,
    tool_calls    TEXT,            -- JSON 数组
    created_at    TEXT NOT NULL,
    active        INTEGER DEFAULT 1,
    extra         TEXT,            -- JSON,塞 ApprovalRecord 等元数据
    FOREIGN KEY(session_id) REFERENCES sessions(id)
);
```

**架构问题**:

| 问题 | 严重程度 | 后果 |
|------|----------|------|
| 无事务保护 (每次 Connection::open,不包 BEGIN) | **致命** | 进程在 DELETE 和 INSERT 之间崩溃 → messages 表被清空 |
| 时间戳文件名 (save_session_log() 用 `{timestamp}-{session_id}.md`) | **高** | 每次保存生成新文件,旧文件成垃圾;重启后"列表"和"磁盘"对不上 |
| 三写不一致 (DB + md + json 是 3 个独立调用) | **高** | 任何一步失败,其他两步不回滚 |
| 没有 reflection role (messages 表 role 列只有 5 种) | **中** | 反思/总结/教训没地方存,只能塞 extra JSON,读不出来 |

### 5.3 L3 跨项目记忆 (未实现)

**当前状态**: **不存在**。

`hermix-focus` 的 `open_at_hermix_home()` 写死 `~/.hermix/data/memory.db`,每个项目/IDE 实例用自己的,不共享。

**按 BaiLongma 设计**(L1/L2/L3 三明治),L3 应该存在,但 Hermix 没实现。

---

## 六、"重启就没了"的根因分析

### 6.1 根因 1: 三写不一致 + 无事务

**场景**: Agent 在一次会话里反复保存(每 N 轮保存一次)

1. `persist_session_with_summary()` 执行:
   - `DELETE FROM messages WHERE session_id = 'abc'`
   - **(崩溃在这里发生)**
   - `INSERT INTO messages VALUES (...)` ← 没执行到

2. 重启后:
   - UI 读 `sessions` 表,看到 `message_count = 0` (因为 DELETE 成功了,但 INSERT 没执行)
   - 显示"空会话"
   - 磁盘上其实有 `<home>/sessions/2026-06-18_135632-abc.md` (上一次成功保存的)
   - **但 UI 不读这个文件**,只信 DB

### 6.2 根因 2: 时间戳文件名导致"列表-磁盘"不一致

**场景**: 同一会话保存了 3 次

磁盘上:
```
~/.hermix/sessions/
  2026-06-18_130000-abc.md  (10KB, 包含前 5 轮对话)
  2026-06-18_130120-abc.md  (20KB, 包含前 10 轮对话)
  2026-06-18_130253-abc.md  (30KB, 包含前 15 轮对话)
```

DB 里:
```sql
SELECT message_count FROM sessions WHERE id = 'abc';
  → 15
```

UI 列出会话:
- 查 `sessions` 表 → 看到 1 个会话, `message_count = 15`
- **不扫描** `sessions/` 目录
- 用户点进去 → 读 DB 的 `messages` 表 → 有 15 条消息 ✅ (如果没遇到根因 1)

**但如果遇到根因 1**:
- DB 里 `message_count = 0`
- 磁盘上有 3 个 md 文件,每个都包含部分对话
- **用户感觉"重启就没了"** —— 其实是 DB 和磁盘不一致

### 6.3 根因 3: `flush_messages_to_session_db()` 的 DELETE + INSERT 模式

**代码**: `session_persistence.rs:362-410`

```rust
fn flush_messages_to_session_db(&self, conn, session_id, messages) {
    // 删除该会话所有消息
    conn.execute("DELETE FROM messages WHERE session_id = ?1", [session_id])?;  // ← 危险!
    
    // 逐条插入当前内存状态
    for msg in messages {
        conn.execute("INSERT INTO messages (...) VALUES (...)", [...])?;
    }
}
```

**问题**: 这是"全量替换"模式,不是"增量追加"。中途崩溃,消息就没了。

---

## 七、架构改进方案

### 7.1 紧急修复 (优先级最高)

#### PR-1: `save_session` 三写原子性

**改哪个文件**: `crates/hermix-agent/src/session_persistence.rs`

**改法**:
1. 新增 `save_session_atomic()` 方法,包住 DB + md + json 三步
2. 用 `BEGIN IMMEDIATE` 事务包住 DB 操作
3. `save_session_log()` 改成**覆盖式写入** `<session_id>.md` (不再用时间戳)
4. 加 `recovery_sweep()` 函数,启动时扫 `<home>/sessions/` 目录,把"没有对应 DB 行"的 md 文件移到 `orphans/`

**测试**:
```rust
#[test]
fn atomicity_kill_between_db_and_md() {
    // mock `std::fs::write` 抛错
    // 断言: SQLite `sessions.message_count` 没增长
}
```

#### PR-2: `flush_messages` 改成增量 UPSERT

**改哪个文件**: `crates/hermix-agent/src/session_persistence.rs:362-410`

**改法**:
1. 给 `Message` 加 `id: Option<String>` 字段 (用 UUID 或 `session_id + role + created_at` 做复合键)
2. `flush_messages_to_session_db()` 改成:
   ```sql
   INSERT INTO messages (...) VALUES (...)
   ON CONFLICT(id) DO UPDATE SET
     content = excluded.content,
     tool_calls = excluded.tool_calls,
     extra = excluded.extra
   ```
3. 不再 DELETE,只 UPSERT

### 7.2 中期改进 (1-2 周)

#### PR-3: L1 反向链接

**改哪个文件**: `crates/hermix-focus/src/embedded.rs:127-141`

**改法**:
1. 加迁移: `ALTER TABLE frames ADD COLUMN source_session TEXT;`
2. `FocusFrame` 加字段
3. `FocusStack::conclude()` 时把 `current_session_id + turn_idx` 存进去
4. 新 API: `frames_for_session(session_id: &str) -> Vec<FocusFrame>`

#### PR-4: L2 反思显式字段

**改哪个文件**: `crates/hermix-agent/src/session_persistence.rs:137-148`

**改法**:
1. `messages` 表加 `kind TEXT DEFAULT 'message' CHECK(kind IN ('message','reflection','summary','lesson'))`
2. `SessionEntry` 枚举加 `Reflection(String)` / `LessonLearned(String)` 变体
3. 新方法: `record_reflection(session_id, turn_id, content)`
4. Agent loop 在检测到"上一轮答错"时调它

### 7.3 长期重构 (1 个月)

#### PR-5: 统一记忆写入路径

**目标**: L1(LSTM 帧)和 L2(会话消息)的写入应该用**同一个事务**协调,而不是各自为政。

**改法**:
1. 引入 `MemoryWriteBatch` 结构体
2. 新增 `MemoryCoordinator` 单例,负责接收 `MemoryWriteBatch` 并在一个事务里写所有 DB
3. 改 `AgentLoop`,不再直接调 `persist_session()`,而是构造 `MemoryWriteBatch` 交给 `MemoryCoordinator`

---

## 八、团队技术能力提升方案

### 8.1 代码审查清单 (避免再犯同类错误)

#### 持久化代码审查必查项:

1. ✅ 多写操作是否包在同一个事务里?
2. ✅ 文件名是否用"会话 ID"而非"时间戳"?(避免生成垃圾文件)
3. ✅ 崩溃恢复测试有没有?(模拟进程在中间被杀,重启后数据对不对?)
4. ✅ "真源"(DB)和"缓存"(md/json)的一致性谁维护?

#### 并发代码审查必查项:

1. ✅ `Mutex` 保护的连接,是不是每次都 `lock()` 拿新连接?(应该复用连接)
2. ✅ 有没有 `Connection::open()` 在循环里?(应该在循环外 open,循环内用同一个 conn)
3. ✅ 异步代码里有没有 blocking I/O?(SQLite 是同步的,要用 `tokio::task::spawn_blocking`)

### 8.2 技术债务追踪

**建议引入 `TECH_DEBT.md`**,记录每次 code review 发现的架构问题、每次生产事故的根因、每次重构的 before/after 对比。

### 8.3 测试覆盖率提升

**当前**: `cargo test --workspace --lib` 有 ~1834 个测试 (AGENTS.md 说的)

**问题**: 没看到"崩溃恢复测试"和"并发测试"

**建议加的测试类型**:

1. **崩溃恢复测试**: 用 `proptest` 随机在代码路径中间抛 `panic!`,然后用 `std::panic::catch_unwind` 捕获,检查数据一致性
2. **并发测试**: 用 `tokio::task::JoinSet` 并发调 `persist_session()`,检查 DB 没死锁、没数据竞争
3. **迁移测试**: 每次加 DB 迁移,都要有对应的 `test_migration_xxx()` 测试

---

## 九、附录: 关键代码位置索引

| 功能 | 文件路径 | 关键行号 |
|------|----------|----------|
| Agent Loop 主循环 | `crates/hermix-agent/src/agent_loop.rs` | - |
| 会话持久化 | `crates/hermix-agent/src/session_persistence.rs` | `:301-358` (persist_session), `:362-410` (flush_messages) |
| L1 记忆 SQLite | `crates/hermix-focus/src/embedded.rs` | `:127-141` (schema), `:196` (upsert_frame) |
| MemoryLayer trait | `crates/hermix-focus/src/memory_layer.rs` | `:14-200` |
| FocusStack | `crates/hermix-focus/src/focus_stack.rs` | - |
| 工具路由 | `crates/hermix-intelligence/src/` | - |
| 配置加载 | `crates/hermix-config/src/` | - |
| CLI 入口 | `crates/hermix-cli/src/main.rs` | - |
| Server 入口 | `crates/hermix-server/src/main.rs` | - |

---

**文档结束**

**下一步**: 将此文档交付给团队,作为架构基准。后续所有重构/新功能开发都必须对照此文档,确保不破坏已有架构。
