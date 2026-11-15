# DeepSeek Harness (dsh) 底座分析：可借鉴点

Archived: 2026-11-15
对照分析类文档；P0 借鉴项已落地（见下「落地状态」表），本文存档备查。

> **落地状态（2026-09-28 晚）**：P0 缓存稳定性已完成并带回归测试。
>
> | 改动 | 落点 |
> |---|---|
> | 时间戳会话内恒定 | `AgentConfig.session_started_at`（per-request config 组装时写一次，local_agent_service ×2）；`SystemPromptBuilder::with_timestamp_at` 新增；`build_dynamic_suffix` 改用它。**两个回归测试**（`dynamic_suffix_is_stable_within_a_session` / `..._tracks_session_start_and_model`） |
> | workbench 实时样本移出系统提示 | `build_workbench_data_summary` 只保留 schema + 行数 + "用 db_query 现查"提示；agent 每写一次库就会让样本变一次，原实现每 turn 摧毁缓存 |
> | 缓存可观测 | 两处 `hermix.prompt_cache` 日志新增 `prompt_fingerprint`（系统提示 FNV 指纹）：跨 turn 不变 = 前缀可缓存；每 turn 变 = 有易变内容漏进系统提示 |
> | 验证 | `cargo check --workspace` 零警告；hermix-agent 1064 测试全过 |
>
> 待做：P1 transcript replay 测试支撑；P2 关键词门控 direction（HasSkillKeyword /
> HasBatchKeyword）从中段移到消息侧；P3 Windows write-restricted token 沙箱。
> 2026-09-28 · 基于公开源码分析（developersdigest 源码走读 453K 行 + 知乎/CSDN 架构解读 +
> dsh 缓存 issue），并对hermix 自身代码做了对照验证。
> 原则：只借鉴模式，不搬架构——dsh 是 453K 行 TypeScript / 219 packages 的 Node 运行时，
> 我们是 21 crates 的 Rust 桌面应用，搬框架是负收益。

---

## 0. dsh 是什么

DeepSeek AI 2026-08 开源的 agent 运行时（GitHub `deepseek-ai/deepseek-harness`，MIT，
npm `@deepseek-ai/dsh`），对标 Claude Code / Codex。不是聊天界面，而是把「模型 + 工具 +
权限 + 会话记忆 + 任务循环」串起来的运行时层。核心设计主张"一切皆插件"（vendor 了
Cordis 4.0.0-rc.7 微内核，打了 18 个补丁），~453K 行 TS / ~219 packages。
上下文窗口 1M / 输出 256K；Standard / Code / Minimal / Creator 四种运行模式。
来源见文末。

**对我们最重要的不是它的插件架构，而是它在运行时层立的三条纪律**——以及我们对照
检查后发现**自己也有一处同类毛病**。

---

## 1. 借鉴一（高价值 · 低成本）：提示词缓存稳定性——我们已中招

### dsh 的问题

dsh 的 GitHub issue：一个会话的提示词缓存命中率从 ~97% 掉到 ~11%——每个 step
把完整消息历史重新序列化发给模型，而 DeepSeek 的提示词缓存是**自动前缀缓存，
两次请求的前部逐字节一致才命中**。任何一字节漂移 = 之后全部重算。

### 我们的验证（代码证据，非猜测）

`prompt_assembly.rs:578`：

```rust
let prompt = format!("{stable_text}{directions_text}{context_text}{suffix_text}");
```

`suffix_text = build_dynamic_suffix(...)` 的内容是**当前时间戳 + session_id**
（`prompt_assembly.rs:454-471`）——每 turn 都变，且拼在系统提示**尾部**。

请求序列是 `[system prompt][tools][messages history]`。前缀缓存命中要求从第一个
字节开始逐字节一致：时间戳在系统提示尾部，从那一个字节开始，**tools 块 + 全部
消息历史每 turn 都缓存 miss**。也就是说：满配 20-35K token 的系统提示 + 工具
schema，每 turn 全额重付费。MiniMax/DeepSeek 都有自动前缀缓存，我们把它废掉了。

第二处：directions 每 turn 重算且**内容会翻转**——think.full（首回合）→
think.short（第 2 回合起）只翻转一次还好；`format.short` 的注入条件是
`!user_message.is_empty()`、CONTEXT_BUDGET/SKILLS 等路由按关键词门控——这些
在第 N 回合可能翻转，使系统提示中段漂移，同样打碎缓存。

### 方案

1. **时间戳/session_id 移出系统提示**：挪到最后一条 user message 的尾部
   （模型仍能拿到"现在几点"），或干脆删除——agent 决策极少依赖当前分钟。
2. **directions 门控审计**：只有三类合法变化——a) 首回合→后续的一次性切换
   （代价一次 miss，可接受）；b) 工具集变化（本身就该 miss）；其余按
   `user_message` 关键词翻转的路由（HasBatchKeyword / HasSkillKeyword 等）
   改为注入到**消息侧**（追加一条 system 角色的临时消息，位于历史尾部，
   不破坏 system 前缀），而不是系统提示中段。
3. **工具列表稳定化**：KeywordRouter 的 top-N 每 turn 可能不同 → tools 块漂移
   → 整个 messages 历史缓存 miss。优先用 AlwaysInclude 白名单 + resident 集合
   固定排序；top-N 变化尽量延后（如只在会话首次确定）。
4. **加一个可观测指标**：prompt_cache 日志已在（`prompt_assembly.rs:583`+），
   补一个"系统提示哈希是否与上一 turn 一致"的字段，回归可见。

预期收益：长会话每 turn 输入成本降到 ~1/5-1/10（缓存命中的 token 按 1/10 计价），
且首 token 延迟显著下降。**这是本次分析里性价比最高的一项。**

---

## 2. 借鉴二（高价值 · 中成本）：Keyless transcript replay 测试

### dsh 的做法

`packages/test-support/llm-replay`：把**真实录制的 session log** 变成测试夹具——
从录制的 assistant chunks 派生确定性 mock model，真实 agent 子进程对着 mock 跑完
整条 loop→tools→persistence 链，再把新落盘的 log 归一化后与 fixture 做 diff。
同一个 `.jsonl` 既是输入又是期望输出；CI 不需要 API key，没有 flaky 的
LLM-as-judge。三种模式：`replay` / `record` / `refresh`。

### 为什么我们特别需要

我们这轮稳定性改造直接动了 `run_loop` 的循环结构（watchdog 布防点、工具预算），
能依赖的只有 1062 个单元测试——**没有任何一个测试覆盖"完整 run 的端到端行为
是否与改造前一致"**。parity-tests 是 Python↔Rust 的 golden fixture，不含 LLM 循环。

### 方案

1. 给 hermix-agent 加 `llm-replay` 测试支撑：mock LLM provider（按 fixture 脚本
   回放 chunk 序列，脚本耗尽即 panic——与 dsh underrun 语义一致）。
2. 录制 3-5 条真实会话（纯工具流 / 审批流 / 流式中断 / 子 agent / 压缩触发），
   脱敏后作为 fixture。
3. 断言：重放后落库的消息序列（role + 内容哈希）与 fixture 一致。
4. 跑在 CI（`ci-gate.sh` 已有钩子位）。

这个模式**依赖借鉴一成立**（日志可重建投影）——我们已有 SQLite 持久化 +
unified_steps 透传，缺的是"从落库记录重建模型可见投影"的确定性函数。
落地顺序：先做轻量版（持久化消息序列 diff），投影不变量（借鉴三）随后补。

---

## 3. 借鉴三（结构修正）：Model-visible = logged 不变量

### dsh 的做法

运行时强制不变量：**每条模型可见消息必须可从 append-only session log 重建**。
- `agent-loop/src/invariant.ts`：每次 LLM dispatch 前，断言外发消息与
  `session.deriveMessages()` 投影逐字节一致；
- `session/src/surface.ts`：append 时若事件缺少 surface marker（说明它如何投影进
  模型历史）直接抛错。

### 对我们的意义

这正是我们审计里的两个已知弱点的**根级解法**：
- S9（崩溃丢任务）：落盘点只有两处，轮间不落盘；
- unified_steps 一致性靠客户端透传，服务端不对账。

**采用不变量、不采用全量事件溯源**：全量 append-only 事件日志是对
`session_persistence` 的重写，收益集中在调试/fork/replay——现在要为它动底座
不符合性价比。轻量版落地：

1. **每轮 finalize 后增量落盘**（已在计划 P2，就是这条的子集）；
2. 在 run/run_stream 的 LLM dispatch 前加 debug 断言：`ctx.get_messages()` 里
   最后一条 user/tool 消息已包含在 session 持久化快照中（开发模式开启，
   不一致即 tracing::error + crash dump）；
3. reload 路径已有 reconcile（`subscribe_session_snapshot`），补一个"重建投影
   与 live 上下文一致性"的测试。

---

## 4. 借鉴四（P3）：Fail-closed 沙箱阶梯 + "审批缺失即拒绝"

dsh 的沙箱按调用解析 `read-only / workspace-write / danger-full-access` 三档：
Linux bubblewrap→Landlock 回退、macOS seatbelt、**Windows write-restricted token**；
请求受限模式但无可用后端 → 抛 `SANDBOX_UNAVAILABLE` **绝不裸跑**；审批服务缺失
→ 拒绝，绝不挂起。Windows 后端诚实标注"读/网络/进程可见性不受限"。

对我们的意义：
- 我们**完全没有沙箱**（`permission_profile.rs` 自认），`code_execution` 只是
  沙箱子进程。Windows-only 项目 → write-restricted token 是唯一可行路线，
  dsh 证明了它连自己都只敢标"token-limited"——诚实标注的做法直接学。
- "审批服务缺失即拒绝，绝不挂起"与我们的审批修复同源：现在 HTTP 路径审批
  兜底 600s 自动 deny，已经对齐；沙箱阶梯可作为 P3（优先级低于前两项，
  因为桌面单主人场景的威胁模型更弱）。

---

## 5. 明确不借鉴的

| 不借鉴 | 理由 |
|---|---|
| Cordis 微内核 / 一切皆插件 | 453K 行 TS + vendor fork + 18 补丁的框架层。我们的 21-crate 分层已足够模块化；把 loop/hooks 插件化对桌面单机产品没有用户可见收益，只会增加间接层。dsh 自己也被此拖累（vendored fork 维护成本） |
| 四种运行模式 UI | 我们的 DomainMode 五域自动检测（已建待接线）+ 专家手动覆盖是同一问题的更优解，无需再加一层显式模式 |
| 1M 上下文 / 模型目录 | 模型侧参数，不归 harness |
| BENCHMARK.md 三行 stub | 反面教材：dsh 零 eval 主张。我们的 transcript replay（借鉴二）就是补这个的 |

顺带验证了一个方向判断：dsh 的 Standard/Code/Minimal/Creator 四模式与我们的
DomainMode 五域检测同构——**业界头部玩家也在做"任务域自动适配、无需用户手动切"**，
阶段 5（模式接线）的方向是对的。

---

## 6. 实施顺序

| 优先级 | 项 | 规模 | 依赖 |
|---|---|---|---|
| P0 | 缓存稳定性：timestamp 出系统提示尾 + directions 门控审计 + 哈希指标 | ~1 天 | 无 |
| P1 | transcript replay 测试支撑（mock provider + record/replay/refresh） | ~2-3 天 | 无 |
| P1 | 轮间增量落盘（原 P2，即不变量的子集） | ~1-2 天 | 无 |
| P2 | dispatch 前投影一致性断言 + reload 一致性测试 | ~1 天 | 上一项 |
| P3 | Windows write-restricted token 沙箱阶梯 | ~1 周 | 无（独立 PR） |

---

## 来源

- [We Read DeepSeek Harness: What 453K Lines of Agent Runtime Actually Say](https://www.developersdigest.tech/blog/deepseek-harness-dsh-first-look)（2026-08-13，源码走读）
- [DeepSeek Harness 终于来了：开源，一切皆插件](https://cloud.tencent.com)（腾讯云，2026-08）
- [DeepSeek Harness 深度解读：Agent 竞赛已经从模型权重下移](https://zhuanlan.zhihu.com)（知乎，2026-08-17）
- [Codex vs DeepSeek Harness：两种 Agent 架构路线对比](https://blog.csdn.net)（CSDN，2026-09-06）
- [dsh 缓存命中率 issue：~97% → ~11%](https://github.com/deepseek-ai/deepseek-harness)（GitHub）
- [Cordis：DSH 底下那颗「心」](https://zicode.com)（2026-08-17）
