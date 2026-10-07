# Decision Note: 记忆沉淀可见化 + 记忆管理界面

Status: implemented — 已交付（2026-10-07 夜间）；用户观察"从未自动沉淀记忆"的根因分析与修复

## Problem

用户观察：实际对话中从未见到自动沉淀记忆（每日记忆 / 长期记忆）。核实代码后的根因有四个，全部有 file:line 证据：

- **RC1 写读分裂（长期记忆失效主因）**：desktop 私聊的 `memory` 工具按会话隔离写入
  `<home>/sessions/<sid>/memories/`（`agent_config.rs::resolved_memory_dir`，刻意设计），
  而唯一的管理入口（dashboard `/api/persona/memory`，`dashboard/persona.rs::memory_dir`）
  只读全局 `<home>/memories/`。新会话回退读全局（空）→ LLM 显式"记住 X"全部散落在
  会话孤岛上，**没有晋升通道**，跨会话等于失忆。
- **RC2 自动沉淀不可见**：memory.db（frames/facts/reflections/会话总结）每轮都在自动写
  （`memory_sync`、`memory_on_session_end`、reflection_generator 全部接线在主循环），
  但 hermix-server 没有任何读取 API/界面，用户可看的 MEMORY.md / daily_memory 恰好是
  最空的两个 → "感觉从不沉淀"。
- **RC3 daily_memory 是 LLM 可选**：只有空骨架自动创建（`daily_memory.rs::ensure_today`）；
  内容要求 LLM 主动调 `daily_memory` 工具，而它**不在** `acui_dispatch.rs` 的
  always-include 白名单（`memory` 在），可能被 tool router 按分截掉；mid-session 提示
  又已移除（run_loop.rs 里有注释）→ 每日记忆基本无内容。
- **RC4 事件不显眼**：`memory_update` 事件全链路存在（agent_loop → ws.rs →
  useStreamParser → execution_timeline），但渲染在助手气泡内部可折叠的执行时间线里
  （`ToolCallBubble.tsx::MemoryUpdateBlock`，且要求 hasAnyTextStep），用户几乎不会注意。

## Decision

落地内容（与文件对应）：
- **晋升 + 每日自动沉淀**：`memory_prefetch.rs::promote_session_memory_files` /
  `append_daily_memory_conclusion`，由 `memory_on_session_end` 调用；`MemoryKind` 新增
  `Daily` / `Promotion`（wire kind `daily` / `promotion`）。事件只在"新晋升条目"和
  "当日首条每日沉淀"时发射（否则每轮对话都会刷屏）。
- **白名单**：`daily_memory` 加入 `acui_dispatch.rs` always-include。
- **读取面**：`hermix-focus/src/embedded/ui.rs`（list/search/soft-hide/pin/stats +
  `has_fact`）→ `hermix-server/src/dashboard/memory_inspect.rs`（`/api/memory/graph|item|
  search|daily|overview|pin`，md 条目路径走进程内注册表 + home 边界校验）。
- **前端**：设置页新增「记忆管理」（`SettingsShell` memory 页）——
  `MemoryGraph.tsx`（echarts 力导向图，颜色运行时读 CSS 变量 + HSL 派生分类色，
  监听 THEME_CHANGED_EVENT 重建）、`PersonasPanel.tsx`（角色全文展示/编辑 + SOUL.md）、
  `MemoryTextPanel.tsx`（全局 MEMORY.md / USER.md）、`DailyMemoryPanel.tsx`（逐日内容）。
- **对话流**：`ToolCallBubble.tsx::MemoryConsolidationCard` 渲染在助手消息正文之下，
  与 execution_timeline 同源持久化；`MemoryUpdateEntry.memoryType` 扩展
  `daily` / `promotion` / `reflection`。

1. **晋升通道（修 RC1）**：会话结束时（`memory_on_session_end`）把会话隔离目录下
   `MEMORY.md` / `USER.md` 的条目解析后 `upsert_fact` 进 memory.db：
   `canonical_key = user.memory:<sha1 前 16>`，source=`memory_tool_promotion`，
   tags=[session_id]。`fact_ttl_for_key` 给 `user.memory:` 前缀 180 天 TTL
   （条目还在 md 里时每次会话结束重复晋升 → TTL 续期 + access_count 增长；
   条目被 archive 轮出后自然衰减）。幂等由 canonical_key UNIQUE 保证。
   发 `memory_update`（新 kind=`promotion`）。
2. **每日记忆自动沉淀（修 RC3）**：会话结束时把会话总结行追加进
   `daily_memory[今天].conclusions`（`- HH:MM sid前8: 摘要≤120字`，纯确定性、无 LLM 成本）；
   `daily_memory` 加入 always-include 白名单（LLM 主动记录保持不变）。
   发 `memory_update`（新 kind=`daily`）。
3. **沉淀显性化（修 RC4）**：新增独立"记忆沉淀"卡片渲染在助手消息文本之下
   （复用 memory_update 管道与 execution_timeline 持久化，零新事件类型）；
   时间线内的 MemoryUpdateBlock 行保留。
4. **记忆读取面（修 RC2）**：新增 `dashboard/memory_inspect.rs`：
   - `GET /api/memory/graph?max_nodes=` — 统一图谱：facts / frames（按 source 分类）/
     reflections / daily 天节点 / 全局+会话 MEMORY.md·USER.md 条目节点；
     边两类：frame→parent_id、day→当日 frame。节点带 salience/created_at/preview。
   - `GET /api/memory/item?kind=&id=` — 点击查看全文。
   - `DELETE /api/memory/item?kind=&id=` — frames 软隐藏（可逆），facts 软隐藏+FTS 同步。
   - `GET /api/memory/search?q=` — facts FTS5 + frames/reflections LIKE。
   - `GET /api/memory/daily` — 每日记忆列表与全文。
   - `GET /api/memory/overview` — 计数、最近 7 天写入量。
   memory.db 用与写侧相同的 `data_dir_in(home)/memory.db` 打开第二个只读连接（WAL 并发读）。
5. **设置页「记忆管理」**：SettingsShell 新增 memory 页：
   - **记忆图谱**（echarts `graph` force，零新依赖）：节点按 kind 着色、salience 决定大小，
     力导向 + 缩放平移；点击节点 → 详情抽屉（全文 + 元数据 + 删除/置顶）；
     搜索框高亮；颜色全部运行时读 CSS 变量 + `isDarkTheme`，监听 THEME_CHANGED_EVENT 重建。
   - **角色**：复用 `/api/persona/personalities`（内置只读全文展示 + 用户可编辑 textarea 保存）。
   - **记忆文本**：全局 MEMORY.md / USER.md 编辑器（已有 API）+ 每日记忆列表。

## Alternatives considered

- **取消会话隔离，memory 工具直写全局**：违背 `resolved_memory_dir` 的既有设计
  （多 agent room / 私聊隔离防污染），风险大；晋升通道同样达成跨会话留存。否决。
- **新增 `memory_consolidated` 流事件类型**：transport/parser/App 四层都要加分支，
  而 `MemoryUpdate` 的 action/kind 语义完全覆盖（加两个 kind 即可）。否决，复用现有事件。
- **3D 宇宙（react-force-graph-3d / three.js）**：为观感引入重依赖，违反六步阶梯；
  echarts 2D 力导向 + 深色发光即可达到"大脑宇宙"观感。否决。
- **记忆内容可编辑（facts/frame 正文手改）**：LLM 生成的记忆手改收益低、canonical_key
  一致性难维护；提供软删除（隐藏）+ 置顶已满足管理需求。否决，md/角色文本已有编辑。
- **晋升时用 LLM 改写/过滤条目**：会话结束路径不能依赖额外 LLM 往返（延迟+失败面）；
  条目本就是 LLM 写的记忆，直接晋升。否决。

## Consequences

- 对抗性自评（已过的检查单）：① 事件洪水 — 晋升/每日/总结各一条且仅会话结束发一次，
  前端聚合为一张卡片；② 图谱 payload — max_nodes 默认 400 + label/preview 截断；
  ③ memory.db 并发 — WAL + busy_timeout，读连接独立；④ 老 sidecar 无新端点 —
  前端错误态 + 需重建后端才生效（交付说明注明）；⑤ 主题 — JS 色值全部来自 CSS 变量，
  过 `check:tokens`；⑥ 晋升重复 — canonical_key 幂等，重复晋升只 bump access_count。
- 放弃：会话 md 与全局 md 的物理合并（保持隔离语义）；3D 渲染；记忆正文手改；
  向量召回增强（fastembed 可选依赖，geo-blocked）。
- 行为变化：长期记忆在跨会话召回中真实生效（事实面）；每日记忆不再是空壳；
  对话流出现可见的沉淀卡片。存储增速：晋升 facts 有 TTL，量级受 md 2200 字符预算约束。
