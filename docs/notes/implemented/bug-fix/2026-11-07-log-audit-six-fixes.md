# Decision Note: 2026-11-07 日志体检后的六类修复（孤儿工具结果 / 技能 YAML / 效果记录 / 日志根 / 会话删除 / 磁盘）

Status: implemented — 2026-11-07 落地

## Decision

一天实机日志（`~/.hermix/logs` + 安装目录 `data/logs`）暴露六类问题，全部就地修复：

1. **孤儿 tool_result** —— 压缩管线会产生「有 tool_result、无配对 assistant tool_call」的消息。
   `semantic_dedup` 只按 role 判断，把**携带 tool_calls 的 assistant 消息**当成重复文本删掉；
   `context_collapse` 在中间切一刀，尾部起点可能落在 tool 组内部。provider 侧只能每请求丢弃
   （实机 `dropped=88` 持续 18 分钟，模型再也看不到那 88 个结果，反复重跑直到 max turns）。
   修法三处：`semantic_dedup` 跳过带 `tool_calls` 的消息（两个方向）；`context_collapse` 与
   `prune_old_tool_results` 的**产出**都过一遍 `sanitize_tool_pairs`。
2. **技能 frontmatter 非法 YAML** —— 写侧 `description: |` 后正文顶格，YAML 在第一个顶格行结束块，
   `## 标题` 被当成键 → 整份 frontmatter 报废（实机 55 份 auto 技能，`GET /api/skills` 每次刷 271 条）。
   写侧（`skill_auto_create` / `skill_patch`）改为块标量内容缩进；读取侧加修复式解析救存量文件
   （含 `name: 333` 这类被 YAML 推断成整数的）；坏文件告警改为每文件一次。
3. **技能索引按字典序截断** —— 98 个技能只放 80 行，砍谁全凭首字母。改为手写优先、auto 垫底。
4. **effects.db 只写不清** —— 未决项从 33 单调涨到 36 且每次启动重报（累计 2042 条 WARN）；
   入参存原文（`write_file` 单条 46 KB），表 13.9 MB。启动改为「24 h 未决收口 → 新鲜未决聚合告警
   （明细降 debug）→ 30 天保留期」，入参只存前 800 字符。
5. **会话删/改走读模式** —— `delete_session` / `patch_session` 用 `OpenMode::Read`（500 ms）开写事务，
   与在跑的 turn 抢写锁就 500（实机两次 DELETE 子代理会话 500、3 秒后重试成功）。新增
   `open_sessions_db_for_write`（5 s）。
6. **日志根三处分裂** —— `desktop.log` 硬编码 `~/.hermix`、`frontend.log` 走 `resolve_data_dir()`
   落到 `<install>/data`、`server.log` 走 `hermix_home()`。统一到 `hermix_home()`。

同批降噪：客户端断开后的 WS 丢事件、`override_existing = true` 的预期覆盖注册、「打包型自定义应用
缺 index.html」（`installed.json` 里有 `exe`）、流式未收齐时的 `argsJson` 解析失败上报。

## Alternatives considered

- **只修 provider 侧的丢弃（保持每请求 drop）**：治不了根 —— 模型永远拿不到那些结果，行为退化照旧。
- **在 ContextManager 每轮就地修复**：需要改持有者与借用模型，收益与三处产出修复重叠，未做。
- **删除 auto 技能目录**：可能删掉下次要复用的条目，且磁盘只占 2.5 MB；改为索引排序 + 健康度门槛。
- **把 `resolve_data_dir` 改成 `%APPDATA%`**：会搬运现网 `~/.hermix` 的历史数据，代价远大于收益。
- **effects.db 直接清空**：会丢掉「上次崩溃了哪些工具调用」的证据；保留 30 天窗口更诚实。

## Consequences

- 压缩后不再产生孤儿结果；`dropping orphan tool_results` 应为 0（若有残留，说明还有第四条产生路径）。
- 技能库 98/98 可解析（PyYAML 全量校验通过），索引里的 80 行优先给手写技能。
- `effects.db` 13.9 MB → 10.0 MB，未决 0；启动日志里 `recovery: non-idempotent ...` 不再逐条刷。
- 日志统一在 `<hermix_home>/logs`；安装目录 `data/logs` 只剩 11-05 之前的历史。
- 磁盘：`data/` 229 MB → 125 MB；`browser-profiles/` 1011 MB → 0（探针/重复/冒烟 profile 与两份
  超星登录态一并清除，运行时按需重建）；`skills/` 2.6 MB → 1.5 MB（62 个自动沉淀条目清空，
  剩 36 个手写技能）。
- `~/.hermix` 总量 2.0 GB → 1.02 GB。剩余大头顶为引擎与运行时（`viewer-webview` 309 MB、
  `dsh-engine` 199 MB、`runtime` 195 MB、`data` 125 MB），属功能必需，不属可回收项。
- 2026-11-07 追加：按用户要求把 `~/.hermix` 清空到只留模型配置（`config.yaml` / `.env` /
  `auth.json`，共 12 KB），用于从零观察新行为；其余内容（会话、记忆、技能、自动化任务、MCP 配置、
  引擎与全部缓存）已备份到 `<USERPROFILE>\hermix-home-backup-20261107`（150 MB），
  需要时可单个恢复。
