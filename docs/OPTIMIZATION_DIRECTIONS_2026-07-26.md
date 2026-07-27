# Hermix-Desktop 优化方向清单（对照 Hermes Agent 项目深度分析）

> 生成时间：2026-07-26
> 方法：先学习 Hermes 项目架构（Tauri v2 + Vue3 + Rust axum + 内嵌 Python Gateway agent），再对 hermix 在 **稳定性 / 性能 / 资源占用 / 架构健壮性** 四个维度做代码级审计（全部 file:line 已核对真实代码）。
>
> **关键架构差异**：Hermes 用独立 Python 进程跑 agent；hermix 是**纯 Rust agent loop**（18 crate）。本清单**全部保持纯 Rust，不做任何重写**，只做增量式局部改动。

---

## 核心结论

共 **17 个方向**，全部满足「真正有很大收益 + 不破坏当前架构」。按收益/风险比排序，**Top 5 必做**见末尾。

---

## 一、稳定性（Stability）

### ① 工具子进程超时未 kill → 孤儿进程泄漏　【头号稳定性】
- **证据**：`crates/hermix-environments/src/local.rs:274-333` —— `timeout()` 包裹 `child.wait_with_output()`，未设 `KillOnDrop`；grep 确认该路径无 `KillOnDrop`。
- **问题**：超时丢弃 future 并不终止 OS 进程，`storage_analyzer` / 长 `code_execution` 等子进程脱离管控继续跑，跨会话/循环累积，吃 CPU 与 PID。
- **收益**：杜绝孤儿进程；稳定性 + 资源双收益。
- **改动**：`local.rs:254` 加 `cmd.kill_on_drop(true)`；或在 `Err(_)` 分支 `child.kill().await`（1-2 行）。
- **风险**：低。

### ② 中毒 Mutex 链式崩溃（cascade panic）
- **证据**：`crates/hermix-server/src/lib.rs` 中 `active_handles`(5368/5449/5450)、`active_sessions`(5450)、`approval_registry`(195/231/265/281)、`session_bypass`(747/856)、`user_allowlist`(803/1034)、`recent_sends`(938) 全是 `Arc<std::sync::Mutex<…>>` + `.lock().unwrap()`。
- **问题**：任一任务在持锁时 panic → Mutex 中毒 → 后续**所有** `.lock().unwrap()` 全 panic，WS / 审批处理被级联拖垮。代码里 `local.rs:284` 已用 `.unwrap_or_else(|e| e.into_inner())` 的正确写法，但 server 主路径没照抄。
- **收益**：单点 panic 不再拖垮整条连接 / 审批链路。
- **改动**：上述站点 `.lock().unwrap()` → `.lock().unwrap_or_else(|e| e.into_inner())`。
- **风险**：低。

### ③ 后台进程 PID 只记录不回收
- **证据**：`local.rs:280-286` `background_processes.push(child.id())`，无 kill/reap；`LocalEnvironment` drop / 会话退出未清理。
- **收益**：避免长任务遗留 daemon 占用资源。
- **改动**：drop / 会话退出时遍历 `background_processes` 逐一 `kill()`。
- **风险**：低-中。

### ④ 序列化失败静默发空帧 → 前端流错乱
- **证据**：`lib.rs:6304/6409/5426` `serialize(...).unwrap_or_default()` 在 JSON 序列化失败时发 `""` 空帧，前端 `JSON.parse` 抛错、流错位，且**无日志**。
- **改动**：`Err(e)` 时 `tracing::error!` 并 skip 发送。
- **风险**：低。

---

## 二、性能（Performance）

### ⑤ 每轮全量 DELETE + 重插 message 历史　【头号性能】
- **证据**：`crates/hermix-agent/src/session_persistence.rs:1353-1415` `flush_messages` 先 `DELETE FROM messages WHERE session_id = ?1` 再循环重插**整个** `messages` 切片；每条 INSERT 触发 FTS shadow trigger(671-715) + 每条 `serde_json::to_string`(1391-1399) + `pii_redact` 正则(1401-1407)。
- **问题**：O(n) 写 × FTS 整表重索引 × serde+regex，长会话每轮都全量重写。
- **收益**：落库延迟与 DB 写量从 O(n) → O(新增)；消除整表 FTS 重索引抖动。
- **改动**：改为 append-only（只 INSERT 新消息 / 维护 `last_persisted_max_id` 水位），或 `INSERT … ON CONFLICT DO UPDATE`；schema 已有唯一 id。
- **风险**：中（须保证 reload 顺序/保真，加小 migration）。

### ⑥ 每轮整段历史 clone + 重序列化进 LLM 请求
- **证据**：`crates/hermix-agent/src/agent_loop/mod.rs:1469/2030/2100` `ctx.get_messages().to_vec()` 克隆整段；配合 ⑤ 每条 serde，整段历史每轮被拷贝序列化多次。
- **收益**：长会话每轮堆分配/延迟下降。
- **改动**：上下文存 `Arc<[Message]>` 或传 `&[Message]`；只克隆请求所需 tail；缓存序列化请求体直到历史变更。
- **风险**：低（改 API 取 `&[Message]`）。

### ⑦ compression O(n²) 评分 + 重复全量克隆
- **证据**：`compression_pipeline.rs:299-311` `frequency` 项 `all.iter().filter(...).count()` 每消息 O(n)；`drop_low_score:464` `messages.clone()` 后再 filter；`msg_content_lowercase` 每消息 alloc 两次(315/328)；`compress()` 又 clone(2209) + `agent_loop/mod.rs:1469` `to_vec()`。
- **收益**：1k 消息会话 `compress()` 从 ~1M 次比较 + 数千 alloc → O(n)。
- **改动**：循环前建 `tool_call_id` 引用计数 `HashMap<&str, usize>`；`msg_content_lowercase` 每消息只算一次；去掉多余 `messages.clone()`（对借用切片打分）。
- **风险**：低-中（保持评分一致，纯内部重构）。

### ⑧ async handler 内阻塞 rusqlite
- **证据**：`lib.rs:2805-3049` `delete_session` 是 `async fn` 却同步 `Connection::open` + 内联 `rebuild_messages_fts`(3005)，无 `spawn_blocking`，阻塞 executor 线程。
- **收益**：其它请求不再被单个 DB 调用阻塞。
- **改动**：DB 段包 `tokio::task::spawn_blocking`（agent 侧 `local_agent_service.rs:1538` 已有范式）。
- **风险**：低。

### ⑨ 每次工具调用新建 tokio Runtime
- **证据**：`tools/cua.rs:1812-1917` 多处 `Runtime::new()`；`backends/browser.rs:2125`。
- **收益**：减少线程 spawn/join 抖动（每次工具调用从建整套多线程 runtime 改为复用）。
- **改动**：复用 `tokio::runtime::Handle::current()` 或进程级 lazy `Runtime`。
- **风险**：低（确认调用已在 runtime 内）。

---

## 三、资源占用（Resource / Memory）

### ⑩ read_image base64 绕过压缩 → 每轮重发 + 落库　【头号内存/上下文泄漏】
- **证据**：`file.rs:651/698/721/742` `read_image` 把整图 raw base64 放进 `_image_data`；`compression.rs:1685-1691` Pass 4 仅当 content 含 `"data:image/"` 或 `"base64,"` 才剥离，但 `_image_data` 的 JSON 既无 `data:image/` 也无 `"base64,"` 子串 → **不剥离**；Pass 4.5(1709) 只处理 User `content_parts`，不动 Tool 结果。
- **问题**：每张读图产生数 MB blob，压缩后仍留在 Tool 消息，之后**每个 LLM 调用都重发** + 原样 persist 到 SQLite。
- **收益**：单图 MB 级 → ~40 字节上下文 + 库；多图/长会话上下文与磁盘双降。
- **改动**：Pass 4 条件加 `|| content.contains("\"_image_data\"")`（或检测长 base64 run `[A-Za-z0-9+/=]{10000,}`）；替换为 `"[image data removed; see path]"`，`path` 字段已保留，LLM 可再 `read_image` 复读。
- **风险**：低（保留 path 元数据）。

### ⑪ SQLite 缺 `synchronous = NORMAL`（5 处仓库）
- **证据**：`session_persistence.rs:37` / `automation/src/repository.rs:39` / `task/src/repository.rs:41` / `server/src/lib.rs:2793` / `cron/src/persistence.rs:192` 仅设 `WAL + busy_timeout`；仅 `hermix-focus/src/embedded.rs:478` 设 NORMAL。
- **收益**：每提交少一次 fsync，写延迟/CPU 约减半；WAL 下安全。
- **改动**：各 `open_with_wal` 补 `PRAGMA synchronous = NORMAL;`。
- **风险**：低。

### ⑫ Metrics DB 每次 flush 新建连接 + 表无限增长
- **证据**：`server/src/metrics.rs:206` `Connection::open` 每次 `write_to_db`；`:190` `metric_events` 表无保留期。
- **收益**：减少 CPU/IO 抖动；磁盘有界。
- **改动**：长生命周期单连接（仿 `session_persistence`）；定期 `DELETE FROM metric_events WHERE timestamp < ?`（如 30 天）。
- **风险**：低。

### ⑬ 服务端无界内存 map 不淘汰
- **证据**：`lib.rs:217` `session_map` / `:721` `recent_sends` / `:723`+`:5368` `active_handles`，grep `.remove`/`.retain` 无匹配。
- **问题**：request 完成不清理 → 进程生命周期内缓慢增长（内存泄漏）。
- **改动**：完成/teardown 时 `remove`；`recent_sends`/`active_handles` 加 TTL `retain` 清扫。
- **风险**：中（先确认完成路径再删）。

### ⑭ OCR 引擎常驻不释放（可选）
- **证据**：`file.rs:31` `static ENGINE: OnceCell<OarOcrEngine>`，首次懒加载后永不释放（ONNX + 模型约数十 MB）。
- **改动**：包 `Arc<Mutex<Option<Engine>>>` + 最后使用时间戳，空闲 N 分钟 drop 重建。
- **风险**：低-中（重建竞态用 Mutex 守）；内存宽松可保持现状。

---

## 四、架构健壮性（对照 Hermes 模式）

### ⑮ 多 profile / 工作区隔离仅部分实现
- **证据**：`session_persistence.rs:329` `db_path = data_dir.join("sessions.db")`；`config/paths.rs:432` 全局；memory 后端有 `set_profile_dir`(`memory.rs:48`) 但 agent 会话 DB / 配置未按 profile 分域。
- **收益**：真正多工作区数据隔离（隐私 / 独立上下文），对齐 Hermes 的 profile 隔离。
- **改动**：`data/profiles/<p>/sessions.db` 命名空间；复用 `set_profile_dir`；默认 profile + 一次性迁移既有 db。
- **风险**：中（需保证老用户单 db 平滑迁移）。

### ⑯ 启动端口冲突无自动探测 / 清理旧实例
- **证据**：`lib.rs:2203` `set_reuseport` / `:2207` bind 失败只提示手动重试；`main.rs:18` 默认 `127.0.0.1:3000`。
- **收益**：崩溃残留 / 端口被占时启动更顺（Hermes 有 `resolve_port` + kill-stale）。
- **改动**：`EADDRINUSE` 时顺序探测 3001.. 或让 OS 分配空闲端口；把实际端口回传 webview（前端 WS 硬编码 3000，需配套配置化）。
- **风险**：低-中（需前端 WS URL 配置化）。

### ⑰ HTTP 响应缺 CSP 头（纵深防御）
- **证据**：`security.rs` 鉴权/限流中间件从不 set `Content-Security-Policy`；webview CSP 只在 `tauri.conf.json:28`。
- **收益**：Dashboard HTTP 面 XSS  containment。
- **改动**：response 层注入与 Tauri 一致 CSP（允许 `ws://127.0.0.1:3000`，保留 SPA 所需 `unsafe-inline`）。
- **风险**：低。

---

## Top 5 必做（收益 / 风险比最高）

| 优先级 | 编号 | 方向 | 收益量级 | 风险 |
|--------|------|------|----------|------|
| 1 | ⑩ | read_image base64 泄漏（压缩绕过） | 高（上下文+磁盘双降） | 低 |
| 2 | ⑤ | 每轮全量重插 message 历史 | 高（长会话落库延迟） | 中 |
| 3 | ① | 孤儿子进程泄漏 | 高（稳定+资源） | 低 |
| 4 | ② | 中毒 Mutex 链式崩溃 | 高（单点 panic 不扩散） | 低 |
| 5 | ⑦ | compression O(n²) 评分 | 高（长会话压缩 CPU） | 低-中 |

---

## 刻意排除的方向（说明判断力）

- **不引入 Python Gateway / 不重写 agent loop**：这是与 Hermes 的根本架构差异，保持纯 Rust 是项目铁律，不在优化范围。
- **不加「加功能」类充数**：如 config 热重载、`/health` 探针过浅、provider key 明文——收益偏低或属打磨，不满足「真正很大收益」，未列入主清单。
- **⑮/⑯ 中风险项**按需再做（涉及数据迁移 / 前端 WS 配置化），已单独标注风险。
