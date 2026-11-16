# 等待 / 超时清单 · 补充件（crates 侧未核命中回源核对）

本文件是 `docs/wait-timeout-audit.md` 第四节「未完成项 / 未核对的命中」的补充件，只收录 Rust crates 侧命中。
主清单正文未改。前端侧命中（useApprovalBridge / usePolling / useCustomApps / rest.ts / cdp_probe / shutdown / screenshot_controller）不在本件范围。
`apps/desktop-tauri/src-tauri/src/commands.rs:723` / `:812` 已由阻塞维度清单收录，本件不重复收录。

结论口径：
- **成立**：该处确实构成「等很久」的等待点，且已核出实际数值。
- **不成立**：该处有明确上限且短，或不产生用户可见等待。
- **与等待无关**：命中处是逻辑判断/常量定义，不构成等待点。
- 每条都标注等待机制（默认超时值 / 轮询间隔 / 重试退避 / 无上限等待）、触发条件、界面表现。
- 界面影响分两类：**卡死型**（≥30s 无任何反馈，用户以为界面死了）与**只是慢**（有上限且期间有反馈）。

---

## 一、hermix-agent：重试退避与工具预算（第四节前 3 组）

### A1. LLM 重试退避：3 次重试 / 4 次尝试，退避封顶 30s，Retry-After 单次封顶 300s

| 项 | 内容 |
|---|---|
| 条目号 | A1 |
| 结论 | 成立 |
| 路径:行号 | `crates/hermix-agent/src/llm_retry.rs:294`（delay 计算）、`:297`（jittered_backoff 调用）、`:310-321`（emit_status + `sleep(delay).await`）；配置默认值 `crates/hermix-agent/src/agent_config.rs:48-56`；退避算法 `crates/hermix-agent/src/agent_helpers.rs:976-982`；Retry-After 解析 `crates/hermix-agent/src/agent_helpers.rs:637-651` |
| 等待机制 | 重试退避（叠加单次请求默认超时值） |
| 触发条件 | LLM 返回错误被分类为 `ErrorClass::RateLimit` 或 `ErrorClass::Retryable`（`llm_retry.rs:248`）；`Auth` 分类会先刷 OAuth token 再重跑，`Fatal`/`Quota`/`ContextOverflow` 直接返回不重试（`llm_retry.rs:213-250`） |
| 界面表现 | 两次尝试之间有状态提示「LLM API retry in {ms}ms (attempt n/3)」（`llm_retry.rs:310-319`）；但单次尝试内最长 180s 无任何输出 |

核出的具体数值：
- `max_retries` 默认 **3**（`agent_config.rs:48-50`），循环写法 `for attempt in 0..=effective_max_retries`（`llm_retry.rs:162`）→ **最多 4 次请求尝试**。
- 退避基数 `base_delay_ms` 默认 **1000ms**，上限 `max_delay_ms` 默认 **30000ms**（`agent_config.rs:51-56`）。
- `jittered_backoff(attempt, base, max)`（`agent_helpers.rs:976-982`）：`exp = base × 2^min(attempt,10)`，封顶 30s，抖动为封顶值的 ±12.5%。默认参数下实际 sleep 区间：
  - attempt 0 → **[875ms, 1125ms)**
  - attempt 1 → **[1750ms, 2250ms)**
  - attempt 2 → **[3500ms, 4500ms)**
  - attempt ≥5 → **[22500ms, 30000ms)**（封顶后不再增长）
- `max_retries=3` 时最多 sleep 3 次，本地退避路径总 sleep **最多约 7.9s**（1125+2250+4500）。
- provider 自带 `Retry-After` **覆盖**本地退避（`llm_retry.rs:294-301`），解析上限 `CAP_MS = 300_000`（`agent_helpers.rs:638`），即单次 sleep 最多 **300s**，不受 `max_delay_ms=30s` 约束。Retry-After 路径 3 次 sleep 最多 **900s**。
- 单次请求总耗时上限 **180s**（`crates/hermix-agent/src/provider.rs:41`，主清单条目 10 已核）。
- **单次请求总耗时上限（整轮重试）**：本地退避路径 ≈ 4 × 180s + 7.9s ≈ **728s**；Retry-After 路径 ≈ 4 × 180s + 900s ≈ **1620s**。

界面影响：卡死型。每次尝试之间有状态行提示，但单次尝试内 180s 无任何模型输出（≥30s 无反馈），Retry-After 路径整轮最长约 27 分钟无输出。

```rust
let delay = parse_retry_after_ms(&err_str)
    .map(Duration::from_millis)
    .unwrap_or_else(|| {
        jittered_backoff(
            attempt,
            effective_base_delay_ms,
            retry.max_delay_ms,
        )
    });
```

### A2. 会话落盘 SQLITE_BUSY 重试：15 次重试 / 16 次尝试 / 总预算 2000ms

| 项 | 内容 |
|---|---|
| 条目号 | A2 |
| 结论 | 成立（重试退避，总预算 2s，属「只是慢」） |
| 路径:行号 | `crates/hermix-agent/src/session_persistence/sessions.rs:259`（`std::thread::sleep(sleep)`）；循环 `:233`；预算判定 `:232`、`:245-250`；常量 `crates/hermix-agent/src/session_persistence/mod.rs:119-125` |
| 等待机制 | 重试退避（带总预算上限） |
| 触发条件 | 落盘返回 SQLITE_BUSY 且 `is_busy_error(&e)` 为真（`sessions.rs:242`） |
| 界面表现 | 会话写入线程被阻塞最多 2s；对话区不冻结，界面无提示 |

核出的具体数值：
- `WRITE_MAX_RETRIES = 15`（`mod.rs:119`），循环 `for attempt in 0..=WRITE_MAX_RETRIES` → **最多 16 次尝试**。
- 每次 sleep 抖动区间 **20–150ms**（`WRITE_RETRY_MIN_MS=20`、`WRITE_RETRY_MAX_MS=150`，`mod.rs:120-121`，抖动计算 `sessions.rs:252-253`）。
- **总预算** `WRITE_RETRY_BUDGET = 2000ms`（`mod.rs:125`），deadline 在 `sessions.rs:232` 设定；每次 sleep 会被 `.min(remaining)` 截断（`sessions.rs:254`），预算耗尽立即返回错误（`sessions.rs:245-250`）。
- **总预算上限 = 2s**（不再是「15 × 150ms = 2.25s」的裸次数上限，代码注释 `sessions.rs:228-230` 明确说明）。

界面影响：只是慢。2s 预算内不产生用户可见等待。

```rust
let jitter_ms = WRITE_RETRY_MIN_MS
    + (rand_jitter_u64() % (WRITE_RETRY_MAX_MS - WRITE_RETRY_MIN_MS + 1));
let sleep = std::time::Duration::from_millis(jitter_ms).min(remaining);
```

### A3. 工具单次调用超时：按工具身份 60s–600s

| 项 | 内容 |
|---|---|
| 条目号 | A3 |
| 结论 | 成立 |
| 路径:行号 | `crates/hermix-agent/src/tool_executor.rs:910`（`tokio::time::timeout(per_call_timeout, handler_fut)`）；预算取值 `:819`；预算表 `crates/hermix-tools/src/dispatch.rs:125-153` |
| 等待机制 | 默认超时值（按工具身份的差异化预算） |
| 触发条件 | 单个工具 handler 未在预算内返回（`tool_executor.rs:910` 的 timeout 分支） |
| 界面表现 | 工具卡片转为错误「per-call timeout after {n}s: tool '...' did not return in time」；最长 600s（terminal / file_converter / 图像视频类）无返回 |

核出的具体数值（`dispatch.rs:125-153`）：
- `terminal` → **600s**
- `execute_code` → **180s**
- `delegate_task` → **190s**
- `file_converter` → **600s**
- `markitdown` / `read_pdf` → **300s**
- `image_generate` / `gpt_image_generate` / `video_generate` / `tts` → **600s**
- `browser_*`（7 个）→ **180s**
- 其它全部工具 → **60s**

界面影响：卡死型（terminal / file_converter / 图像视频类单次可静默 600s）。60s 档已属 ≥30s 无反馈。

### A4. 批超时后的 drain 宽限：5s（不是等待点上限，是收尾宽限）

| 项 | 内容 |
|---|---|
| 条目号 | A4 |
| 结论 | 成立（是等待点，但是收敛宽限，不是「等很久」） |
| 路径:行号 | `crates/hermix-agent/src/tool_executor.rs:1013`（`tokio::time::timeout(left, join_set.join_next())`）；宽限常量 `:1008`；宽限 deadline `:1009` |
| 等待机制 | 默认超时值（宽限期） |
| 触发条件 | 整批预算耗尽（`remaining.is_zero()`，`:997`）后仍有槽位未回填 |
| 界面表现 | 批超时后再多等最多 5s，把在飞结果收回来；用户看到的仍是「batch-wide timeout」错误 |

核出的具体数值：`DRAIN_GRACE = 5s`（`:1008`）。drain 循环内每次 `join_next` 用 `timeout(left, …)` 兜底，`left` = 剩余宽限时间，**整段 drain 累计不超过 5s**（`:1009`、`:1011-1012`）。drain 结束后未回填槽位被填入合成错误信封（`:1064-1086`）。

界面影响：只是慢（5s 内收敛，且有 heartbeat 事件）。

### A5. 工具批总预算：各工具预算之和 + 120s，封顶 3600s

| 项 | 内容 |
|---|---|
| 条目号 | A5 |
| 结论 | 成立 |
| 路径:行号 | `crates/hermix-agent/src/tool_executor.rs:982`（`batch_budget` 调用）、`:986`（deadline）、`:1093`（批内等待处的 `timeout(remaining, join_set.join_next())`）；预算实现 `crates/hermix-tools/src/dispatch.rs:160-167` |
| 等待机制 | 默认超时值（批级总预算） |
| 触发条件 | 批内工具全部未在各自预算内返回 |
| 界面表现 | 整批工具停在运行中，最长 3600s（1 小时）无结果；期间有 5s 一次的心跳事件（`tool_executor.rs:55` `TOOL_HEARTBEAT = 5s`，`:992`、`:1099-1120`） |

核出的具体数值：`batch_budget` = 各工具 `per_tool_budget` 之和 **+120s 调度余量**，再 `min(3600s)`（`dispatch.rs:160-167`）。即 **封顶 3600s**。批内单次等待用 `timeout(remaining, join_set.join_next())`，`remaining` 来自批 deadline（`:986`、`:1093`）。

界面影响：卡死型。有 5s 心跳事件推给前端（用户能看到「仍在运行」提示），但工具结果最长 3600s 才出。

### A6. turn 转换看门狗：1800s（30 分钟）

| 项 | 内容 |
|---|---|
| 条目号 | A6 |
| 结论 | 成立 |
| 路径:行号 | `crates/hermix-agent/src/agent_config.rs:925`（`default_turn_transition_deadline_secs`），值 `:936`（`1800`），文档注释 `:922-924` |
| 等待机制 | 默认超时值 |
| 触发条件 | 一个 turn 的工具执行（含审批等待）到下一 turn 准备之间的窗口超时 |
| 界面表现 | 整轮停在工具执行/审批阶段，最长 30 分钟才被看门狗收口 |

核出的具体数值：默认 **1800s**。代码注释（`agent_config.rs:926-935`）自述该值需覆盖审批等待默认 600s（`ApprovalConfig.timeout_secs`）+ 批工具预算（和值，易超 300s），1800s 是这些上界之上的兜底网。

界面影响：卡死型（1800s ≥30s 无反馈）。注释同时说明旧的 30s 默认值会误杀合法长审批/工具等待。

---

## 二、待补（本轮尚未核完的第四节命中）

以下条目待核，本件后续轮次补齐：

- `crates/hermix-agent/src/compression/mod.rs:506-516`
- `crates/hermix-agent/src/member/runner.rs:811`
- `crates/hermix-agent/src/sub_agent_orchestrator.rs:423`
- `crates/hermix-agent/src/copilot_acp.rs:56-58`
- `crates/hermix-agent/src/memory_prefetch.rs:58-60`
- `crates/hermix-agent/src/rate_limit.rs`（Retry-After 解析）
- `crates/hermix-core/src/win_job.rs:336`
- `crates/hermix-cron/src/file_lock.rs:156`
- `crates/hermix-server/src/approval.rs:156-160`
- `crates/hermix-server/src/ws/mod.rs:573/618/685`
- `crates/hermix-filesearch/src/service.rs:518`
- `crates/hermix-plugin/src/hx_packager.rs:492`
- `crates/hermix-environments/src/local.rs:850/956/1025`
- `crates/hermix-environments/src/docker.rs:153`
- `crates/hermix-intelligence/src/auxiliary/client.rs:207`
- `crates/hermix-intelligence/src/models_dev/client.rs:115`
- `crates/hermix-intelligence/src/model_directory.rs:623`

---

## 三、本次读过的文件清单

| 文件 | 读取范围（行） |
|---|---|
| docs/wait-timeout-audit.md | 全文 |
| crates/hermix-agent/src/llm_retry.rs | 130-189, 200-339 |
| crates/hermix-agent/src/agent_config.rs | 44-61, 885-944 |
| crates/hermix-agent/src/agent_helpers.rs | 630-684, 965-1004 |
| crates/hermix-agent/src/session_persistence/sessions.rs | 225-294 |
| crates/hermix-agent/src/session_persistence/mod.rs | grep 定位 119-125（未整读，待补） |
| crates/hermix-agent/src/tool_executor.rs | 880-1119 |
| crates/hermix-tools/src/dispatch.rs | 100-189 |
