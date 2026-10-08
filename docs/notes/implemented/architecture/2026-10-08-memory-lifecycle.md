# Decision Note: 记忆帧生命周期（turn/focus/session 衰减）+ 写入去重 + user_style latest-wins

Status: implemented — 2026-10-08 交付。

## Decision

memory.db 此前是只进不出的聊天流水库（实测 1543 帧 / 94.5% 为对话回放，同一对对话最多存 3 份）。三刀修齐，全部软删可逆：

1. **生命周期衰减**（`EmbeddedMemoryBackend::decay_stale_frames`）：三类聊天叙事帧衰减——`turn`（原始对话对）与 `focus`（每消息会话总结，与 turn 同内容类）14 天、`session`（会话尾注）30 天；`memory_tool` / `delegation` 是真记忆不衰减；置顶帧（pin_count>0）永不衰减。挂载点为每消息级 session-end 维护（`memory_on_session_end`），同时补跑 `cleanup_expired_facts`（facts TTL 此前只在 DB 打开时执行一次，长驻进程内从不生效）。长期记忆不受影响——它住在 facts（自带 TTL 梯度）/ MEMORY.md / USER.md / daily。
2. **写入去重**：`finalize_agent_result` 是每条用户消息触发的，导致 (a) 每消息的「会话总结」focus 帧逐字重复刚写入的 turn 帧（存量 867 条的来源）——现在 `memory_on_session_end` 检测到 10 分钟内已有同文本 turn 帧即跳过；(b) `on_session_end` 对同一 session 一小时内不再叠写尾注帧（模块文档一直声称有此检查，实现首次落地）。
3. **user_style latest-wins**：`user.style:{style}` 每种风格各占一行导致 4 条矛盾探测并存；每次检测写入后 `soft_hide_other_user_styles` 隐藏其余可见风格行，召回与 `most_frequent_user_style` 只见最新推断。隐藏行保留 access_count 历史（摇摆可审计）。

## Alternatives considered

- **不写 turn 帧、只留会话总结**：turn 帧是短期召回的工作马（FTS/embedding 主通道），砍掉会伤跨 session 召回；且「会话总结」本来就是同一内容的第二份副本。留 turn、去总结副本，方向反了成本更高。
- **硬删过期帧**：软删（visibility=0 + hidden_at）+ 既有 30 天后硬删的 janitor 已覆盖磁盘回收；硬删会烧掉审计 trail，无收益。
- **style 改单槽位键 `user.style:current`**：会破坏 `most_frequent_user_style` 的前缀剥离 SQL（返回 "current"）。行级互斥（隐藏兄弟行）零 SQL 改动、语义等价。
- **衰减阈值做成配置**：14/30 天先硬编码常量；没有出现第二个想调它的场景前，配置面是过度工程（YAGNI）。

## Consequences

- 帧表从只增变为「写 3 份 → 留 1-2 份 → 14/30 天后淡出」，库体积随活跃度稳态化而非无限增长。
- 14 天前的原始对话不再进入召回——需要长期保留的对话应走 `memory` 工具（晋升 facts / md，TTL 180 天/永久）或置顶帧。
- 存量一次性清理已执行（软删 445 turn/session + 805 focus + 3 style 行），DB 备份于 `memory.db.bak-2026-10-08-2`。
- `most_frequent_user_style` 语义从「历史频次最高」实际收敛为「最新可见风格」——这正是反射生成想要的输入。
