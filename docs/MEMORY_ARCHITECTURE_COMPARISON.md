# Hermix 记忆层 vs TencentDB Agent Memory — 架构对比与优化方向

> 文档日期: 2026-06-21
> 参考项目: https://github.com/TencentCloud/TencentDB-Agent-Memory (v0.3.6, 5.9k stars)
>
> ## 🚨 硬约束（最高优先级，主人 2026-06-21 定）
>
> **所有记忆数据必须在本地。坚决不使用云数据库或云存储记忆方式。**
>
> 含义：
> - ❌ 任何外部 SaaS embedding API（OpenAI / Cohere / DashScope 远程）
> - ❌ 云对象存储（S3 / COS / GCS）存 archive / persona / scenario
> - ❌ 集中式记忆服务（云端 MemoryRouter / 多端同步）
> - ❌ API Key + CORS 暴露记忆端点给外部调用方
> - ✅ SQLite 本地库（memory.db / sessions.db / groupchat.db）
> - ✅ 本地文件（MEMORY.md / USER.md / persona.md / sessions/<sid>/archive-*.md）
> - ✅ 本地 ONNX embedding（fastembed bge-small-en-v1.5, 384 维，零网络）
> - ✅ TencentDB **借鉴项必须做本地化改造**，云依赖项直接 N/A

---

## 1. TencentDB Agent Memory 架构概述

### 1.1 核心理念

> "Memory is not about hoarding everything in the AI — it is about sparing humans from having to repeat themselves."

TencentDB 拒绝两种极端：暴力历史累积（上下文爆满）和不可逆有损压缩（信息丢失）。核心设计是**符号化短期记忆 + 分层长期记忆**。

### 1.2 四级语义金字塔 (L0→L3)

```
┌─────────────────────────────────────────────────┐
│  L3 Persona (人格)                              │
│  用户长期偏好、习惯、目标                         │
│  存储: Markdown persona.md                      │
├─────────────────────────────────────────────────┤
│  L2 Scenario (场景块)                           │
│  可复用的解决方案模式、SOP                        │
│  存储: Markdown                                 │
├─────────────────────────────────────────────────┤
│  L1 Atom (原子事实)                             │
│  具体的日期、项目细节、关键数据                    │
│  存储: SQLite (向量 + FTS)                      │
├─────────────────────────────────────────────────┤
│  L0 Conversation (原始对话)                     │
│  完整对话记录                                    │
│  存储: SQLite (FTS)                             │
└─────────────────────────────────────────────────┘
```

**渐进式提炼流程**:
- 每 N 轮对话 → LLM 提取 L1 原子事实 (pipeline.everyNConversations=5)
- 每 N 个原子事实 → LLM 聚合为 L2 场景
- 每 N 个场景 → LLM 更新 L3 人格 (persona.triggerEveryN=50)

### 1.3 短期记忆压缩 — Mermaid Canvas

TencentDB 最大的创新是**短期上下文压缩**：

```
冗长工具日志 (数十万 token)
  │
  ├─ 1. 原始内容卸载 → refs/*.md 文件
  ├─ 2. 提取关系 → Mermaid 符号图 (带 node_id)
  ├─ 3. 轻量注入 → Agent 上下文 (几百 token)
  └─ 4. 按需回溯 → Agent 通过 node_id grep 原始内容
```

**效果**（实测数据）:
| 场景 | Token 减少 | 通过率提升 |
|------|-----------|-----------|
| WideSearch | -61.38% | +51.52% |
| SWE-bench | -33.09% | +9.93% |
| AA-LCR | -30.98% | +7.95% |
| PersonaMem | — | +59% (48%→76%) |

### 1.4 渐进式召回

```
用户查询
  │
  ├─ 先查 L3 Persona (用户偏好/习惯)
  │   └─ 够用 → 直接返回
  │
  ├─ 不够 → 下钻到 L2 Scenario (解决方案模式)
  │   └─ 够用 → 返回场景 + 支撑原子
  │
  ├─ 不够 → 下钻到 L1 Atom (具体事实)
  │   └─ 够用 → 返回原子 + 关联对话
  │
  └─ 不够 → 下钻到 L0 Conversation (原始对话)
```

### 1.5 技术实现 — 云依赖清单

| 组件 | TencentDB 实现 | 本地 first 合规性 |
|------|----------------|-------------------|
| 语言 | TypeScript (Node.js) | — |
| 存储 | SQLite + sqlite-vec | ✅ 完全本地 |
| **Embedding** | **远程 OpenAI-compatible API** | ❌ **云依赖 → 必须替换为 fastembed ONNX 本地** |
| 检索 | BM25 + 向量 + RRF 融合 | ✅ 纯本地 |
| LLM | **可配置远程**（默认 DeepSeek-V3.2） | ⚠️ 可保留（业务侧可配置本地模型，但记忆层不强制） |
| 插件 | OpenClaw plugin + Hermes Gateway adapter | — |
| 安全 | **API Key + CORS** | ❌ **云 API 鉴权 → 本地不需要，移除** |

### 1.6 本地化改造清单（TencentDB → Hermix）

| 借鉴项 | TencentDB 原始 | Hermix 本地化方案 | 状态 |
|--------|----------------|-------------------|------|
| 短期压缩 Mermaid Canvas | refs/*.md + Mermaid | `sessions/<sid>/archive-*.md` + ArchiveRefsBanner + ArchiveRefsGraph | ✅ **已落地 v2 + ArchiveRefsGraph** |
| 4 级金字塔 L0 | SQLite conversation | `sessions.db` (已有) | ✅ |
| 4 级金字塔 L1 | SQLite atoms + 向量 | `memory.db` atoms 表 + fastembed | ⏳ 待做 |
| 4 级金字塔 L2 | Markdown scenario | `<hermix_home>/scenarios/*.md` | ⏳ 待做 |
| 4 级金字塔 L3 | Markdown persona | `<hermix_home>/persona.md`（已有 MEMORY.md 邻接） | ⏳ 待做 |
| 渐进式召回 | L3→L2→L1→L0 | 修改 `MemoryRouter::recognize()` | ⏳ 待做 |
| BM25+向量+RRF | sqlite-vec + RRF | FTS5 + fastembed + RRF | ⏳ 待做 |
| 远程 embedding | OpenAI API | fastembed ONNX bge-small-en-v1.5（已有） | ✅ 替换完成 |
| API Key + CORS | SaaS 鉴权 | **删除**（本地无需） | ✅ 跳过 |
| 多端同步 / 云对象存储 | S3 / GCS | **删除**（违反硬约束） | ✅ 跳过 |

---

## 2. Hermix 记忆层架构概述

### 2.1 五层架构

```
┌─────────────────────────────────────────────────┐
│  Layer 5: MEMORY.md / USER.md (文件记忆)        │
│  人可读写, 原子写入, 威胁扫描, 漂移检测         │
│  容量: MEMORY.md 2200字符 / USER.md 1375字符     │
├─────────────────────────────────────────────────┤
│  Layer 4: Session Persistence (sessions.db)     │
│  SQLite FTS5 + 压缩摘要跨会话传递               │
├─────────────────────────────────────────────────┤
│  Layer 3: FocusStack (内存帧栈)                 │
│  5 状态: Created→Pushed→Kept→Returned→Cleared   │
│  自动从元数据生成 FocusConclusion                │
├─────────────────────────────────────────────────┤
│  Layer 2: Structured Memory (memory.db)         │
│  MemoryRouter → embedded-sqlite (主)            │
│  byterover (接 6 hook，本地 CLI)                │
│  ⚠️ 7 个 stub/孤立 plugin 已 cull (主人 2026-06-21) │
├─────────────────────────────────────────────────┤
│  Layer 1: Embeddings (向量搜索, 可选)            │
│  FastEmbedEmbeddingProvider (ONNX 本地推理)      │
│  BAAI/bge-small-en-v1.5, 384维                   │
└─────────────────────────────────────────────────┘
```

### 2.2 读路径 (正常工作)

```
用户消息 → memory_prefetch()
  ├─ TimeAwareRecall: 解析中文时间 ("昨天", "上周")
  ├─ MemoryRouter.recognize(): 扇出到 1-2 个后端
  │   ├─ EmbeddedMemoryBackend: FTS5 + cosine_topk
  │   └─ byterover (本地 CLI)
  ├─ 去重 + salience 重排序 (新近×频率×置顶)
  ├─ 读取 MEMORY.md (最高优先级)
  └─ 构建 "## Memory" 块 (12,000 字符预算)
```

### 2.3 写路径 (T1-A 设计选择)

| 写入路径 | 状态 | 说明 |
|----------|------|------|
| MEMORY.md (LLM memory tool) | ✅ 原子写入 + 威胁扫描 | 唯一落地路径 |
| FocusStack conclusion | ✅ 自动持久化 | 元数据级 |
| Session compressed_summary | ✅ 跨会话 | sessions.db |
| **archive v2** (2026-06-21 新增) | ✅ **短期上下文卸载** | `sessions/<sid>/archive-*.md` + jsonl index |
| P0 memory auto_archive | ✅ **80% 阈值触发** | `hermix-tools/src/backends/memory.rs` |
| Turn sync / 7 个 write-path hooks | ⚠️ T1-A 迁移后空 | **设计选择，非 bug**（主人定） |

> **设计决定**：T1-A 迁移后，`pm.memory_providers()` 返回空是预期的——7 个 stub/孤立 plugin 已全部删除（hindsight/retaindb/openviking/holographic/honcho/mem0/supermemory），仅保留 byterover 接线 `impl MemoryProviderPlugin` 真接 6 hook。
>
> 来源：`MEMORY.md` "记忆架构铁律"+ "主人决策日志 2026-06-21 删除重写 = 真的删"。

### 2.4 关键技术特性

| 特性 | 实现 |
|------|------|
| 语言 | Rust (21 crates, ~146K 行) |
| 存储 | SQLite (memory.db + sessions.db + groupchat.db) + 文件 |
| Embedding | fastembed ONNX 本地推理（零网络） |
| 检索 | FTS5 + cosine + salience 三元组评分 |
| 时间感知 | 中文/英文时间表达式解析 (TimeAwareRecall) |
| 多后端 | embedded-sqlite (主) + byterover (本地 CLI) |
| **短期压缩** | **archive v2** (refs + node_id 锚点 + 前端 ArchiveRefsBanner) |
| **P0 自动归档** | **80% 阈值触发 archive.md + jsonl 索引轮转** |
| 帧栈 | FocusStack 5 状态生命周期 + 自动结论 |
| 安全 | 威胁扫描 + 原子写入 + 漂移检测 + 审批引擎 |

---

## 3. 逐维度对比

### 3.1 短期记忆压缩

| 维度 | TencentDB | Hermix v2 (2026-06-21) |
|------|-----------|------------------------|
| **机制** | Mermaid 符号图 + 工具日志卸载 | archive-*.md 卸载 + ArchiveRef (node_id 锚点) |
| **存储** | refs/*.md（云端可达） | `<hermix_home>/sessions/<sid>/archive-NNN.md`（本地） |
| **索引** | Mermaid 图内嵌 | `archive-index.jsonl`（按行追加，可 grep） |
| **Token 节省** | 实测 33-61% | 待量化（参考 TencentDB 数据） |
| **可回溯性** | ✅ node_id → grep 原始内容 | ✅ ArchiveRef.node_id → Read archive file |
| **人可读性** | ✅ Mermaid 人类可读 | ✅ Markdown 原始内容 + 前端 ArchiveRefsBanner |
| **自动触发** | ✅ 基于 context window 比例 | ✅ P0 80% 预算阈值 + auto_compress 之前 archive |
| **前端可见** | ❌（CLI only） | ✅ **ArchiveRefsBanner**（折叠/展开 + 复制路径） |
| **本地 first** | ⚠️ refs 路径可走云 | ✅ **强制本地 hermix_home** |
| **评级** | ⭐⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ |

**结论**：Hermix v2 在本地 first 维度反超 TencentDB。前端可见性是独有优势。

### 3.2 长期记忆分层

| 维度 | TencentDB | Hermix 当前 |
|------|-----------|-------------|
| **层级数** | 4 (L0→L3) | 3（MEMORY.md / FocusConclusion / sessions.db） |
| **提炼过程** | LLM 驱动，每 N 轮自动提炼 | 元数据级自动结论，非语义 |
| **人格建模** | L3 Persona (用户画像) | ❌ 无等价机制（但 MEMORY.md 承担部分功能） |
| **场景识别** | L2 Scenario (可复用模式) | ⚠️ FocusConclusion 含标签但无语义 |
| **溯源能力** | Persona→Scenario→Atom→Conversation | ⚠️ 结论无下钻路径 |
| **存储格式** | L2/L3 用 Markdown（人可读） | MEMORY.md (人可读) + FocusConclusion JSON |
| **本地 first** | ⚠️ 远程 LLM 提炼 | ✅ 完全本地（待 LLM 提炼接入本地模型） |
| **评级** | ⭐⭐⭐⭐ | ⭐⭐⭐ |

**差距分析**：Hermix 缺少 L2/L3 语义提炼层。但若引入，**必须用本地 LLM**（主人铁律），不能走 OpenAI API 提炼。优先复用现有 fastembed embedding 做聚类，必要时再考虑本地 7B 模型蒸馏。

### 3.3 召回策略

| 维度 | TencentDB | Hermix |
|------|-----------|--------|
| **检索方式** | BM25 + 向量 + RRF 融合 | FTS5 + cosine + salience |
| **层级感知** | ✅ 渐进式 L3→L2→L1→L0 | ❌ 扁平搜索（Phase 4 待做） |
| **时间感知** | ❌ 无 | ✅ 中文/英文时间表达式解析（独有） |
| **显著性** | 仅向量相似度 | ✅ 三元组 (新近×频率×置顶) |
| **多后端融合** | ❌ 单一 SQLite | ✅ MemoryRouter 扇出去重（缩减到 1-2 后端） |
| **本地 first** | ⚠️ 远程 embedding | ✅ fastembed 本地 |
| **评级** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐ |

**各有优势**：TencentDB 的 RRF 融合可借鉴（修改 `MemoryRouter::recognize()`），Hermix 的时间感知和显著性是独有优势。

### 3.4 Token 优化

| 维度 | TencentDB | Hermix v2 |
|------|-----------|-----------|
| **实测数据** | ✅ SWE-bench -33%, WideSearch -61% | ⏳ 待量化（架构等价 TencentDB） |
| **策略** | 符号化压缩 + 渐进式召回 | archive 卸载 + 80% 阈值 + LLM 摘要 |
| **可配置** | ✅ 3 级参数 (日常/高级/完整) | ⚠️ 硬编码预算（待 `archive_threshold` 可配置） |
| **本地 first** | ⚠️ 远程 LLM 摘要 | ✅ 本地 LLM（待接入） |
| **评级** | ⭐⭐⭐⭐ | ⭐⭐⭐ |

### 3.5 安全性

| 维度 | TencentDB | Hermix |
|------|-----------|--------|
| **写入安全** | ⚠️ 基本 | ✅ 原子写入 + 威胁扫描 + 漂移检测 |
| **API 安全** | ⚠️ **API Key + CORS**（云鉴权） | ✅ 审批引擎 + 风险分级（**本地进程内**） |
| **数据隔离** | ✅ session_key 隔离 | ✅ 独立 session_id |
| **离线能力** | ⚠️ 需 API Key 联网 | ✅ **完全离线** |
| **评级** | ⭐⭐ | ⭐⭐⭐⭐⭐ |

**结论**：本地 first 让 Hermix 在安全维度反超 TencentDB 1 颗星（无需云鉴权 = 无云泄漏风险）。

### 3.6 离线能力

| 维度 | TencentDB | Hermix |
|------|-----------|--------|
| **本地存储** | ✅ SQLite + sqlite-vec | ✅ SQLite + 文件 |
| **本地 Embedding** | ❌ 依赖远程 API | ✅ fastembed ONNX 本地推理 |
| **零 API 依赖** | ⚠️ Embedding 需 API | ✅ **完全可离线** |
| **本地 first 合规** | ❌ 违规 | ✅ **完全合规** |
| **评级** | ⭐⭐⭐ | ⭐⭐⭐⭐⭐ |

### 3.7 白盒可调试性

| 维度 | TencentDB | Hermix |
|------|-----------|--------|
| **人可读存储** | ✅ L2/L3 Markdown | ✅ MEMORY.md + archive-*.md |
| **层级间溯源** | ✅ Persona→Scenario→Atom→Conversation | ⚠️ MEMORY.md ↔ sessions.db 双向（弱溯源） |
| **文件系统检查** | ✅ `~/.openclaw/memory-tdai/` | ✅ `~/.hermix/sessions/<sid>/archive-*.md` + `MEMORY.md` |
| **前端可见** | ❌ | ✅ **ArchiveRefsBanner**（独有） |
| **本地 first 合规** | ✅ 文件本地 | ✅ 文件本地 |
| **评级** | ⭐⭐⭐⭐ | ⭐⭐⭐⭐⭐ |

---

## 4. Hermix 独有优势 (TencentDB 不具备 / 本地化反超)

| 特性 | 描述 | 价值 |
|------|------|------|
| **FocusStack 帧栈** | 5 状态生命周期，自动结论生成 | 任务级记忆管理，子代理委托追踪 |
| **时间感知召回** | 解析中文/英文时间表达式，自动附加时间窗口过滤 | "昨天讨论的那个 bug" 能精确召回 |
| **显著性评分** | 三元组：新近(0.5) × 频率(0.3) × 置顶(0.2) | 常引用的内容自然排前面 |
| **文件记忆 MEMORY.md** | 人可读写 + 原子写入 + 威胁扫描 + 漂移检测 + 冻结快照 | 用户可直接编辑记忆文件 |
| **本地 Embedding** | fastembed ONNX 本地推理，零网络依赖 | 完全离线工作 |
| **跨会话摘要** | SessionPersistence.compressed_summary | 会话重启时恢复上下文 |
| **tool_search 桥接** | 工具数 > 25 时自动渐进式披露 | 控制 LLM 工具列表大小 |
| **⭐ 本地 first 强约束** | 全栈本地化，云依赖零容忍 | 无云泄漏 / 无 API Key 管理负担 |
| **⭐ 前端 ArchiveRefsBanner** | archive 引用可视化（折叠/展开/复制路径） | 用户可直接看到被卸载的内容 |
| **⭐ P0 自动归档** | 80% 阈值触发 archive.md + jsonl 索引轮转 | 无需 LLM 即可自动减负 |

---

## 5. 优化路线（已重排，本地 first 优先）

### ✅ Phase 0: 死代码 cull（已完成 2026-06-21）

**动作**：删除 7 个 stub/孤立 plugin，保留 1 个真接 hook 的。

| 状态 | Plugin | 原因 |
|------|--------|------|
| ✅ 保留 | `byterover` | 本地 CLI，接线 `impl MemoryProviderPlugin` 真连 6 hook |
| ❌ 删除 | `hindsight` | stub 永不返回真数据 |
| ❌ 删除 | `retaindb` | stub 永不返回真数据 |
| ❌ 删除 | `openviking` | stub 永不返回真数据 |
| ❌ 删除 | `holographic` | stub 永不返回真数据 |
| ❌ 删除 | `honcho` | stub 永不返回真数据 |
| ❌ 删除 | `mem0` | stub 永不返回真数据 |
| ❌ 删除 | `supermemory` | stub 永不返回真数据 |

**主人原话**：*"5 个里 4 个不写，写了的那个人是有本地 CLI 的 byterover。不写的就删除掉啊"*

### ✅ Phase 1: 写路径 T1-A 迁移（已完成 2026-06-21）

**结论**：T1-A 迁移后 `pm.memory_providers()` 返回空是预期行为（cull 完只剩 byterover，且按需 lazy load）。

**不需要修复**，是主人钦定的设计选择。

### ✅ Phase 2: 短期工具输出压缩 v2（已完成 2026-06-21）

**实现**：`crates/hermix-agent/src/archive.rs` (311 行 + 7 测试 + 3 session-index 测试)

```rust
pub struct ArchiveRef {
    pub node_id: String,           // 锚点
    pub file_path: PathBuf,        // archive-NNN.md 路径
    pub line_range: (usize, usize),// 段落行号
    pub char_count: usize,
}

pub fn archive_context_section(
    messages: &[Message],
    hermix_home: &Path,
    now: SystemTime,
    session_id: &str,
) -> Result<Option<ArchiveRef>>;

pub fn read_session_index(
    hermix_home: &Path,
    session_id: &str,
) -> Result<Vec<ArchiveRef>>;

pub fn append_session_index(
    hermix_home: &Path,
    session_id: &str,
    refs: &[ArchiveRef],
) -> Result<()>;
```

**触发链**：
```
auto_compress_if_over_threshold
  └─ archive_context_section (写入 archive-NNN.md)
  └─ append_session_index (写入 archive-index.jsonl)
  └─ AgentLoop.last_archive_refs = Mutex<Vec<ArchiveRef>>
```

**前端对接**（5 文件，2026-06-21 完成）：
- `crates/hermix-server/src/lib.rs`: `GET /v1/sessions/{id}/archive-refs`
- `apps/desktop-tauri/src/api.ts`: `getSessionArchiveRefs(id) -> Promise<ArchiveRef[]>`
- `apps/desktop-tauri/src/components/ChatView.tsx`: `ArchiveRefsBanner` 折叠/展开/复制
- `apps/desktop-tauri/src-tauri/src/sidecar.rs`: `.env("HERMIX_SERVER_ADDR", ...)` 显式注入
- `apps/desktop-tauri/src/App.tsx`: agents 页 onAfterHire 补 setPage("chat")

**验证**：
- `cargo check --workspace --all-targets` → 0 error
- `cargo test -p hermix-agent --lib` → 369 passed; 0 failed
- `cargo test -p hermix-tools --lib backends::memory` → 16 passed; 0 failed
- `tsc --noEmit -p apps/desktop-tauri` → 0 error

### ✅ Phase 3: P0 memory auto_archive（已完成 2026-06-21）

**实现**：`crates/hermix-tools/src/backends/memory.rs::try_auto_archive(target, target_kept_size)`

- 触发：MEMORY.md 字符数 ≥ 80% 阈值
- 动作：将超额内容写入 `<hermix_home>/memory/archive-<ts>.md`（rotation）
- 返回：`MemoryWriteResult { archived_to: Option<PathBuf>, ... }`
- parse_entries 按行遍历 + drop `ARCHIVE_INDEX_PREFIX` 行

**测试**：8 个测试覆盖（新增/轮转/解析/索引行过滤/边界/不重复归档/保持最新优先）

### ✅ Phase 4: ArchiveRefsGraph 前端可视化（**已落地 2026-06-21**，手写 SVG）

**借鉴**：TencentDB Mermaid 关系图 → **本地化 = 手写 SVG timeline，不引入 mermaid 库**

**落地结构**（`apps/desktop-tauri/src/components/ArchiveRefsGraph.tsx`，~190 行）：
1. 水平 SVG 时间线，每个 ArchiveRef = 一个 circle，半径 ∝ √(charCount)
2. user/assistant 交替上下排列（refs push 顺序保证），基线 = 虚线时间轴
3. Hover tooltip：role 箭头（↑ user / ↓ assistant / • other）+ node_id + 行号
4. Click node → 复制 file_path + anchor（与 list 模式同行为，bundle 复用 handleCopy）
5. **单一颜色 + alpha 透明度变化**（主人视觉铁律：渐变色禁止 ✅）
6. **不引入 mermaid 依赖**（主人 cross-project 偏好：bundle 越小越好 ✅）

**ChatView 接入**：ArchiveRefsBanner 标题栏右侧新增 **列表 / 图谱** 切换 tab，
onClick stopPropagation 避免触发外层 onToggle。viewMode state 互斥，二选一。

**验收**：
- `tsc --noEmit -p apps/desktop-tauri` → ✅ 0 error
- 无新 npm 依赖（package.json unchanged）
- 视觉与现有 ArchiveRefsBanner 一致（共用 var(--color-border) / accent / success）

**价值**：从 ⭐⭐⭐⭐⭐ 升级到 ⭐⭐⭐⭐⭐+（可视化档案 + 节点图导航）
**对比 TencentDB**：同样提供节点视图，但数据 100% 本地（archive-index.jsonl），
不依赖外部 refs 服务化方案。

### 🟡 Phase 5: 长期记忆 L1/L2/L3 提炼（待做，2-3 周）

**借鉴**：TencentDB 4 级金字塔

**本地化方案**（**强制本地 LLM**）：
```
Layer 0: sessions.db（已有）
  │ 每 5 轮
  ▼
Layer 1: memory.db atoms 表（新增）
  │ LLM 提取: 用户偏好、关键决策、项目约束
  │ Embedding: fastembed bge-small-en-v1.5
  │ 存储: {id, session_id, content, embedding, created_at}
  │
  │ 每 10 个原子
  ▼
Layer 2: <hermix_home>/scenarios/*.md（新增）
  │ LLM 聚合: 将相关原子合并为场景
  │ Embedding: fastembed（同上）
  │
  │ 每 5 个场景
  ▼
Layer 3: <hermix_home>/persona.md（新增，邻接 MEMORY.md）
  │ LLM 更新: 用户画像、偏好、习惯
  │
  召回: L3 → L2 → L1 → L0 (渐进式)
```

**本地 first 约束**：
- LLM 提炼必须用本地模型（如 llama.cpp / Ollama 本地服务）
- 不允许任何云 LLM API 提炼记忆层数据
- 若本地 LLM 不可用 → 退化到 MEMORY.md 单层（不强制 4 级）

### 🟢 Phase 6: RRF 融合检索（待做，3 天）

**借鉴**：TencentDB BM25 + 向量 + RRF

**本地化方案**：
- BM25：SQLite FTS5（已有）
- 向量：fastembed cosine（已有）
- RRF：`ReciprocalRankFusion::fuse(bm25_hits, vector_hits, k=60)`

### ⚪ Phase 7: archive_threshold 可配置（待做，1 天）

**动作**：将硬编码 80% 提取到 `config.yaml` 的 `memory.archive_threshold` 字段，默认 0.8。

---

## 6. 优先级排序（本地 first 重排）

| 优先级 | 工作 | 状态 | 本地 first | 工期 | 影响 |
|--------|------|------|-----------|------|------|
| ✅ P0 | 死代码 cull（7 plugin → 1） | 已完成 | ✅ | 1 天 | 代码清理 |
| ✅ P0 | 写路径 T1-A 迁移 | 已完成 | ✅ | 已迁移 | 设计收口 |
| ✅ P1 | archive v2 后端 | 已完成 | ✅ | 1 天 | 短期压缩 |
| ✅ P1 | archive 前端对接 | 已完成 | ✅ | 1 天 | 用户可见 |
| ✅ P1 | P0 memory auto_archive | 已完成 | ✅ | 1 天 | 长期防爆 |
| 🟡 P2 | Mermaid Canvas 前端（手写 SVG） | ✅ 已做 | ✅ | 已落地 | 可视化升级 |
| 🟡 P2 | L1 原子提炼 | 待做 | ⚠️ 需本地 LLM | 1 周 | 跨会话 |
| 🟡 P3 | L2 场景提炼 | 待做 | ⚠️ 需本地 LLM | 1 周 | 跨会话 |
| 🟡 P3 | L3 人格提炼 | 待做 | ⚠️ 需本地 LLM | 1 周 | 用户画像 |
| 🟡 P3 | RRF 融合检索 | 待做 | ✅ | 3 天 | 召回精度 |
| 🟡 P3 | 渐进式召回 | 待做 | ✅ | 3 天 | 召回效率 |
| ⚪ P4 | archive_threshold 可配置 | 待做 | ✅ | 1 天 | 可调优 |
| ❌ ~~P4~~ | ~~远程 embedding API~~ | **不做** | ❌ 违反硬约束 | — | — |
| ❌ ~~P5~~ | ~~API Key + CORS 暴露端点~~ | **不做** | ❌ 违反硬约束 | — | — |
| ❌ ~~P5~~ | ~~S3 / 云对象存储 archive~~ | **不做** | ❌ 违反硬约束 | — | — |
| ❌ ~~P5~~ | ~~多端云同步 persona~~ | **不做** | ❌ 违反硬约束 | — | — |

---

## 7. 本地 first 反超项总结

| 维度 | TencentDB | Hermix | 反超原因 |
|------|-----------|--------|----------|
| 离线能力 | ⚠️ 需远程 embedding | ✅ 零网络 | fastembed ONNX |
| 安全性 | ⚠️ API Key + CORS | ✅ 本地进程内 | 无云鉴权 = 无云泄漏 |
| 白盒可调试 | ⚠️ CLI only | ✅ 前端 Banner | ArchiveRefsBanner |
| 自动归档 | ⚠️ 需 LLM | ✅ 纯字符阈值 | P0 memory auto_archive |
| 数据主权 | ⚠️ 云端可达 | ✅ 100% 本地 | 硬约束落地 |

---

## 8. 参考资源

### TencentDB Agent Memory
- GitHub: https://github.com/TencentCloud/TencentDB-Agent-Memory
- README: https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/main/README.md
- 中文文档: https://github.com/TencentCloud/TencentDB-Agent-Memory/blob/main/README_CN.md
- 核心源码: `src/core/` (conversation/, hooks/, persona/, profile/, scene/, store/, tools/)
- Gateway 适配器: `src/gateway/` + `src/adapters/`
- Hermes 插件: `hermes-plugin/memory/memory_tencentdb/`
- 配置 Schema: `openclaw.plugin.json`
- 版本: v0.3.6 (2026-05-28)
- 许可: MIT
- Stars: 5.9k, Forks: 515

### 本项目记忆层相关文件

**v2 archive 实现**：
- `crates/hermix-agent/src/archive.rs` — ArchiveRef + archive_context_section + read_session_index + append_session_index
- `crates/hermix-agent/src/agent_loop.rs` — last_archive_refs 字段 + auto_compress_if_over_threshold 调 archive
- `crates/hermix-server/src/lib.rs` — `GET /v1/sessions/{id}/archive-refs` HTTP endpoint

**P0 memory auto_archive**：
- `crates/hermix-tools/src/backends/memory.rs` — try_auto_archive + parse_entries

**前端对接**：
- `apps/desktop-tauri/src/api.ts` — getSessionArchiveRefs
- `apps/desktop-tauri/src/components/ChatView.tsx` — ArchiveRefsBanner
- `apps/desktop-tauri/src-tauri/src/sidecar.rs` — HERMIX_SERVER_ADDR env 注入

**记忆层核心**：
- `crates/hermix-focus/src/memory_layer.rs` — MemoryLayer trait
- `crates/hermix-focus/src/memory_router.rs` — MemoryRouter
- `crates/hermix-focus/src/embedded.rs` — SQLite 后端
- `crates/hermix-focus/src/focus_stack.rs` — FocusStack
- `crates/hermix-focus/src/salience.rs` — 显著性评分
- `crates/hermix-focus/src/time_aware_recall.rs` — 时间感知召回
- `crates/hermix-agent/src/memory_plugins/byterover.rs` — 唯一保留的外部后端
- `crates/hermix-agent/src/memory_layer_adapter.rs` — 路由器构造
- `crates/hermix-tools/src/tools/memory.rs` — LLM 面向的 memory tool
- `crates/hermix-intelligence/src/embedding.rs` — FastEmbedEmbeddingProvider

### 架构对比文档
- `docs/ARCHITECTURE_BENCHMARK.md` — 架构基准对比
- `docs/ARCHITECTURE_REFACTOR_PLAN.md` — 架构重构计划
- `docs/MIGRATION_TODO.md` — 迁移待办
- `.workbuddy/memory/MEMORY.md` — 本项目长期记忆（含 T1-A 设计选择 + 死代码 cull 决策）

---

## 附录 A: 主人决策日志（影响本文档）

### 2026-06-21 "做好自己，不要去感知外部"
> 永久不做：跨平台 / macOS+Linux / 多平台社交分发 / 资源自动感知
> **不做云端记忆服务、不做远程 LLM 提炼记忆层（除非本地模型不可用时的 fallback）**

### 2026-06-21 "坚决不使用云数据库或云存储记忆方式"
> 所有记忆数据均在本地
> → 本文档 §1.5/§1.6/§6 直接剔除腾讯云依赖项

### 2026-06-21 "5 个里 4 个不写，写了的那个人是有本地 CLI 的 byterover"
> 不写的就删除掉啊
> → 7 个 stub/孤立 plugin 全删（Phase 0 ✅）

### 2026-06-21 "删除重写 = 真的删"
> 不留兜底 / 不留 commented-out / 不留 try/catch 回退
> → grep 复查 0 残留

### 2026-06-20 "完整实现铁律"
> 不能跑的东西不要写，写了就要跑通；不能跑就删掉
> → P3.2 rank badge 反面案例：adjust_member_score trait 写了 0 调用方 → 接上 update_subtask(Done) +10 / Failed -15