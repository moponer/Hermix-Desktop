# Decision Note: 事件溯源会话改造（批次 F-1 立项）

Status: rejected — 2026-10-04 评审：三条收益已被 hermix 现有机制摘走（`compression_summary_history` append-only / `archive-index.jsonl` 可查 / `backfill_unanswered_tool_results` 补合成），唯一真收益「fork 任意 seq」当前无需求。裁决见 [批次评审](../../proposed/process/2026-10-04-proposed-batch-review.md)。若将来真有 fork 需求，**重写而非复活**——届时压缩与 archive 的现状会被新设计重新解释一遍。

> 以下为 2026-10-01 一稿原文，按制度冻结保留（rejected 件不再编辑，仅作论证依据）。

## Decision

将会话持久化从"可变状态存储"改造为 **append-only 事件日志 + 投影派生**，对标
dsh `2026-06-11-event-sourced-sessions` / `2026-06-14-session-persistence`。
分四步，每步一个 PR、独立有价值：

1. **事件域语义**（先行，纯设计）：定义事件分类法——`session/*` = 持久事实
   （消息、tool call/result、usage）；`agent/*` = 活跃运行时信号（进度、
   流 delta）；`tools/*` = 工具管线事件。规则：回合/步骤边界只入日志、
   **删除镜像 emit**，杜绝双份真相（dsh `event-domain-semantics`）。
2. **存储层**：SessionEvent 直接落 JSONL（append-only，zstd 分帧可后置）；
   崩溃回合**闭合而非截断**，resume 时补合成 tool 结果（与已落地的 C.2
   `backfill_unanswered_tool_results` 合流，不重复造）；原子发布用
   Windows `MoveFileExW` + fsync（dsh 同款，主平台对口）。
3. **投影层**：`derive_messages()` 从日志派生 LLM 消息历史；压缩用
   surfaceOp replace **遮蔽事件**而非改写日志（sourceEventSeqs 可验证）；
   读取方缺投影状态时**显式抛错**（fail-loud，不默认空值）。
4. **不变式**："凡到达模型的必可从日志字节级重建"（`reconstructable-requests`）
   ——前缀缓存稳定性、fork、审计都是这条的推论。跨格式迁移**流式有状态**
   （dsh 教训：整件迁移曾撑爆 16GB 内存），未知事件**拒收**不静默跳过。

## Alternatives considered

- **维持现状（可变状态）** —— 否决：任意 seq fork、崩溃精确恢复、压缩
  可验证性、审计回放四件事在可变模型下永远做不了，且越晚改迁移越贵。
- **一次性大改** —— 否决：四步渐进，每步测试全绿再走下一步；第 2 步后
  系统已具备崩溃恢复收益，风险不再增长。
- **照搬 dsh microkernel + 40 包拆分** —— 否决：hermix 只取存储与投影
  设计，不引插件内核；事件词汇用 Rust enum + serde，不用 declaration merging。

## Consequences

- 放弃：可变消息数组的简单性；日志体积大于现状态（流 delta 若入日志，
  用 zstd + 采样控制——`agent/*` 信号类事件可不入持久层）。
- 代价：四步约 2-3 周；SQLite 设备注册表等非会话存储**不迁**（各自独立）。
- 风险：投影与日志分叉——以"重建不变式"测试钉死（随机日志前缀 → 投影
  → 再投影，幂等断言）。
- 收益：fork 任意 seq、崩溃回合精确恢复、provider 前缀缓存稳定（省钱）、
  压缩行为可审计。
