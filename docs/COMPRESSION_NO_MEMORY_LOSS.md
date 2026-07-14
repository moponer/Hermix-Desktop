# 压缩不丢记忆：机制说明与验证

> 关联任务：Phase 6（E1 召回回归 / E2 持久化 / E3 手动验证 / E6 默认引擎审计）
> 日期：2026-07-14

用户的核心担忧：**"思考压缩是不是真的？用户能不能感知？会不会丢记忆？"**

结论先说：**压缩是真实的，且不会丢记忆。** 下面给机制、测试、审计、手动验证四段。

---

## 1. 机制：压缩后原始对话完整保留

压缩走的是 `compression_ops::auto_compress_if_over_threshold` → 真正 LLM 压缩器（`ContextCompressor`）→ `create_compression_child_session`。

关键落库逻辑（`session_persistence.rs`）：

- 每次压缩创建一个**子会话**（`parent_session_id` 指向原会话），原始会话 `messages` 表**原封不动**（UI 路由继续用原 session id）。
- 折叠掉的轮次（`CompressionSnapshot.archived_turns`）作为**真实消息行**插入子会话的 `messages` 表（`active = 1`，保留原 role：user / assistant / tool）。
- 折叠摘要写入 `sessions.compressed_summary` 列（最新摘要快查索引），同时子会话 `system_prompt` 也存一份供 lineage 树展示。
- 摘要文本**不会**作为 `system` 消息行插入（避免被 `ensure_db` 的 `DELETE FROM messages WHERE role='system'` 清掉）。

因此"被压缩 = 移到子会话 + 写摘要"，不是"删除"。原始措辞始终可恢复。

---

## 2. E1 — 召回回归测试（已落地）

`crates/hermix-agent/src/session_persistence.rs` 测试模块：

- `compression_child_session_preserves_summary_and_archived_turns`
  - 压缩后 `load_compressed_summary(child)` 返回摘要（`contains("config")`）。
  - `load_session(child)` 返回全部归档轮次（3 条 user/assistant 内容全部可取回）。
- `aggregate_usage_counts_compressions_and_archived_turns`（同时覆盖 E4 / B5）
  - 压缩一次后 `aggregate_usage(30)` 的 `total_compactions == 1`、`total_archived_turns >= 2`。

跑法：

```bash
. ./cargo-env.sh && cargo test -p hermix-agent --lib \
  compression_child_session_preserves_summary_and_archived_turns \
  aggregate_usage_counts_compressions_and_archived_turns
```

---

## 3. E3 — 手动验证步骤

1. 开一个长会话，让上下文压力触发自动压缩（或手动 `/compact`）。
2. 前端「使用统计 → 上下文压缩」卡片应显示：
   - **压缩次数** ≥ 1
   - **保留的历史轮次** > 0
3. 打开 lineage / 会话树，找到 `[Compressed #N from ...]` 子会话，点开能看到被折叠的原始对话。
4. 让 LLM 回答一个只存在于"被压缩区段"的事实问题（例如压缩前问过的一个文件名）。若它能正确引用，说明摘要 + 归档都生效、记忆未丢。

若 `保留的历史轮次 == 0` 但 `压缩次数 > 0`：说明归档写入路径异常，先查 `create_compression_child_session` 的 archived_turns 插入是否执行（不会静默吞错，会返回 `AgentError::Io`）。

---

## 4. E6 — 默认引擎审计：NoOp **不是**默认

用户怀疑："`no_op_compression_engine` 是不是被静默设成默认了？"

**审计结论：不是。** 证据链：

1. **配置层无开关**。`crates/hermix-config/src/config.rs` 的 `CompactionConfig`（约 239–308 行）只有 `enabled`、`threshold_percent`、`summary_model` 等字段，**没有 `engine` 字段**。因此没有任何 config 能把压缩静默切成 noop。
2. **agent loop 默认走真压缩器**。`compression_ops::auto_compress_if_over_threshold` 接收 `compressor: Option<&Arc<TokioMutex<dyn CompressionEngine + Send>>>`：
   - 有压缩器 → 调用真正的 `ContextCompressor`（LLM 摘要）。
   - 无压缩器（`None`）→ 退化为 `ctx.compress()`（字符级，已标记 deprecated 的空操作兜底），**不是** `NoOpCompressionEngine`。
3. **registry 的 noop 不参与默认选择**。`CompressionEngineRegistry::with_defaults()` 只注册了 `"noop"`，但 agent loop 构造压缩器时直接 `ContextCompressor::new(...)`，不通过 registry 选默认。registry 仅用于运维显式按名切换。
4. **noop 的语义是自证清白的**。`NoOpCompressionEngine::should_compress` 永远返回 `false`，`record_outcome` 会把成功/失败分别计入 `compression_count` / `summary_errors`，所以即使被错误接入，dashboard 也能用指标暴露它。

**结论**：默认行为 = 真 LLM 压缩（带字符级兜底）；noop 仅在运维显式 `registry.resolve("noop")` 时才会被注入，且无任何 config 默认启用它。压缩是真的，不会因 noop 静默失效。

---

## 5. 统计界面如何呈现（Phase 3）

- **Totals 第 5 卡「缓存命中 Token」**：`totals.total_cache_read`。
- **Totals 第 6 卡「缓存命中率」**：`cache_read / (input + cache_read)` 代理指标。
- **DailyCurve 命中率折线**：按天画 `cache_read / (input + cache_read)`。
- **模型列表每行「命中率」**：同上，按模型聚合。
- **「上下文压缩」卡片（E5）**：显示 `total_compactions` 与 `total_archived_turns`，并注明"压缩以子会话保留完整历史，不丢记忆"。

> 缓存命中率分母为 `input + cache_read` 的代理口径：把 `input`（本次新写 prompt token）与 `cache_read`（缓存命中 token）之和视为"总 prompt 尝试量"。不报缓存统计的供应商（当前 MiniMax）显示 0，即"未观测到缓存命中"，如实而非伪造。
