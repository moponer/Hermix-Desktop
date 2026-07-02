# 团队技术能力提升方案

**版本**: 1.0  
**日期**: 2026-06-18  
**目标**: 提升团队在 Rust + Tauri + AI Agent 系统开发的技术能力

---

## 一、当前问题诊断(基于代码审查)

### 1.1 架构层面

| 问题 | 影响 | 根因 |
|------|------|------|
| 持久化三写无事务 | 重启丢失数据 | 对 SQLite 事务模型理解不足 |
| 时间戳文件名 | 磁盘垃圾 + 列表不一致 | 对"真源 vs 缓存"理解不足 |
| L1 反向链接缺失 | 无法追溯记忆来源 | 对数据建模考虑不周 |
| 向量搜索全表扫描 | 性能瓶颈 | 对向量数据库索引理解不足 |

### 1.2 流程层面

| 问题 | 影响 | 根因 |
|------|------|------|
| 无崩溃恢复测试 | 生产环境数据丢失 | 测试策略不完整 |
| 无并发测试 | 数据竞争风险 | 对异步代码测试经验不足 |
| 代码审查无清单 | 重复犯同类错误 | 无标准化流程 |
| 技术债务无追踪 | 问题被遗忘 | 无文档化机制 |

---

## 二、技术能力提升计划(3 个月)

### 阶段 1: 紧急修复 (第 1-2 周)

**目标**: 修复"重启就没了"的问题,建立基础质量保障。

#### 任务 1.1: 修复持久化原子性问题

**负责人**: Senior Developer (高级开发工程师)  
**时间**: 3-5 天  
**交付物**:
- PR-1: `save_session_atomic()` 实现
- PR-2: `flush_messages` 改成增量 UPSERT
- 测试: `atomicity_kill_between_db_and_md()`

**学习要点**:
- SQLite 事务模型 (`BEGIN IMMEDIATE` vs `BEGIN DEFERRED`)
- Rust 的 `rusqlite` 事务 API
- 崩溃恢复测试方法

#### 任务 1.2: 建立代码审查清单

**负责人**: 团队 Leader  
**时间**: 1 天  
**交付物**: `docs/CODE_REVIEW_CHECKLIST.md`

**内容**:
```markdown
## 持久化代码审查清单

### 必查项 (Blocking)
- [ ] 多写操作是否包在同一个事务里?
- [ ] 文件名是否用"业务 ID"而非"时间戳"?
- [ ] 有没有崩溃恢复测试?

### 建议项 (Non-blocking)
- [ ] "真源"(DB)和"缓存"(文件)的一致性谁维护?
- [ ] 有没有性能瓶颈?(全表扫描? N+1 查询?)
```

#### 任务 1.3: 引入 TECH_DEBT.md

**负责人**: 团队 Leader  
**时间**: 1 天  
**交付物**: `TECH_DEBT.md` (根目录)

**模板**:
```markdown
# 技术债务追踪

## 2026-06-18: 会话持久化原子性问题

**发现人**: 主人  
**根因**: `persist_session` + `save_session_log` + `save_trajectory` 三写无事务  
**修复 PR**: #XXX  
**状态**: 进行中  
**教训**: SQLite 的 `Connection::open()` 每次拿新连接,不包事务就等于没持久化
```

---

### 阶段 2: 测试覆盖率提升 (第 3-6 周)

**目标**: 建立完整的测试体系,避免回归。

#### 任务 2.1: 添加崩溃恢复测试

**负责人**: Senior Developer + 1 名团队成员  
**时间**: 1 周  
**交付物**:
- `crates/hermix-agent/tests/crash_recovery.rs`
- 至少 5 个崩溃场景测试用例

**学习要点**:
- `proptest` 随机测试框架
- `std::panic::catch_unwind` 捕获 panic
- SQLite 数据一致性检查

#### 任务 2.2: 添加并发测试

**负责人**: Senior Developer + 1 名团队成员  
**时间**: 1 周  
**交付物**:
- `crates/hermix-agent/tests/concurrency.rs`
- 至少 3 个并发场景测试用例

**学习要点**:
- `tokio::task::JoinSet` 并发任务管理
- Rust 数据竞争检测 (`#[test]` + `RUSTFLAGS="-Z sanitizer=thread"`)
- SQLite 并发模型 (WAL mode vs DELETE mode)

#### 任务 2.3: 引入测试覆盖率要求

**负责人**: 团队 Leader  
**时间**: 2 天  
**交付物**: 更新 `AGENTS.md`,加入测试覆盖率要求

**内容**:
```markdown
## 测试要求

- **单元测试覆盖率**: 新增代码必须 ≥80%
- **集成测试**: 关键路径必须有集成测试 (持久化、并发、崩溃恢复)
- **CI 门禁**: `cargo test --workspace --lib` 必须通过,且覆盖率不降低
```

---

### 阶段 3: 架构优化 (第 7-12 周)

**目标**: 优化 L1/L2 记忆架构,提升系统性能。

#### 任务 3.1: L1 反向链接

**负责人**: Senior Developer  
**时间**: 1 周  
**交付物**:
- DB 迁移: `ALTER TABLE frames ADD COLUMN source_session TEXT;`
- `FocusFrame` 加字段
- `frames_for_session()` API

**学习要点**:
- SQLite 迁移管理 (`rusqlite::Connection::execute_batch`)
- 数据建模: 如何设计可追溯的记忆系统

#### 任务 3.2: L1 向量搜索性能优化

**负责人**: Senior Developer + 1 名团队成员  
**时间**: 2 周  
**交付物**:
- 引入 `usearch` Rust crate (HNSW 索引)
- 替换当前的"dim-indexed scan"方案

**学习要点**:
- 向量数据库索引原理 (HNSW, IVF, ...)
- `usearch` Rust API
- 性能基准测试 (`criterion` crate)

#### 任务 3.3: 统一记忆写入路径

**负责人**: Senior Developer  
**时间**: 2 周  
**交付物**:
- `MemoryWriteBatch` 结构体
- `MemoryCoordinator` 单例
- 改 `AgentLoop` 使用新的写入路径

**学习要点**:
- Rust 单例模式 (`once_cell::sync::Lazy`)
- 跨 crate 架构重构
- 事务边界设计

---

## 三、知识分享机制

### 3.1 每周技术分享 (1 小时/周)

**形式**: 团队成员轮流分享  
**内容**:
- 本周学到的技术点
- 遇到的 bug 及根因分析
- 代码审查中发现的优秀/糟糕实践

**输出**: 分享记录存入 `docs/TECH_SHARING/` 目录

### 3.2 月度架构评审 (2 小时/月)

**形式**: 全员参与,审查当前架构  
**内容**:
- 当前架构是否满足需求?
- 有没有新的技术债务?
- 下一阶段的架构优化方向?

**输出**: 架构评审记录存入 `docs/ARCHITECTURE_REVIEWS/` 目录

### 3.3 代码审查轮岗 (每次 PR 至少 2 人审查)

**目的**: 避免"单人审查"的盲区  
**规则**:
- 至少 1 名 Senior Developer 参与审查
- 至少 1 名非原作者审查 (避免"原作者护短")

---

## 四、工具链升级

### 4.1 引入 `sqlx` CLI (可选,长期)

**当前**: 用 `rusqlite` (同步,需手动管理迁移)  
**升级**: 引入 `sqlx` (异步,支持编译时 SQL 检查 + 自动迁移)

**好处**:
- 编译时检查 SQL 语法
- 自动管理迁移 (`sqlx migrate run`)
- 更好的异步支持

**成本**: 需要重写所有 DB 访问代码 (约 2-3 周)

### 4.2 引入 `criterion` 基准测试

**目的**: 量化性能优化效果  
**交付物**:
- `crates/hermix-focus/benches/vector_search.rs`
- `crates/hermix-agent/benches/persistence.rs`

### 4.3 引入 `tokio-console` (可选)

**目的**: 调试异步代码性能瓶颈  
**交付物**: 在 `hermix-server` 中集成 `tokio-console`

---

## 五、团队角色分工建议

| 角色 | 职责 | 技能要求 |
|------|------|----------|
| **Senior Developer (高级开发工程师)** | 架构设计、代码审查、关键技术决策 | Rust 高级、异步编程、DB 设计 |
| **Backend Developer** | 实现后端功能 (Agent Loop、工具、记忆) | Rust 中级、了解 LLM API |
| **Frontend Developer** | 实现前端 UI (React + Tauri) | React、TypeScript、Tailwind |
| **QA Engineer** | 测试、崩溃恢复、性能基准 | Rust 测试、自动化测试 |
| **DevOps Engineer** | CI/CD、构建系统、发布流程 | GitHub Actions、NSIS、Windows 安装包 |

**当前状态**: 团队可能只有 1-2 人,身兼多职。  
**建议**: 优先补齐 Senior Developer 和 QA Engineer 角色(可以外包或远程)。

---

## 六、成功指标 (3 个月后)

| 指标 | 当前值 | 目标值 | 测量方法 |
|------|---------|--------|----------|
| "重启就没了" 问题 | 频繁发生 | 0 次 | 用户反馈 + 崩溃恢复测试通过率 |
| 测试覆盖率 | 未知 | ≥80% | `cargo tarpaulin` 或 `grcov` |
| 代码审查清单遵守率 | 0% | 100% | 每次 PR 检查清单完成情况 |
| 技术债务解决率 | 0% | ≥50% | `TECH_DEBT.md` 中已解决问题占比 |
| 团队技术分享次数 | 0 | ≥12 次 | `docs/TECH_SHARING/` 目录文件数 |

---

## 七、风险与应对

| 风险 | 影响 | 应对措施 |
|------|------|----------|
| 团队成员时间不足 | 计划延期 | 优先做紧急修复 (阶段 1),其他可以延后 |
| 技术能力差距太大 | 任务无法完成 | Senior Developer 提供结对编程 + 代码审查 |
| 需求变更频繁 | 架构不稳定 | 引入"架构评审"机制,重大变更需全员讨论 |

---

## 八、附录: 推荐学习资源

### Rust 高级主题

- **异步编程**: [Asynchronous Programming in Rust](https://rust-lang.github.io/async-book/)
- **测试**: [Rust Testing](https://doc.rust-lang.org/book/ch11-00-testing.html)
- **性能优化**: [The Rust Performance Book](https://nnethercote.github.io/perf-book/)

### SQLite 最佳实践

- **事务模型**: [SQLite Transaction](https://www.sqlite.org/lang_transaction.html)
- **WAL Mode**: [Write-Ahead Logging](https://www.sqlite.org/wal.html)
- **性能优化**: [SQLite Query Optimization](https://www.sqlite.org/optoverview.html)

### AI Agent 系统架构

- **LangChain**: [Agent Architecture](https://python.langchain.com/docs/modules/agents/)
- **AutoGPT**: [Agent Loop Design](https://github.com/Significant-Gravitas/AutoGPT)
- **Mem0**: [Memory Layer Design](https://github.com/mem0ai/mem0)

---

**文档结束**

**下一步**: 团队 Leader 审阅此方案,确定优先级和时间表,分配任务给团队成员。
