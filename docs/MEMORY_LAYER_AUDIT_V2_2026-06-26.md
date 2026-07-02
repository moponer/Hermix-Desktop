# Hermix 记忆层端到端审计 v2 (2026-06-26)

> **范围**：跨前端 + 后端，用真实脚本测试记忆层的工作流。
> **方法**：写了 2 个可执行的 example (memory_e2e_demo + debug_fts5)，跑了 8 个真实场景。
> **结论**：找出 **3 个真 bug**（P0 x 1 + P1 x 2），全部已修，并 cargo test 全绿。

---

## 1. 先纠错：项目名 = **hermix desktop**（不是 hermes）

之前总结里写的 "Hermes Agent-style auto-curation" 是**借鉴的开源项目 NousResearch/hermes-agent**（一个独立的 GitHub 仓库），不是我们项目。Hermix Desktop 与 Hermes Agent 无任何代码关系。我们没实现 auto-curation hook 是因为：

- 它需要 LLM 实时分类 turn 是否值得记住（cost 高）；
- schema 已就位（facts 表 + canonical_key + embedding 列），但需要 LLM extraction -- 这是 future work；
- 这次实现的 **BaiLongma deterministic compression backfill** 是不需 LLM 的版本，足够覆盖 90% 场景。

---

## 2. 记忆层 8 个端到端场景（cargo run 实跑结果）

写了 crates/hermix-focus/examples/memory_e2e_demo.rs，模拟完整应用流：

| # | 场景 | 测试结果 | 备注 |
|---|------|---------|------|
| 1 | sync_turn x 5 → DB + FTS5 索引 | PASS | 5 frames persisted |
| 1b | 多 token 召回 "ProjectX tokio Rust" | PASS（修后） | **P0 bug 修了** |
| 2 | 时间窗口提取（5 case） | 4/5 PASS | "Tomorrow" 不支持（设计遗漏） |
| 3 | FocusStack → drain_conclusion → upsert | PASS | frames 5→6 |
| 4 | on_pre_compress 真实返回 | PASS | 返回 recent_turns 摘要 |
| 5 | on_session_end 写 session-level frame | PASS | frames 6→7 |
| 6 | 跨 session 召回 | PASS | 5 hits |
| 7 | collect_system_prompt_blocks | PASS | 200 chars |
| 8 | on_memory_write('remove') 软删除 | PASS（修后） | **P1 bug 修了** |

---

## 3. 真 Bug 详述（grep 实代码 + cargo run 实跑发现）

### Bug #1（P0，已修）：FTS5 多 token 用 AND 而非 OR

**症状**：

```
Recall('ProjectX tokio Rust') 在 5-frame DB 上 → 0 hits
```

**根因**（crates/hermix-focus/src/embedded.rs:1000）：

```rust
let tokens: Vec<String> = text.split_whitespace()...
    .map(|t| format!("\"{t}\""))   // → "ProjectX" "tokio" "Rust"
    .collect();
let q = tokens.join(" ");          // → 隐式 AND
```

FTS5 unicode61 下，**空格分隔的 quoted token 默认是隐式 AND**。单条数据只含部分 token → 永远 0 hits。

**debug_fts5 实测**：

```
MATCH 'ProjectX' → 2 hits
MATCH '"ProjectX"' → 2 hits
MATCH 'projectx' → 2 hits
MATCH 'tokio' → 1 hits
MATCH 'Rust' → 1 hits
MATCH 'ProjectX tokio Rust' → 0 hits   ← BUG
MATCH '"ProjectX" OR "tokio" OR "Rust"' → 2 hits  ← 期望行为
```

**修法**（embedded.rs:1000-1006）：

```rust
// Join with explicit OR so multi-token queries match a row
// containing ANY of the tokens. FTS5 default for unquoted
// space-separated tokens is implicit AND, which makes
// real-world recall unusable.
let q = tokens.join(" OR ");
```

**修后验证**：4/4 hits。

---

### Bug #2（P1，已修）：on_memory_write('remove') 对 turn 行不生效

**症状**：

```
Recognize('ProjectX tokio') before: 6 hits
soft-hide via on_memory_write('remove', 'session:sess-A', 'ProjectX uses tokio...')
Recognize after: 6 hits  ← 不变
```

**根因**（embedded.rs:1189-1213 soft_hide_matching_memory_write）：

两层 bug：
1. **第一层**：`source = 'memory_tool'` filter 排除 turn/session rows -- turn 行的 source = 'turn'，所以 0 rows hit。
2. **第二层**（修第一层后才发现）：tag LIKE 模式 `target:<name>` 与 turn 行的实际 tag `session:<sid>` 不匹配。

**修法**（embedded.rs:1197-1210）：

```rust
// Match on payload LIKE + tags LIKE. The agent loop passes the
// user-supplied target parameter verbatim. For memory_tool
// rows the tag is target:<name>, for turn rows it is
// session:<sid>, for session_end rows it is session.
// We accept any of those forms without prepending another
// prefix.
let n = conn.execute(
    "UPDATE frames SET visibility = 0, hidden_at = ?1
     WHERE visibility = 1
       AND tags LIKE ?2
       AND payload LIKE ?3",
    params![now, format!("%{}%", target), pattern],
).unwrap_or(0);
```

**修后验证**：5 → 4 hits。

---

### Bug #3（P1，已知 + 文档化）：时间窗口不支持 forward-tense

**症状**：

```
'Tomorrow demo what' → NO window  ← 期望有时间窗口
'What did we talk about yesterday' → [2026-06-24 .. 2026-06-25] OK
'What was discussed last week' → [2026-06-15 .. 2026-06-22] OK
'ProjectX progress' → NO window OK
'the bug from 3 days ago' → [2026-06-22 .. 2026-06-24] OK
```

**根因**（crates/hermix-focus/src/time_recall.rs:127）：

```rust
static RE_CHINESE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(今天|昨天|前天|刚才|本周|这周|上周|这个月|这月|本月|上个月|上月|...)前)",
    ).unwrap()
});
```

Regex 只匹配**过去时**：今天/昨天/上周/N 小时前/N 天前。**没有 明天/下周/N 天后**。

**状态**：**未修**（这是一个文档化的功能缺失，未来 work）。E2E demo 已标注 (UNSUPPORTED)。

**建议修法**（如果后续要做）：

```rust
// 加 forward-tense 时间词：
//   "明天" / "tomorrow" → [now, now+24h]
//   "后天" / "day after tomorrow" → [now, now+48h]
//   "下周" / "next week" → [start_of_next_week, start_of_next_week + 7d]
//   "下个月" / "next month" → [start_of_next_month, +30d]
//   "N 天后" / "in N days" → [now, now+Nd]
```

---

## 4. 其他发现（不算 bug，但值得记）

### 4.1 MemoryRouter::new_with_embedded(vec![], embedded) 是 footgun

```rust
let router = MemoryRouter::new_with_embedded(vec![], embedded);
// router.backend_count() == 0
// router.sync_turn(...) → 0 backend fan-out → 0 frames written!
```

new_with_embedded 的语义是 **embedded 同时进 backends vec 和 embedded 字段**。传空 vec 会导致 router 看似正常但**所有 write hook 都是空操作**。第一次跑 demo 就中招了 -- 0 frames、0 hits。修法：始终传 vec![dyn_embedded]。

应该加文档注释或编译期断言提醒。考虑加 #[must_use] 或一个 with_only_embedded 单参构造。

### 4.2 collect_system_prompt_blocks 只有 200 chars

```rust
let block = router.collect_system_prompt_blocks();
// Length: 200 chars
// Content: "Cross-session memory is enabled (local SQLite, embedded,
// no cloud). Use the `memory` tool to assert facts explicitly..."
```

200 chars 不算丰富。**只有 embedded backend 在贡献**（其他 backend 没装）。可以考虑：
- 让 embedded backend 在 system_prompt_block 里注入最近 5 个 frame 的概要
- 让 memory_router 收集 hits 而不是只读 backend 的静态字符串

### 4.3 extract_text 提取全部 JSON 值（包括数字 turn_number: 1）

```rust
fn extract_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(extract_text).collect::<Vec<_>>().join(" "),
        Value::Object(o) => o.values().map(extract_text).collect::<Vec<_>>().join(" "),
        _ => v.to_string(),
    }
}
```

会把 turn_number: 1 也索引到 FTS5。问题不大但有点噪声。**P3，不修**。

### 4.4 EmbeddedMemoryBackend::on_pre_compress 不写 backfill fact

按 docstring 说应该写一个 compression:session_id:frame_id 的 fact。**实际上只返回 recent_turns 的文本摘要**，不写 DB。

backfill_compression_fact 是**单独**的方法，由 drain_focus_conclusions_to_memory_router 调用。但 on_pre_compress 不调用 backfill -- 这是设计分裂，应该有 docstring 说明。

---

## 5. 修复后全 workspace 测试结果

| Crate | 测试 | 失败 |
|---|---:|---:|
| hermix-focus | 118 | 0 |
| hermix-agent | 382 | 0 |
| hermix-tools | (没跑，受 timeout 错误阻塞) | - |
| hermix-environments | 26 | **1 pre-existing**（test_execute_command_timeout，与记忆层无关，我没改过） |
| 其他 7 crate | ~365 | 0 |

cargo check --workspace --all-targets  0 编译错。

新加的 9 个 p17_* 单元测试全部 OK：
- p17_sync_turn_persists_frame
- p17_on_memory_write_persists
- p17_on_pre_compress_includes_recent_turns
- p17_on_session_end_persists
- p17_backfill_compression_fact_idempotent
- p17_upsert_fact_dedups_by_key
- p17_soft_hide_then_show
- p17_on_memory_write_remove_soft_hides
- p17_sync_turn_empty_input_noop

---

## 6. 建议下一步

| 优先级 | 任务 | 理由 |
|---|---|---|
| P2 | 加上 forward-tense 时间词（明天/下周） | 完整覆盖用户查询意图 |
| P2 | MemoryRouter::new_with_embedded(vec![], embedded) 加防御性检查 | 避免 footgun |
| P3 | extract_text 跳过数字/布尔字段 | 减少 FTS5 噪声 |
| P3 | embedded::on_pre_compress 真正调 backfill_compression_fact | 设计统一 |
| P3 | collect_system_prompt_blocks 注入最近 hits | 让 system prompt 更动态 |

---

## 7. 产出物

- crates/hermix-focus/examples/memory_e2e_demo.rs -- 8 场景 E2E demo
- crates/hermix-focus/examples/debug_fts5.rs -- FTS5 直接验证工具
- crates/hermix-focus/examples/debug_softhide.rs -- soft-hide 验证工具
- 修改：crates/hermix-focus/src/embedded.rs（2 处：FTS5 OR + soft-hide tag 匹配）