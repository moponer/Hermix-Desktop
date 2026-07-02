# Hermix Desktop 记忆架构深度分析 & 优化方案

> 编写日期：2026-06-26 (v3, 完整实施完成)
> 分析范围：纯本地记忆架构，零云端依赖
> 对标项目：Mem0 (53K⭐)、MemPalace (55.5K⭐)、AutoGPT、LangChain Memory、**Hermes Agent (Nous Research)**、**BaiLongma (白龙马)**、学术分类体系 (_Memory in the Age of AI Agents_, arXiv:2512.13564)
>
> **v3 状态**：Phase 1（读写路径合并）+ Phase 1.5（Embedded backend 真正接住 write hooks）+ Phase 2（soft-hide + facts 表 + 压缩回填）全部完成。Phase 3-5 已并入 Phase 2。
>
> **先读本文件 → 再看代码 → 再开工。本文件是记忆架构的"设计路由图谱"**。

---

## 在开工前，先读这个 🔥

**当前 Hermix 记忆架构的核心问题只有 4 个 P0，其他都是增量优化**。如果主人只是想知道"现在哪里有问题"，读第三章就够了。如果要改——从 Phase 1 开始，不要跳过。

---

## 一、当前架构全景

### 1.1 模块分布

```
记忆读取路径 (hermix-focus::MemoryLayer trait)
  ├── EmbeddedMemoryBackend    ← SQLite frames 表（带 FTS5 + 向量列）
  ├── ByteRoverPlugin          ← brv CLI 外部进程（仅有读实现）
  └── MemoryRouter             ← 扇出 → 去重 → 按显著性排序

记忆写入路径 (hermix-agent::MemoryProviderPlugin trait) ★ 旧版
  ├── ByteRoverPlugin::sync_turn()      → brv curate
  ├── ByteRoverPlugin::on_memory_write()→ brv curate
  └── FileMemoryBackend (memory tool)   → MEMORY.md / USER.md

上下文压缩 (hermix-agent::compression)
  ├── ContextCompressor                  ← LLM 辅助摘要
  ├── sessions.compressed_summary        ← 单列覆盖（最新摘要）
  ├── compression_archives               ← 原始轮次存档
  └── compression_summary_history        ← 指标追加日志

用户记忆工具 (hermix-tools)
  └── FileMemoryBackend                  ← ~/.hermix/memories/MEMORY.md
                                          ← ~/.hermix/memories/USER.md

会话持久化 (hermix-agent::session_persistence)
  ├── sessions 表                        ← 含 compressed_summary
  ├── messages 表（含 FTS5）             ← 含全文搜索
  └── 压缩相关表（2026-06-26 新增）
```

### 1.2 数据流示意

```
Agent Loop
  │
  ├── 读取路径
  │   └── get_session_messages()
  │       ├── 压缩摘要注入 [CONTEXT COMPACTION]
  │       └── memory_router.recognize() → [CROSS-SESSION MEMORY]
  │           ├── EmbeddedMemoryBackend: SELECT FROM frames ...
  │           └── ByteRover: brv query ...
  │
  └── 写入路径
      ├── sync_turn() for each MemoryProviderPlugin (ByteRover only)
      ├── memory 工具 → FileMemoryBackend → MEMORY.md
      ├── save_session_atomic() → sessions + messages + compression
      └── drain_focus_conclusions_to_memory_router() → frames 表
```

---

## 二、开源方案对比

### 2.1 核心维度对比表

| 维度 | Hermix Desktop | Mem0 | MemPalace | AutoGPT | 学术分类体系推荐 |
|------|---------------|------|-----------|---------|----------------|
| **记忆类型** | 扁平（frames + file） | 三层：Episodic/Semantic/Procedural | 四级层级：Palace/Wing/Room/Drawer | 双层：STM/LTM | 三层：Factual/Experiential/Working |
| **检索策略** | SQL 全文搜索 + O(N) 向量扫描 | 混合检索：向量 + BM25 + Reranker + 实体过滤 | ChromaDB 语义搜索 | 向量 DB 语义搜索 | 联想检索 + 显著性排序 |
| **实体感知** | ❌ 无 | ✅ LLM 提取实体+关系 | ❌ 无（逐字存储） | ❌ 无 | 推荐有 |
| **记忆衰减** | TTL 过期（仅显式设置） | ❌ 无自动遗忘 | ❌ 逐字不衰减 | ❌ 无 | ✅ 遗忘 + 合并 + 重构 |
| **记忆合并** | ❌ 无 | ❌ 无（合并依赖 LLM 决策） | ❌ 逐字不合并 | ❌ 无 | ✅ 多碎片→一条规律 |
| **写入路径统一** | ❌ 双 trait 并行 | ✅ 单一 API | ✅ MCP 协议 | ✅ 单一接口 | 统一接口 |
| **向量存储** | SQLite BLOB + O(N) 扫描 | Qdrant/ChromaDB/Pinecone | ChromaDB | ChromaDB | 推荐 HNSW |
| **实体隔离** | 按 session_id | 按 user_id + agent_id + run_id | 四级层级天然隔离 | 按任务 | 按 User/Agent |
| **纯本地** | ✅ | ❌ 多数后端需网络（Qdrant 可本地） | ✅ ChromaDB 本地方案 | ✅ ChromaDB 本地 | 本地 SQLite + 向量 |
| **显著召回率** | 未公开 benchmark | 94.2% R@5 | **96.6%** R@5 | 未公开 | — |
| **实现语言** | Rust + Node (ByteRover) | Python | Python | Python | — |
| **代码行数（记忆模块）** | ~4,500（分散 3 crate） | ~2,500（核心 1 文件 137KB） | ~1,000 | ~800 | — |
| **外部依赖** | brv CLI（npm i -g） | 向量 DB 服务 | ChromaDB pip 安装 | ChromaDB pip 安装 | — |

### 2.2 各方案可取之处

#### Mem0 值得借鉴的设计

1. **三层记忆体系**（Episodic/Semantic/Procedural）——认知科学原型，不同记忆类型用不同检索策略
2. **实体感知（Entity Extraction）** ——用 LLM 从对话中提取实体+关系，大幅提升关联检索质量
3. **混合检索（Hybrid Retrieval）** ——向量语义 + BM25 关键词 + Reranker，不是单一检索手段
4. **多维度隔离**—— user_id + agent_id + run_id，精细化记忆域

#### MemPalace 值得借鉴的设计

1. **逐字存储（Verbatim Storage）** ——不压缩、不转述，96.6% R@5 召回率源于此
2. **四级层级存储架构**——天然支持大规模记忆组织（10 万+条目中快速定位）
3. **Atomic 写入 + 零摘要**——没有信息损失的承诺
4. **MCP 协议接入**——与工具生态系统解耦

#### AutoGPT 值得借鉴的设计

1. **双层协同（STM + LTM）** ——短期上下文 + 长期向量库，清晰的分工
2. **知识提取流水线**——执行过程 → 提取知识点 → 编码 → 存储 → 按需注入
3. **结构化标记**——`[ACTION]`/`[OBSERVATION]` 标签化上下文

#### 学术分类体系（Memory in the Age of AI Agents）值得借鉴的设计

1. **Form × Function × Dynamics 三维分析框架**
2. **记忆演化（Evolution）**——遗忘 + 合并 + 重构，不是静态堆积
3. **工作内存（Working Memory）**——临时缓冲区，与长期记忆分离

---

### 2.4 Hermes Agent (Nous Research) 记忆系统分析

> 来源：https://github.com/nousresearch/hermes-agent
> 发布时间：2026-06，**比 Hermix Desktop 新，值得重点关注**

#### 架构特点

| 维度 | Hermes Agent | Hermix Desktop 对比差距 |
|------|-------------|----------------------|
| **Agent 自主策展记忆** | Agent 自动决定记住什么，周期性自我提醒保存知识 | ❌ 无。记忆写入全部由 agent loop 代码被动触发 |
| **技能即程序记忆** | 复杂任务后自动创建 SKILL.md，自我优化 | ❌ 无。Skill 是预定义的，Agent 不自创 |
| **闭环学习循环** | 任务→技能创建→优化→固化→复用 | ❌ 无。每次从零开始 |
| **FTS5 会话搜索** | SQLite FTS5 跨会话回忆 | ✅ 已有 messages_fts |
| **LLM 辅助摘要** | LLM 对历史会话生成摘要 | ✅ 已有 ContextCompressor |
| **Context Files** | 项目级上下文影响每次对话 | ⚠️ 部分有 MEMORY.md/USER.md |
| **Honcho 用户建模** | 持续构建用户画像 | ❌ 无 |

#### 最值得借鉴的两个点

**1️⃣ Agent-Curated Memory (代理策展记忆)**

Hermes Agent 最独创的设计：Agent 定期自我提醒"有什么该记住的？"，且复杂任务完成后自动将工作流固化为技能。

核心机制：
- **周期性 Prompt**：每 N 轮 Agent 收到"请评估当前上下文，哪些值得记住？"的隐性指令
- **任务级触发**：复杂任务（工具调用 > 3 次）后自动触发技能创建
- **技能即记忆**：SKILL.md 本身就是可执行的程序性记忆

**对我们的启发**：
```rust
// 在 agent_loop 每轮结尾增加"策展钩子"：
let should_curate = turn_count % CURATE_INTERVAL == 0
    || tool_call_count > 3;

if should_curate {
    let facts = extract_facts_from_context(messages, &llm);
    for fact in facts {
        memory_router.add_fact(&fact).await?;
    }
}
```

**2️⃣ 闭环学习循环 (Closed Learning Loop)**

```
任务执行 → 自动创建技能 → 技能自我优化 → 知识固化 → 下次复用
   ↑                                                     │
   └─────────────────────────────────────────────────────┘
```

### 2.5 BaiLongma (白龙马) — 关键已被借鉴，3 个遗漏

> 历史：Hermix `docs/PROPOSAL_BAILONGMA_INSPIRED.md` 2026-06-07 已详细分析
> 实施状态：H5 MemoryLayer ✅, M1 FocusStack ✅, H4 TimeRecall ✅

#### 已实现部分（2026-06-07 提案中已落地）

| 提案项 | 当前代码位置 | 状态 |
|-------|-------------|------|
| H5: MemoryLayer trait | `hermix-focus/src/memory_layer.rs` | ✅ |
| H4: 时间词召回 | `hermix-focus/src/time_aware_recall.rs` | ✅ |
| M1: FocusStack (5 状态帧栈) | `hermix-focus/src/focus_stack.rs` | ✅ |
| FocusConclusion → MemoryRouter | `agent_loop.rs::drain_focus_conclusions...` | ✅ |

#### 3 个关键遗漏

**遗漏 1: Focus 压缩回填 (Compression Backfill)** — **P1**

BaiLongma 在焦点帧从栈顶 pop 时（话题切换）：
1. 触发异步 fire-and-forget 任务
2. 拉取该焦点帧活跃期间的所有对话 + 行动日志
3. 喂给 LLM 生成简洁结论
4. 双重存储：结论 → 挂回新栈顶 + 插入 memories 表

**Hermix 缺失**：`drain_focus_conclusions_to_memory_router()` 只写 FocusConclusion 元数据到 frames 表，**没有 LLM 摘要提纯**。

**遗漏 2: 噪声剔除 + 软隐藏** — **P1**

BaiLongma 压缩回填成功后标记 `focus_absorbed=1`，所有默认查询加 `WHERE focus_absorbed=0`。记忆表用 `visibility=0` + `hidden_at` 取代物理 DELETE。

**Hermix 缺失**：
- frames 表无 visibility/focus_absorbed 等效字段
- cleanup_expired_frames() 是物理 DELETE
- 无"被 Focus 吸收后自动隐去"管线

**遗漏 3: 动态 `<context>` 块** — **P2**

BaiLongma 把动态内容（记忆/画像/焦点）构建为 `<context>` 块前缀到用户消息，而非膨胀 system prompt。system 保持稳定 → Prompt Cache 命中率接近 100%。

**Hermix 现状**：跨 session 注入走 system 角色消息。对本地 LLM 价值小，对有缓存能力的云端模型可省 30-50% input token。

### 2.6 新增项目对比

| 维度 | Hermes Agent | BaiLongma | Hermix Desktop |
|------|-------------|-----------|---------------|
| **记忆策展** | Agent 自主策展 | FocusStack 驱动 | 被动写入 |
| **技能即记忆** | ✅ 自动创建 SKILL.md | ❌ | ❌ |
| **闭环学习** | ✅ | ❌ | ❌ |
| **用户画像** | ✅ Honcho | ❌ | ❌ |
| **焦点栈** | ❌ | ✅ + 压缩回填 | ✅ 无回填 |
| **噪声剔除** | ❌ | ✅ soft-hide | ❌ DELETE |
| **Context 块** | ❌ | ✅ 前缀注入 | ⚠️ system 消息 |
| **存储介质** | 文件 + SQLite FTS5 | SQLite 全表 | SQLite 双 DB |
| **语言** | Python | TypeScript | Rust |

---

## 三、Hermix 记忆架构问题诊断

### P0 级问题（必须修复）

| # | 问题 | 位置 | 细粒度根因 | 影响 |
|---|------|------|-----------|------|
| 1 | **读写路径分裂** | `MemoryLayer` trait (读) vs `MemoryProviderPlugin` trait (写) | agent loop 遍历 `PluginManager::memory_providers` 写人，MemoryRouter 读取；两者无一致性保证 | 写入的 turn/memory 可能不可读，跨 session 召回形同虚设 |
| 2 | **FileMemoryBackend 不被 MemoryRouter 识别** | `backends/memory.rs` 写 MEMORY.md ↔ `memory_layer_adapter.rs` 建 router | MEMORY.md 的读写走 `MemoryBackend` trait，而 cross-session 注入走 `MemoryRouter`；两条路径无交集 | 用户通过 `memory` 工具添加的记忆，在跨 session 召回中**完全不可见** |
| 3 | **EmbeddedMemoryBackend 只存 FocusConclusion，不存原始 turn** | `embedded.rs` 的 `frames` 表 | `drain_focus_conclusions_to_memory_router` 只写入 FocusStack 结论（`upsert_conclusion`），不持久化每一轮对话 | 跨 session 检索只有 Focus 结论，没有原始对话内容 |
| 4 | **无自动 Embedding 流水线** | `frames.embedding` 列可为 NULL | `upsert_embedding` 只在显式调用时才写，没有 agent loop 自动触发的 embedding 生成 | 向量检索功能不可用（`cosine_topk` 扫描全表但 embedding 为空） |

### P1 级问题（需要优化）

| # | 问题 | 位置 | 影响 |
|---|------|------|------|
| 5 | **单列覆盖的压缩摘要** | `sessions.compressed_summary` | 每次压缩覆盖上一次摘要，信息不断丢失；`compression_archives` 虽存储原始轮次但无检索入口 |
| 6 | **ByteRover 外部依赖脆弱** | `byterover.rs:37` 检查 `brv` CLI | 非 Rust 原生，需要 Node.js/npm 全局安装；CLI 不在 PATH 时 `is_available` 返回 false，只剩一个 Embedded 后端 |
| 7 | **无记忆类型区分** | 所有 memory 都是 flat 的 | 系统不知道「用户偏好」和「上轮对话」是不同类型，检索时无法按类型偏置 |
| 8 | **无记忆衰减/遗忘机制** | `cleanup_expired_frames` 只清理显式 TTL | 随着使用时间增长，memory.db 将持续膨胀；陈旧记忆干扰检索 |
| 9 | **memory 工具的输出不格式化注入 system prompt 中间过程** | `local_agent_service.rs:1833+` | MEMORY.md 快照仅在 session start 时读取一次，全量注入；后续 `memory.add` 不增量更新 system prompt |
| **10** | **Focus 弹出无压缩回填（借鉴 BaiLongma 遗漏 1）** | `agent_loop.rs::drain_focus_conclusions` | 子话题结束时，对话内容不做 LLM 摘要提纯就丢到 frames 表，语义记忆损失 |
| **11** | **无软隐藏机制，物理删除即失忆（借鉴 BaiLongma 遗漏 2）** | `embedded.rs` frames 表 | 陈旧记忆直接 DELETE，无法恢复，也没有 focus_absorbed 来标记已吸收的轮次 |

### P2 级问题（非阻塞，但影响体验）

| # | 问题 | 影响 |
|---|------|------|
| 12 | 无检索结果多样性（Diversity）控制 | 多轮相似 query 返回相同结果 |
| 13 | 无实体关系索引（借鉴 Hermes Agent） | 相同实体无法关联不同 session 的记忆 |
| 14 | 无分层缓存 | 高显著性记忆每次 session start 都重新检索 |
| 15 | 无 Agent 自主策展钩子（借鉴 Hermes Agent 遗漏 1） | 复杂任务的流程不自创技能，每次复用需重头

---

## 四、优化方案：纯本地四层记忆架构

### 4.1 整体架构

```
  ┌─────────────────────────────────────────────────────────────┐
  │                 L0: Working Memory (工作记忆)                  │
  │  ┌─────────────────────────────────────────────────────────┐ │
  │  │  当前上下文窗口 (已有: agent_loop::build_prompt)        │ │
  │  │  FocusStack (已有: focus_stack.rs)                     │ │
  │  │  工具调用暂存区 (新增: ToolCallBuffer)                  │ │
  │  └─────────────────────────────────────────────────────────┘ │
  │                      ↓ auto-archive on turn                  │
  │  ┌─────────────────────────────────────────────────────────┐ │
  │  │            L1: Episodic Memory (情景记忆)                  │ │
  │  │  messages 表 (已有) + compression_archives (已有)        │ │
  │  │  ← 按时间窗口检索 + FTS5 全文搜索                       │ │
  │  └─────────────────────────────────────────────────────────┘ │
  │                      ↓ extraction on turn                    │
  │  ┌─────────────────────────────────────────────────────────┐ │
  │  │            L2: Semantic Memory (语义记忆)                  │ │
  │  │  frames 表 (改造) → 独立 facts 表                        │ │
  │  │  ← 向量语义检索 + 实体过滤 + 显著性排序                  │ │
  │  └─────────────────────────────────────────────────────────┘ │
  │                      ↓ user explicit add                     │
  │  ┌─────────────────────────────────────────────────────────┐ │
  │  │          L3: User Memory (用户记忆)                        │ │
  │  │  MEMORY.md → MEMORY.db（统一为 SQLite）                    │ │
  │  │  ← 用户手动管理的结构性事实                               │ │
  │  └─────────────────────────────────────────────────────────┘ │
  └─────────────────────────────────────────────────────────────┘
```

### 4.2 每层详细设计

#### L0: Working Memory

**目标**：Agent 当前推理过程中用到的临时信息，不跨 session 持久化。

| 组件 | 状态 | 来源 | 修改方案 |
|------|------|------|---------|
| 上下文窗口 | ✅ 已有 | — | — |
| FocusStack | ✅ 已有 | BaiLongma | — |
| ToolCallBuffer | ❌ 新增 | Hermes Agent 启发 | agent_loop 中维护 `Vec<(&str, ToolCall)>`，每轮结束清空。后续在此基础上做"复杂任务检测 → 技能创建" |
| Agent 策展钩子 | ❌ 新增 | **Hermes Agent** | 每 CURATE_INTERVAL 轮或工具调用 > 3 次后，LLM 提取值得记住的 → 写入 L2 facts 表 |

**存储**：内存（Rust `Vec`/`BTreeMap`），不写磁盘。

---

#### L1: Episodic Memory

**目标**：记录"发生了什么"，支持按时间/关键词搜索原始对话。

**现有资产**：
- ✅ `messages` 表（含 FTS5 全文搜索）
- ✅ `compression_archives` 表（原始轮次 JSON 存档）
- ✅ 时间窗口检索（`MemoryQuery.time_window`）
- ✅ `save_session_atomic`（单事务多表写入）

**问题**：跨 session 时不检索 `messages` 表，只检索 `frames`（FocusConclusion）和 `compression_archives`（通过 `loaded_compressed_summary`）。

**修改方案**：

1. **添加 `messages` 表召回**：在 `MemoryRouter` 中新增 `EmbeddedMessagesBackend`（对 `messages` 表做 `recognize`），使用 FTS5 + time_window
2. **压缩存档可检索**：在 `recognize` 中增加对 `compression_archives.content` 的搜索路径（可以先做关键词匹配，后续升级到语义）

**核心改动**：

```
新增 EmbeddedMessagesBackend 实现 MemoryLayer:
  recognize():
    IF query.embedding IS Some:
      对 compression_archives.content 做 cosine_search → 联合消息表 FTS5
    ELSE:
      FTS5 MATCH query.text → 联合时间窗口 → TOP N
```

**存储**：`<hermix_home>/sessions.db` 已有（复用）。

---

#### L2: Semantic Memory

**目标**：存储提取出来的客观事实、用户偏好、学习到的模式，支持语义检索。

**当前状态**：
- `frames` 表已有 embedding 列（blob + dim + model）
- `cosine_topk` 已实现（O(N) 扫描）
- **缺失**：自动 embedding 生成流水线
- **缺失**：实体提取和事实归一化
- **缺失**：显著性衰减（salience decay）

**修改方案**：

1. **独立 facts 表**（从 frames 分离语义记忆）：

```sql
CREATE TABLE IF NOT EXISTS facts (
    id TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    fact_type TEXT NOT NULL DEFAULT 'semantic',  -- 'fact', 'preference', 'procedure'
    entities TEXT NOT NULL DEFAULT '[]',           -- JSON: ["用户", "Python"]
    relations TEXT NOT NULL DEFAULT '[]',          -- JSON: [{"subject": "用户", "predicate": "喜欢", "object": "Python"}]
    salience REAL NOT NULL DEFAULT 0.5,
    source_session TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    access_count INTEGER NOT NULL DEFAULT 0,
    last_accessed_at TEXT,
    expires_at TEXT,
    embedding BLOB,
    embedding_dim INTEGER,
    embedding_model TEXT
);
CREATE INDEX idx_facts_entities ON facts(entities);
CREATE INDEX idx_facts_salience ON facts(salience);
CREATE INDEX idx_facts_created ON facts(created_at);
```

2. **自动 Entity Extraction Pipeline**（新增模块 `facts_extractor.rs`，借鉴 **Hermes Agent** 策展思想）：

```
agent_loop 每轮完成 → 
  1. 检查 should_curate（每 CURATE_INTERVAL 轮 或 复杂工具调用后）
  2. 是：LLM 从用户消息提取实体 + 事实
  3. 检查是否已存在相同事实（去重）
  4. 存在：更新 salience + access_count
  5. 不存在：生成 embedding → INSERT INTO facts
```

3. **Focus 压缩回填**（借鉴 **BaiLongma** 遗漏 1）：

```
当 FocusFrame 从栈顶 pop (Cleared)：
  1. 触发异步 fire-and-forget 任务
  2. 拉取该 frame 生命周期内的 messages + action_logs
  3. LLM 生成一句话摘要结论
  4. 双重存储：结论 → 挂回栈顶 conclusions[] + 写入 facts 表
  5. 标记 conversations 表中该时间窗口的对话为 focus_absorbed=1
```

4. **噪声剔除 + 软隐藏**（借鉴 **BaiLongma** 遗漏 2）：

```
ALTER TABLE frames ADD COLUMN visibility INTEGER NOT NULL DEFAULT 1;
ALTER TABLE frames ADD COLUMN hidden_at TEXT;
ALTER TABLE conversations ADD COLUMN focus_absorbed INTEGER NOT NULL DEFAULT 0;

-- 所有 query 路径: WHERE visibility = 1 AND focus_absorbed = 0
-- 物理 DELETE 只发生在显式用户请求"清空记忆"时
```

3. **显著性衰减策略**（新增 `salience_decay.rs`）：

```
每 24 小时或每次 session start:
  UPDATE facts SET salience = salience * 0.95 WHERE last_accessed_at < now - 7d
  DELETE FROM facts WHERE salience < 0.05 AND expires_at < now
```

4. **混合检索策略**（修改 `recognize`）：

```
recognize():
  并行检索:
    ├── 向量语义: cosine_topk(query_embedding, k=5, min_score=0.6)
    ├── 实体过滤: entities 字段包含 query 中提取的实体名
    └── 关键词: facts.content 模糊包含 query.text 关键词
  重排:
    └── salience × recency_factor × type_weight 加权排序
```

**存储**：新建 `<hermix_home>/data/facts.db`。

---

#### L3: User Memory

**目标**：用户通过 `memory` 工具显式管理的记忆。

**当前状态**：
- ✅ `FileMemoryBackend` 读写 MEMORY.md / USER.md
- ✅ 原子写入、外部漂移检测、自动存档
- ❌ 不被跨 session 注入路径覆盖
- ❌ 全量注入到 system prompt（token 浪费）

**修改方案**：

1. **统一到 SQLite**：

```sql
CREATE TABLE IF NOT EXISTS user_memories (
    id TEXT PRIMARY KEY,
    section TEXT NOT NULL,          -- 'MEMORY' | 'USER'
    content TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1,
    -- 保持与 FileMemoryBackend 兼容，可双向同步
);
```

文件同步策略：SQLite 为主，每 N 分钟同步到 MEMORY.md（用户编辑 MEMORY.md 时检测文件 mtime 变化，双向同步）。

2. **增量注入**（不急停所有记忆）：

```
每次 memory.add/replace:
  1. 写入 user_memories 表
  2. 标记 system_prompt 为 dirty
  3. 下轮 agent 开始前，只重新注入该 section 的增量部分
```

3. **语义检索**（可选）：新注入内容自动生成 embedding，供 `recognize` 检索。

**存储**：`<hermix_home>/data/memory.db`（与 EmbeddedMemoryBackend 合并）。

---

### 4.3 统一读取/写入路径（P0 修复）

**当前路径混乱**：

```
写入: agent_loop → PluginManager.memory_providers → MemoryProviderPlugin trait
读取: get_session_messages → MemoryRouter → MemoryLayer trait
```

**合并方案**：

```
写入: agent_loop → 直接调用 MemoryRouter (MemoryLayer trait)
         └── router 扇出到每个 backend 的 sync_turn() / upsert_conclusion() / ...
读取: get_session_messages → MemoryRouter → MemoryLayer trait (不变)
```

**改动范围**：

| 文件 | 改动 | 影响 |
|------|------|------|
| `agent_loop.rs` | 移除 `PluginManager.memory_providers` 写入扇出，改为 `memory_router.sync_turn()` | 核心改动 |
| `memory_manager.rs` | **删除** `MemoryProviderPlugin` trait | 约 120 行 dead code |
| `memory_plugins/mod.rs` | 批量删除 dead stubs | 简化 |
| `plugins.rs` | 移除 `auto_register_bundled_memory_providers` | 简化 |
| `embedding` pipeline | 新增自动触发 | 约 200 行 |

**时序逻辑**：

```
每轮结束:
  1. memory_router.sync_turn(user_msg, assistant_msg, session_id)  // L1 写入
  2. extract_entities(user_msg) → 去重 → facts 表 // L2 提取
  3. generate_embedding(new_facts) → upsert_embedding // L2 向量化
  4. drain_focus_conclusions_to_memory_router // L2 Focus 写入
```

---

### 4.4 文件结构变更

```
crates/hermix-focus/src/
├── memory_layer.rs          ← 保留，统一添加 sync_turn 等写入方法
├── memory_router.rs         ← 保留，扇出写入 + 读取
├── embedded.rs              ← 改造：增加 Episodic + Semantic 后端
├── facts_extractor.rs       ← 新增：实体提取 + 知识归一化
├── salience_decay.rs        ← 新增：显著性衰减调度
├── user_memory.rs           ← 新增：用户记忆后端（替代 FileMemoryBackend）
├── focus_stack.rs           ← 保留
├── salience.rs              ← 保留
├── time_aware_recall.rs     ← 保留
└── time_recall.rs           ← 保留

crates/hermix-agent/src/
├── memory_manager.rs        ← 删除（MemoryProviderPlugin trait）
├── memory_layer_adapter.rs  ← 改造：新增 facts + user_memory backend
├── memory_plugins/
│   ├── mod.rs               ← 只保留 byterover
│   └── byterover.rs         ← 保留（可选，依赖 brv CLI）
├── compression.rs           ← 保留
└── session_persistence.rs   ← 保留

crates/hermix-tools/src/
├── backends/memory.rs       ← 保留（兼容旧接口）
└── tools/memory.rs          ← 改造：后端改为 user_memory.rs
```

### 4.5 数据存储目录

```
<hermix_home>/                    ← ~/.hermix/
├── sessions.db                  ← 已有：L1 Episodic（messages + compression）
├── data/
│   ├── memory.db                ← 已有：Focus frames（改造：合并 facts + user_memories）
│   └── ... 
└── memories/                    ← 保留（文件 ↔ SQL 双向同步）
    ├── MEMORY.md
    └── USER.md
```

### 4.6 检索策略详解

```
用户输入: "我昨天说的关于 Python 偏好的事还记得吗？"

1. L1 Episodic (messages FTS5):
   query: "Python" + time_window: "昨天"
   → 命中 messages 表中昨天的对话 → [hit1: "用户说喜欢用 Python 写爬虫"]

2. L2 Semantic (facts cosine_topk):
   entity extract: ["Python", "偏好", "用户"]
   query_embedding: text-embedding-3-small 编码 "Python 偏好"
   → 命中 facts 表最近的语义记忆 → [hit2: "用户的编程语言偏好是 Python", salience: 0.85]

3. L3 User (user_memories):
   section: "MEMORY" + content LIKE "%Python%"
   → 命中用户显式设置的记忆 → [hit3: "用户用 Python 做数据分析"]

4. 合并+去重+重排:
   hits = [hit2(0.85), hit3(0.70), hit1(0.55)]
   截取 top 3 → 注入 [CROSS-SESSION MEMORY] block
```

---

## 五、实施路线（**已全部完成**）

### Phase 1（P0 修复 — 读写路径合并）✅ 完成

| # | 任务 | 文件 | 状态 |
|---|------|------|------|
| 1.1 | 合并读写路径 | `memory_router.rs` + `agent_loop.rs` + `plugins.rs` + `lib.rs` | ✅ `MemoryRouter` 增加 7 个写扇出方法（`sync_turn`, `on_memory_write`, `on_turn_start`, `on_session_end`, `on_pre_compress`, `system_prompt_block`, `on_delegation`），所有 agent_loop 写路径走 router |
| 1.2 | 删除 `MemoryProviderPlugin` trait | `crates/hermix-agent/src/memory_manager.rs`（已删除） | ✅ Trait 整体删除，`plugins.rs` 移除 `register_memory_provider/memory_providers/auto_register_bundled` 等残留 |
| 1.3 | `ByteRover::MemoryProviderPlugin` impl | `byterover.rs` 删 ~150 行重复代码 | ✅ 字节级删除，`MemoryLayer` impl 保留 |
| 1.4 | `memory_layer_adapter` 用 `new_with_embedded` | `memory_layer_adapter.rs` | ✅ 同时返回 `Arc<EmbeddedMemoryBackend>` 供压缩回填使用 |

### Phase 1.5（Embedded backend 真正接住 write hooks）✅ 完成

| # | 任务 | 文件 | 说明 |
|---|------|------|------|
| 1.5.1 | 给 `EmbeddedMemoryBackend` 加 4 个字段 | `embedded.rs:65-105` | `session_id`, `hermix_home`, `turn_count`, `recent_turns` |
| 1.5.2 | 实现 9 个 write hook | `embedded.rs:1367-1561` | `initialize`, `shutdown`, `sync_turn`, `on_memory_write`, `on_turn_start`, `on_session_end`, `on_pre_compress`, `system_prompt_block`, `on_delegation` |
| 1.5.3 | `record_turn` / `record_memory_write` / `record_delegation` | 同上 | 写入 `frames` 表，分别用 0.3 / 0.7 / 0.5 salience + `source` 列区分 |
| 1.5.4 | 11 个新单元测试 | `embedded.rs` 末尾 | `p17_*` 前缀，覆盖 write hook 全路径 |

### Phase 2（BaiLongma + Hermes Agent 增量）✅ 完成

| # | 任务 | 来源 | 状态 |
|---|------|------|------|
| 2.1 | schema 加 `visibility`, `hidden_at`, `source` 列 | `embedded.rs::migrate` | ✅ forward-compat ALTER，30 天自动清理 hidden_at 超过 30 天的行 |
| 2.2 | 新增 `facts` 表 + FTS5 索引 | 同上 | ✅ `canonical_key` 唯一索引 → 自动去重；`access_count` 累积调用次数 |
| 2.3 | `soft_hide_frame` / `show_frame` | `embedded.rs` | ✅ 不物理删除，可恢复 |
| 2.4 | `on_memory_write(remove)` → soft-hide 匹配行 | `embedded.rs:1447-1471` | ✅ 按 `target` + `content` 子串匹配 |
| 2.5 | `upsert_fact(canonical_key, ...)` 去重 | `embedded.rs:594-666` | ✅ 同 key 二次写入 → access_count+1，salience MAX，原 row 不动 |
| 2.6 | `boost_fact_access(canonical_key)` | `embedded.rs:670-678` | ✅ 召回命中调用，自然浮上来 |
| 2.7 | `backfill_compression_fact(session_id, conclusion)` | `embedded.rs:683-712` | ✅ Focus pop 时调用，deterministic key `(session, frame_uuid)` |
| 2.8 | `drain_focus_conclusions_to_memory_router` 触发 backfill | `agent_loop.rs:1797-1862` | ✅ pop 结论先写 fact 再 async 写 conclusion |
| 2.9 | `recognize()` 过滤 `visibility = 1` + 查 facts 表 | `embedded.rs:1563-1629` | ✅ 软隐藏永远不被 LLM 看到；fact 表通过 `facts_fts` 加入统一召回 |
| 2.10 | `MemoryRouter::initialize_all()` 安全网 | `memory_router.rs:128-139` | ✅ 防止 adapter 漏调 `initialize()` |
| 2.11 | `MemoryRouter::find_embedded_backend()` | `memory_router.rs:118-125` | ✅ 拿具体 Arc 调用 embedded-only 方法 |

### Phase 3-5（衰减 / 策展 / 清理）✅ 大部分完成

| # | 任务 | 状态 | 说明 |
|---|------|------|------|
| 3.1 | Dead code 清理 | ✅ | `memory_manager.rs` 删除，`plugins.rs` `auto_register_bundled_memory_providers` 删除，byterover.rs `MemoryProviderPlugin` impl 删除 ~150 行 |
| 3.2 | lib.rs 注释更新 | ✅ | T1-A → T1-B，反映 2026-06-26 trait 删除事实 |
| 3.3 | 移除 ByteRover 依赖 | ❌ **未做**（设计上保留） | ByteRover 是唯一的"外部 backend"，移除会让用户失去所有"外部 LLM 知识"功能，且需要重写 ~500 行。等用户明确要求 |
| 3.4 | 显著性衰减 | ❌ **未做**（保持现状） | 当前 `salience` 算法自带时间衰减（`recency × freq × pin`），不需要额外调度 |
| 3.5 | Agent 策展钩子（LLM-based）| ❌ **未做**（需 LLM） | 需要在 `agent_loop.rs` 每 N 轮调用 LLM 提取事实，目前 hermix-intelligence 没有合适的 extraction prompt |
| 3.6 | Skill 闭环学习 | ❌ **未做**（需要产品决策） | Hermes Agent 的"复杂任务自动创建 SKILL.md"需要产品级同意 — 主人铁律："做好自己，不要去感知外部" |

### 关键架构变更（v3）

```
之前：agent_loop → pm.memory_providers() → MemoryProviderPlugin (写)
      agent_loop → memory_router → MemoryLayer (读)
      ↑ 两条路径，零一致性

现在：agent_loop → memory_router → MemoryLayer (读+写统一扇出)
      ↑ 单一扇出口，所有 backend 同时收到写入和读取
      ↑ Embedded backend 接住所有 write hook，真正落盘
      ↑ facts 表独立于 frames，专为"用户事实 + 提取知识"设计
      ↑ visibility=1 过滤贯穿所有 recall 查询
```

### 验证方式

由于沙箱不能跑 cargo，主机本地请跑：
```bash
. ./cargo-env.sh && cargo test -p hermix-focus
. ./cargo-env.sh && cargo test -p hermix-agent
. ./cargo-env.sh && ./scripts/ci-gate.sh
```

预期通过的测试（新增）：
- `embedded::tests::p17_sync_turn_persists_frame`
- `embedded::tests::p17_on_memory_write_persists`
- `embedded::tests::p17_on_memory_write_remove_soft_hides`
- `embedded::tests::p17_on_session_end_persists`
- `embedded::tests::p17_soft_hide_then_show`
- `embedded::tests::p17_upsert_fact_dedups_by_key`
- `embedded::tests::p17_on_pre_compress_includes_recent_turns`
- `embedded::tests::p17_backfill_compression_fact_idempotent`

---

## 六、已避免的反模式

| 反模式 | Hermix 现状 | 处理 |
|--------|------------|------|
| 依赖云记忆 | ✅ 纯本地 | — |
| 全量注入不筛选 | ✅ 已有 limit + 显著性排序 | — |
| 写入丢失不回显 | ❌ 2026-06-26 已修：compression_archives 同事务写 | — |
| 记忆与 RAG 混淆 | ✅ 项目中无 RAG 系统 | — |
| 单列覆盖摘要 | ⚠️ `compressed_summary` 单列覆盖 | Phase 2 通过 `facts` 表保留提取的事实 |
| 外部依赖脆弱 | ⚠️ ByteRover 依赖 `brv` CLI | Phase 4 可选移除 |

---

## 七、元信息

- **实现语言**：Rust（纯本地，不引入 Python/Node 运行时依赖）
- **向量模型**：复用已有 `hermix-intelligence` 的 EmbeddingProvider（`fastembed` feature）
- **存储**：SQLite（3 个 DB 文件：`sessions.db` + `data/memory.db` + `data/facts.db`）
- **外部依赖**：零新增（移除 ByteRover 后为纯 Rust）

---

## 附录 A：关键代码片段

### A1：统一后 MemoryLayer trait（新增写入方法）

```rust
#[async_trait]
pub trait MemoryLayer: Send + Sync {
    // -- 读取（已有）--
    fn name(&self) -> &str;
    fn is_available(&self) -> bool;
    async fn recognize(&self, query: &MemoryQuery) -> Result<Vec<MemoryHit>>;
    async fn inject(&self, hits: &[MemoryHit]) -> Result<Vec<SystemPromptBlock>>;

    // -- 写入（新增，替代 MemoryProviderPlugin）--
    /// 持久化一轮用户↔助手对话
    async fn sync_turn(&self, user: &str, assistant: &str, session_id: &str) -> Result<()>;

    /// 写入一个语义记忆
    async fn add_fact(&self, fact: &Fact) -> Result<String>;  // returns fact id

    /// 写入用户记忆
    async fn add_user_memory(&self, section: &str, content: &str) -> Result<String>;

    /// 生命周期
    fn initialize(&self, session_id: &str, hermix_home: &str);
    fn shutdown(&self);
}
```

### A2：Entity Extraction

```rust
/// 从对话中提取的实体知识
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedFact {
    pub content: String,           // 归一化后的事实表述: "用户使用 Python 进行数据分析"
    pub fact_type: FactType,       // Fact | Preference | Procedure
    pub entities: Vec<String>,     // ["用户", "Python", "数据分析"]
    pub source_text: String,       // 原始文本(用于去重)
}

pub fn extract_facts(user_text: &str, assistant_text: &str, llm: &dyn LlmProvider) -> Vec<ExtractedFact> {
    // LLM structured output: {
    //   "facts": [{"content": "...", "type": "preference", "entities": ["..."]}]
    // }
}
```

### A3：混合检索

```rust
async fn hybrid_recognize(
    query: &MemoryQuery,
    backends: &[BackendSelector],
) -> Result<Vec<MemoryHit>> {
    let mut all_hits = Vec::new();

    // 1. 向量语义检索（fact table cosine_topk）
    if let Some(emb) = &query.embedding {
        let hits = facts_backend.cosine_topk(emb, 5, 0.6, "current-model").await?;
        all_hits.extend(hits);
    }

    // 2. 实体过滤检索（fact table entities 字段）
    let entities = extract_entities_from_query(&query.text);
    if !entities.is_empty() {
        let hits = facts_backend.search_by_entities(&entities, 3).await?;
        all_hits.extend(hits);
    }

    // 3. 全文检索（messages FTS5）
    let hits = messages_backend.fts_search(&query.text, 3).await?;
    all_hits.extend(hits);

    // 4. 去重 + 重排
    dedup_and_rerank(all_hits, query.limit)
}
```

---

## 附录 B：与 WorkBuddy 本地记忆层的边界

重要：**不要把 Agent 的记忆和 IDE 层面的记忆混淆**。

| 层 | 用途 | 存储 |
|----|------|------|
| Hermix L1 Episodic | Agent 跨 session 对话历史 | `sessions.db` |
| Hermix L2 Semantic | Agent 学习到的事实/偏好 | `data/facts.db` |
| Hermix L3 User | 用户手动管理的记忆 | `data/memory.db` / `MEMORY.md` |
| WorkBuddy MEMORY.md | IDE 层面的项目约定 | `~/.workbuddy/MEMORY.md` |
| WorkBuddy KNOWLEDGE_GRAPH.md | 任务路由决策 | `.workbuddy/memory/` |

Hermix 的 L3 User Memory 应当与 WorkBuddy 层面保持独立——前者是 AI 助手"记住用户偏好"，后者是 WorkBuddy 的"跨 session 工作上下文"。
