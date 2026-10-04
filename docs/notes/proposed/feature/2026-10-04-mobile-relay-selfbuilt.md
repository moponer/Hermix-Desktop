# Decision Note: 手机端伴侣客户端 + 自建 Relay（安卓优先）

Status: proposed — P1 地基已独立成篇并落地（见 [IM 远程任务生命周期](../../implemented/feature/2026-10-04-im-turn-lifecycle.md)）；本篇只剩手机端与 relay，5 项待拍板见 §待拍板

> P1（IM 远程任务生命周期）已拆出本篇并转入 `implemented/`，不再记于此。
> 交叉引用：[IM 远程指挥三层递进（2026-09-29）](../../implemented/process/2026-09-29-stability-and-remote-plan.md)（远程第 1 层「Dashboard 移动审批 + token 认证」标记为未做，见 §待拍板 6）、[IM 会话工作区范围（2026-09-30）](2026-09-30-im-channel-workspace-scope.md)（仍 proposed，本篇不得与之矛盾）。

## Decision

为 hermix 建设手机端远程入口，采用**全自建**路线，分两个子项目：

1. **P1 — IM 远程任务生命周期**（地基）：已独立成篇并落地 → [IM 远程任务生命周期](../../implemented/feature/2026-10-04-im-turn-lifecycle.md)。QQ / 企微通道与未来的手机端共用此地基。
2. **安卓伴侣 App + 自建 Relay**（本篇剩余范围，手机端命名 **Hermix M**）：手机（Kotlin + Jetpack Compose）↔ 自建 relay（Node + WS + SQLite，只路由不执行）↔ hermix-server（新增出站连接器 + 3 个对接接口）。

三条原则：

- **不自研手机引擎**：agent 引擎只在 PC（Windows MSVC Rust），手机是薄客户端（companion node，定位同 OpenClaw Android）。
- **不 fork 现成客户端**：dshmobile 安卓端闭源（只有插件 / 协议 / relay 开源），Clawket 引入 Expo/RN 新技术栈维护面；改用「功能抄设计、代码全自建」。
- **relay 是唯一通道，且不做直连降级**（2026-10-04 拍板）：App 一律经 relay 连接，PC 出站主动连 relay，任何网络可通。**没有第二条路**——relay 不可用时手机端整体不可用，包括在家里 LAN 上；家里 LAN 上的每条消息同样绕公网，因此不存在「外网 / 本地同一体验」。取舍与随之而来的约束见 §待拍板 4（已定）。

以上三条是已定决策。**P1 地基已按 §实施顺序与验证 落地 6/7 个 PR**（PR-1~6，`ci-gate.sh` 11/11 通过，`hermix-agent` 1045 / `hermix-server` 281 测试全绿）。**手机端具体形态仍有 5 项未决，且第 1 项（App 的执行路径）阻塞其余全部设计**——未决前不得开工 App 与对接层。

## 参考项目与抄了什么设计

| 项目 | 地址 | 许可 | 抄的设计（不抄代码） |
|---|---|---|---|
| OpenClaw queue | github.com/openclaw/openclaw（docs.openclaw.ai/concepts/queue） | MIT | lane 化 FIFO、steer/followup/interrupt 三模式、排队回执、cap+drop:summarize 策略 |
| dshmobile | github.com/zdx8637-gitdog/dshmobile | MIT（插件 / relay）/ App 闭源 | 扫码一码三用（下载 / 配对 / 授权）、relay 三件套结构、断线自愈重连、E2EE 威胁模型（docs/plan-e2ee.md，二期采纳） |
| Clawket | github.com/p697/clawket | MIT | agent-protocol 共享协议契约思想、agent roster、工具调用实时跟踪 UI、多后端能力探测 |
| ClawApp（qingchencloud） | github.com/qingchencloud/ClawApp | 开源 | IndexedDB 离线缓存 + 增量同步、消息去重、智能重连（映射到 App 侧 Room） |
| Dots（OpenAI） | chatgpt.com/dots（闭源） | — | 交付闭环形态：任务结束主动汇报、审批后动手、心跳可见 |
| cc-haha | hermix 注释引用的前身 | — | 审批数字解析互斥规则（已在代码，保持） |

上表为既有调研结论，本篇未重新复核；其中 dshmobile 的 relay 部署方式与协议契约细节直接决定 §待拍板 3 / 4 的答案，见该节。

## 总体架构

```
安卓 App(Kotlin+Compose) ──WSS──▶ relay(apps/relay/, Node) ◀──出站WSS── hermix-server(Rust)
                                     │
                                只路由不执行；按 token→电脑 映射转发帧；存 token 哈希；审计
```

- relay 是唯一中间人：手机不寻址 PC，PC 出站主动连 relay（同 QQ 通道「双方主动连中间人」的连通模型）。
- relay 代码内**零 hermix 业务逻辑**：不理解会话内容，不执行 agent，不解析内层帧（硬约束见 §后端设计 10）。
- 信任模型：会话内容**一期明文过 relay**；relay 只见路由元数据与 token 哈希。二期采纳 dshmobile `docs/plan-e2ee.md` 的 X25519 + XSalsa20-Poly1305 握手，触发条件是 relay 部署到不可信主机或多用户共用。
- **App 的执行路径未定**（§待拍板 1）——本图只画通了链路，没画清 App 的消息进入哪条执行路径。

## 多端隔离（多手机不串的机制）

| 层 | 机制 | 归属 |
|---|---|---|
| 连接层 | 每手机独立 token（relay 侧注册表），token 哈希 → 电脑 WS 连接的唯一映射；A 的帧物理上到不了别人的电脑 | relay |
| 会话层 | 每台电脑内，配对设备来源的对话落独立确定性会话（键含 device_id），上下文 / 历史 / 审批队列互不可见 | hermix-server（`session_persistence`） |
| 授权层 | 每设备独立 token + 能力位交集（`Origin::may` ∩ 设备 scopes）；桌面「设备」页可单独 revoke 任意设备 | hermix-server（`DeviceRegistry`） |
| **可见性层** | **所有按 sid 或跨设备返回的接口，必须按 `Origin::device_id()` 过滤，只回本设备可见的 sid** | hermix-server（新增，随 §后端设计 6 落地） |

可见性层是新增的：连接层保证帧不串，但 hermix 侧接口若不做归属过滤，A 手机能列出 B 手机的会话名与首行，隔离在 API 层就是漏的。IM 账号同样持有 `paired_devices` 行（`channels/session.rs:30-33`），这条防线对 IM 账号与 App 设备一视同仁。

**能力位的硬边界**：`Origin::PairedDevice.may(ApproveAlways)` 是**无条件 `Denied`**（`hermix-core/src/origin.rs:65-72`，注释「授权类 / 配置类 / 管理类一律拒绝」），且 `effective_decision_for_paired` 对矩阵 `Denied` 不做挽救（`hermix-server/src/security.rs:429-440`）。因此设备 scopes 里即使写 `approvealways`（`security.rs:403` 能解析）也不生效。**「永久放行」不能经 HTTP 端点授予**，只能走不过能力矩阵的 channel 侧路径——这是 §待拍板 1 与 2 绑在一起的原因。

## 后端设计（函数级）

### P1：IM 远程任务生命周期（改 crates/）

1. `hermix-core/src/traits.rs:624` — trait `AgentService` 新增两个方法（`interrupt_all` 在 `:676` 已是默认空实现，沿用同一手法）：
   ```rust
   fn interrupt_session(&self, sid: &str) -> bool { false }  // 默认实现；仓库内 ≥3 个实现体
   fn steer_session(&self, sid: &str, text: &str) -> bool { false }
   ```
   `steer_session` **提为 P1**（原标 P2 与 §后端设计 3 的 Queue steer 模式、实施顺序 step 1 冲突）。
2. `hermix-agent/src/local_agent_service.rs` — `LocalAgentService` 固有方法（`active_interrupts` 在 `:215`）：
   - `interrupt_session(&self, sid) -> bool`：锁 `active_interrupts` → `remove(sid)` → `InterruptController::interrupt(None)`，返回是否命中。
   - trait impl 委托固有方法；`interrupt_all()`（`:407`）改为遍历剩余条目复用它。
   - `steer_session`：**已查明「按 sid 找 `pending_steer`」今天不可达**，不是降级问题。`InterruptController`（`hermix-agent/src/interrupt.rs:47-56`）只持 `flag` / `redirect_message` / `notify`，不持 agent 句柄；`pending_steer` 是 `agent_loop/mod.rs:137` 上的 `pub(crate)` 字段，唯一公开入口是 `AgentLoopConfig::steer()`（`config.rs:669`），而该 Arc 在 builder 里创建后被移入 loop（`config.rs:360/412`）。
     实现方向：新增 `steer_buffers: Arc<Mutex<HashMap<String, Arc<Mutex<Option<String>>>>>>` registry，在 `active_interrupts` 登记 / 清理的同一处（`local_agent_service.rs:1649/1656/2860`）一并登记 / 清理；builder 里的 `pending_steer` Arc 需在移入 loop 之前 clone 一份——**这处交接是本项唯一需要现场确认的接线点**。列为独立 PR（见 §实施顺序与验证 PR-6）。
3. `hermix-server/src/channels/session.rs`：
   - `try_handle_immediate` 的 `Command::Stop`（`:364`）：`interrupt_all()` → `interrupt_session(&sid)`，命中回执「已中断」，未命中「没有在运行的任务」。`sid` 用 `session_id(platform, user)` = `im:{platform}:{user}`（`:36-38`）；`running` 的 key 是 `Self::key()` = `{platform}:{user}`（`:232`）——同形不同前缀，改造时勿混。
   - `run_turn` 尾部四路收尾（原来**只有 `Err` 分支**，成功什么都不发，这就是「任务结束无回执」的根因）：用户中断 → `⏹ 已按 /stop 中断，部分结果已保存`；长任务完成 → `✅ 完成：{res.text 截 200 字}`；短任务成功 → 静默；失败 → `crate::util::user_facing_agent_error` 路径加 `❌` 前缀。`Err(AgentError::Interrupted)` 单独分流，不报成「出错了」——用户自己叫停的，报系统坏了是误导。
   - **✅ 回执只给跑够久的轮次**（`LONG_TURN = 60s`，与 PR-3 心跳同源）。用户盯着屏幕看完答案再补一条「完成」是纯噪音，且每轮多烧一条 IM 消息；而这轮回执的用途（人已走开、回来要知道结果）只对长任务成立。
   - `util::was_interrupted(&AgentReply) -> bool` = `interrupted && finish_reason == "interrupted"`。**两个字段必须配对**：单看 `interrupted` 会把后端 5xx 与致命工具参数耗尽（`graceful_interrupt_result_errored`，`finish_reason = "error"`）误报成用户按了 /stop。`send_message_stream` 返回 `AgentReply` 而非 `AgentResult`，正文直接取 `AgentReply::text`，不需要翻 `messages`。
   - `ToolTrace` 从 pump 局部变量提升为 `Arc<Mutex<ToolTrace>>` 供心跳读取。**锁的粒度是「改完立刻放」**——guard 跨 `.await` 持有既非 Send 也会把 await 点锁死。节奏抽成 `struct Heartbeat { fast, slow, fast_beats, max_beats }`（生产走 `Default` = 60s / 300s / 30 / 60，测试传毫秒级，否则测不动）。心跳生命周期**就是本轮**：`send_message_stream` 返回即 `abort()`，**不去查 `running`**——那是多余的一层，且 `ChannelRuntime.running` 不是 `Arc`，跨 task 借不到。轨迹为空说明模型还在想，发 `⏳ 还在想…` 而不是空的进度行。降频切换那一刻**明说一次**（不许静默拉长间隔）；`max_beats` 是泄漏兜底，不是产品决策。
   - 新增 `struct SessionQueue { items: VecDeque<(Inbound, Arc<dyn ImPort>)>, dropped: Vec<String> }`，`ChannelRuntime` 加 `queues: Mutex<HashMap<String, SessionQueue>>`（key = `Self::key(platform, user)`）。**纯数据结构、不依赖 `HttpServerState`**，所以能直接单测；runtime 侧只留 `is_busy` / `enqueue` / `drain_queue` 三片薄壳。
   - **入队判定是 `running.contains(key) || queues.contains_key(key)`，不能只看 `running`**：`run_turn` 结束会把自己从 `running` 摘掉，只看它的话排水期间新到的消息会**插到已排队消息前面**。让队列 key 活到排空为止，既堵住乱序，也让排水期间到达的消息正确续在队尾，还不必再加一个 `draining` 集合。
   - `handle_message`：`is_busy` 命中时入队 + 回执 `📥 已排队（第 N 位）`——位次必须回执出去，否则用户无法区分「排队了」和「没收到」。
   - `drain_queue` 挂在 `run_turn` 之后、**仍在同一把聊天锁内**（同会话必须串行），让排队轮次的心跳 / 三态回执 / 审批走同一条路径。`queues` 是 std Mutex，**绝不能跨 await**——锁内只取队首，跑轮在锁外。排空后摘掉 key。
   - 超限不静默丢：挤掉最旧一条，其前 60 字进 `dropped`，排水时并进下一轮正文开头，agent 和用户都看得见有东西没跑。摘要**只并一次**，不粘到每一条上。
   - `parse_command` 扩展：`Command::Queue(QueueMode)`（`Report` | `Interrupt`）、`Command::Tasks`、`Command::Menu(MenuAction)`。`Steer` 等 PR-6 落地。
     **菜单用字母 a-d，不用 1-4**（与下一行的原写法相反，实现时改掉）：两套语义靠 `pending` 计数切换同一串数字，正是 cc-haha 那条闸门要防的事。字母与审批数字不同形后，**菜单任何时候都能用，不需要 pending 闸门**。裸数字只在 `pending == 1` 时是审批；`pending == 0` 或 `> 1` 一律不是命令。回归红线：`pending == 3` 时打 `3` 必须返回 `None`。
   - 菜单里的「中断」必须走 `try_handle_immediate`（**不需要聊天锁**那条路）：轮次在跑时锁正被持有，等锁就等于排到自己队列后面，中断命令本身失效。
   - `/queue interrupt`：先摘队首（否则中断完它还会被排一次）→ 中断 → **先发状态再开跑**（`run_turn` 自己会发三态回执，顺序反了会让人以为没生效）。
   - `running` 从 `HashSet` 改成 `HashMap<key, 当前消息正文>`：不另加并行 map（两份状态会漂），编译器把所有使用点找齐，共 4 处。`/tasks` 因此能报出「在跑什么」而不是干巴巴一句「运行中」。
   - `first_line` 取**第一条非空行**（用户消息常以换行开头，`lines().next()` 会返回空串）。
   - **`/tasks` 附带「上一轮没跑完」提示**（`Command::Resume` 提供 `/继续` 恢复）：判据读 `task_record_store.list_by_conversation(sid)` + `task_record_sink::last_turn_unfinished`（`turn-` 前缀 + 非终态）。**不做启动时主动推送**——启动时用户多半不在，只会收到一条无人认领的消息；且枚举「所有配对过的 IM 身份」要另写一遍。
     **判据为什么曾是错的**（本文早先写的「用 `hermix_task` / `TaskRecordSink` 的轮次记录」已撤回）：`TaskRecordSink` 只在 `SubAgentOrchestrator` 里被调用（`sub_agent_orchestrator.rs:572` 记 running、`:444` 记终态），记的是 `delegate_task` 派发出去的 **sub-agent**；**主对话轮次当时根本不写 TaskRecord**。虽然 `TaskRecordStore::list_by_conversation`（`record.rs:277`）存在且按时间倒序，但拿它判「上次对话中断」会双向出错：没用过 `delegate_task` 的会话查出来是空的（漏报），用过的会把子任务记录误当对话轮次（误报）。现已由 PR-7 补上主轮次记录，判据从此可靠。
   - `/继续` 走带上下文续跑（§后端设计 4）。
   - `HELP_TEXT` 编号菜单化；裸数字仅在 `pending == 0` 且空闲时映射菜单（见上）。`HELP_TEXT` 现有文案「/stop 中断本机正在跑的任务（会波及本机所有会话）」（`:114`）必须同步改。
4. **Resume 不做盲重放**。从 DB 取上一条 user 消息原样重发，会让上一轮已完成的副作用执行两次。改为带上下文续跑：重发时附「上一轮在执行过程中中断了（未跑完）。下面是当时的任务，请**先检查现状**再决定怎么做，不要假定任何操作都没做过」+ 原文。`dedup_key` 必须重新生成，否则会被去重表当成重投丢掉。
5. P1 对抗审查（已析）：①队列与聊天锁死锁（排水在 guard drop 后，锁外取锁内跑）②队列积压（cap + summarize）③超长 steer（`steer::normalize_accepted_steer_text` 已有闸，超长截断）④心跳 task 泄漏（双保险退出 + 降频不发静默）⑤trait 加方法破坏实现方（默认实现）⑥中断误报（只认 redirect 标记）⑦数字菜单与审批撞车（`pending == 0`）⑧**待补**：`interrupt_session` 与 `run_turn` 收尾清理 `active_interrupts`（`local_agent_service.rs:1649/2860`）之间的竞态；双设备同时 Stop 同一 sid 的幂等性。

### 对接层：hermix-server 面向手机 / Clawket 协议的接口

6. `hermix-server/src/ws.rs` — 握手加 Bearer 分支。`ws.rs` 现无任何 `Authorization` / `Origin` 处理，这是新增面。**必须复用既有鉴权组件，不得在 ws.rs 内重新实现**：`security::derive_origin`（设备凭证优先于 loopback，`security.rs:375-386`）、`effective_decision_with_scopes`（`:450`）、`unpaired_gate_allows`（`:471`）、`RateLimiter`。手写一套会让 WS 面绕过未配对闸门、IP allowlist 与限流。
   - `Authorization: Bearer hmxd_*` → `DeviceRegistry::authenticate()`（`pairing.rs:198`）→ `derive_origin` → 记 `AuditEvent::Auth`（`pairing.rs:63`）。
   - 未命中 401；连续失败计失败预算（复用 pairing 分桶思路）+ 延迟响应。
7. 新增 2 个 handler。**路径前缀对齐既有审批族 `/v1/`**（`POST /v1/sessions/{id}/approval`、`GET /v1/sessions/{id}/approvals/pending`，`routes.rs:1547/1586`）——App 是 `/v1` 客户端，不引入第二套前缀。授权一律走能力位交集，不手写 `require_local` OR 设备鉴权的二元判断（`require_local` 是 Origin 判定而非源 IP 判定，见 `channels/mod.rs:406`）。
   - `POST /v1/sessions/{sid}/interrupt`：`Local` 或 `PairedDevice` + `Chat` scope **且 sid 归属本设备** → `agent_service().interrupt_session(sid)` → `{interrupted: bool}`。
   - `GET /v1/tasks`：读 running + queues → `[{sid, state, first_line, queued_at}]`。**授权与归属过滤不可省**：`first_line` 是用户 prompt 正文，且不按 `origin.device_id()` 过滤即跨设备泄露（本篇 §多端隔离 可见性层）。配对设备三态单测（本地 / 配对设备 / 未认证）+ 一条「A 设备查不到 B 设备 sid」断言。
8. 出站 relay 连接器（新文件 `hermix-server/src/relay_client.rs`）：
   - 启动时若配置 relay 地址，主动 WSS 连 relay，携设备注册帧（token 哈希 + 能力位）；断线指数退避重连。
   - 帧协议单一真相源 = `relay_proto.rs`（含 `proto_version: u32`），App 启动校验版本。**relay 侧不重复定义帧格式**（见 §后端设计 10）。
9. 协议映射适配层（新文件 `hermix-server/src/mobile_protocol.rs`）：Clawket 式事件名 ↔ hermix envelope 的映射**单向收口**在此文件，禁止散落；审批三态（deny / once / session）语义在此对齐。**「session 永久」在 channel 侧对齐，不映射到 HTTP 能力矩阵**（见 §多端隔离 能力位的硬边界）。

### relay（apps/relay/，新建，Node + Express + WS + SQLite）

10. 模块清单：`src/auth.js`（用户注册 / 登录，token 哈希存储）、`src/devices.js`（设备登记 / 撤销）、`src/pairing.js`（配对码中转，6 位码语义与 `hermix-server/src/pairing.rs` 对齐）、`src/router.js`（token → 电脑 WS 映射与帧转发，**唯一含转发逻辑的文件**）、`src/audit.js`（审计行，对齐 `pairing.rs` 的 `AuditRow` 字段）、`src/proto.js`（**仅外层信封 + 版本号**，见下）。
11. 硬约束：不存明文 token；**不解析内层帧内容**；不执行任何业务；每连接记审计（连接 / 配对 / 路由 / 断开）。
12. **外层信封足够**：既然 relay 不解析帧内容，它只需要 `{type: hello|pair|route|ack, token_hash, proto_version, payload}` 五类外层记录，内层 payload 整体透传。据此 `proto.js` 收窄为外层信封定义，**帧格式只有 `relay_proto.rs` 一处真相源**，不再有第二份手写定义。`proto_version` 仍保留作双端漂移的运行时兜底。

## 前端设计（安卓 App，组件 / 布局 / 色系）

### 技术栈与工程

- Kotlin + Jetpack Compose + Material 3；MVVM；Room（离线缓存）；OkHttp（WS + REST）；minSdk 26（无 GMS 依赖）。
- 新目录 `apps/android/`，**不进 workspace 编译链**（不触碰 Rust 交叉编译禁区）。
- token 存 EncryptedSharedPreferences（Keystore）。
- **信任建立不许由手输地址完成**（原「首连记录证书 sha256 的 TOFU」不足）：配对码 / 二维码内绑定 relay 身份（host + 证书 sha256），App 显式展示 host 与指纹并要求用户确认；手输 relay 地址只允许用于**二次连接**，且必须与已确认指纹一致，否则断开。TOFU 单独用的物理问题是：首次连接的信任对象由用户输入决定，输错地址或扫码环节被换址即等于交出设备 token。

### 四屏（范围锁死，超出砍进 backlog）

**屏 1 配对（`ui/pairing/PairingScreen.kt`）**
- 垂直居中列：logo（56dp）→ 标题「连接你的助手」（headlineSmall）→ 说明（bodyMedium, text-muted）→ 6 位码输入（4 个单字符 OutlinedTextField 横排，间距 8dp）或「扫码」（FilledTonalButton，48dp 全宽）→ 底部「relay 地址」折叠项（ExposedDropdownMenuBox）。
- **配对确认页必须显示 relay host + 证书指纹**（见上）。
- 深链 `hermix://pair?code=…` 预填码；状态机：输入 → 本地格式校验 → 提交 → 成功跳主屏 / 失败在页内 error 行（不 toast）。

**屏 2 对话（`ui/chat/ChatScreen.kt` + `ChatViewModel`）**
- Scaffold——TopAppBar（标题 = 会话名，右侧溢出：切换会话 / 新建）→ LazyColumn 消息流（底部 inset 避 IME）→ Composer 行。
- 气泡：用户右对齐（`PrimaryContainer`，圆角 16dp，最大宽 80%）；助手左对齐（`SurfaceVariant`）；工具状态行独立组件 `ToolTraceRow`（Icon + 工具名 + 12dp `CircularProgressIndicator`），渲染规则抄 IM 端 ToolTrace。
- 流式：WS 增量 append 到当前气泡；断线重连后按游标拉增量（Room 表 `messages(sid, seq, role, content, tool_json)`，去重靠 seq）。
- Composer：TextField + 发送 IconButton；运行中显示「排队中…」提示行。
- **PC 重启 / 任务中断要显式呈现**：消费 P1 的「上次任务中断于 {时间}，发「继续」可恢复」回执，给「继续」按钮（走 §后端设计 4 的带上下文续跑，不做原样重发）。

**屏 3 审批（`ui/approval/ApprovalCard.kt`）**
- 消息流内嵌卡片（`SecondaryContainer` 底，1dp `OutlineVariant` 描边）：工具名（titleMedium）+ 参数折叠（Json 摘要，超 3 行截断 + 展开）+ 三键横排 FilledTonalButton。
- **三键是否成立取决于 §待拍板 1 的结论**：走 channel 侧则「允许一次 / 永久允许 / 拒绝」与 IM 1/2/3 同源同义；走 REST 则「永久允许」**必然被拒**（能力位硬边界），必须砍成两键。不得先做三键再发现点不动。
- **必须渲染自动拒绝倒计时**：审批超时默认 600s、到期自动 deny，字段已随帧下发（`hermix-transport/src/lib.rs:521-527`；真相源 `hermix-config/src/config.rs:782-783`）。`2026-09-30-im-channel-workspace-scope.md:95` 写的 300s 是过期数字，本篇以 600s 为准并建议同 PR 修正该篇。
- **必须处理 `expired` / `skipped`**：600s 自动 deny + relay 往返使「点下去时已过期」成为常态而非边缘情况（`apps/desktop-tauri/src/types.ts:8-14` 已定义这两种结果；`routes.rs:1640` 记着批量审批 + 零超时导致 agent 永久阻塞的真实事故）。已决定的卡片置灰 + 徽标；过期的显示「已超时自动拒绝」而非静默无响应。
- 数据流：WS `$events` 审批瀑布帧 → ViewModel → 卡片；点击 → 复用 `POST /v1/sessions/{id}/approval` → 卡片更新。

**屏 4 任务（`ui/tasks/TasksScreen.kt` + `TasksViewModel`）**
- LazyColumn，每行 `TaskRow`：状态图标（运行中 CircularProgressIndicator / 排队 Schedule / 中断 History）+ 任务首行（40 字）+ 相对时间；行尾 StopIconButton（确认对话框后调 interrupt）。
- **Stop 走同一条路径**：屏 4 的 Stop 与 IM `/stop` 调同一个 `interrupt_session`，不各留一个 HTTP 端点和一个 channel 入口。
- 下拉刷新；空态「当前没有任务」。
- 数据源：`GET /v1/tasks` 轮询（10s）+ WS 推送双通道。
- **任务词汇要说清**：server 侧存在三套「任务」——`hermix_task` 的任务记录（`TaskRecordStore`）、channel 的 running/queued 轮次、sub-agent 任务工具记录。屏 4 只呈现**对话轮次**（running + queues），笔记要写明它与另两套的关系，否则 UI 文案与 `/tasks` 语义会漂。

### 色系（Material 3，深色为主）

| 桌面 token | M3 role | 用途 |
|---|---|---|
| bg-primary | surface | 页面底 |
| bg-secondary | surfaceContainer | 卡片 / TopAppBar |
| accent | primary | 主按钮 / 链接 / 发送键 |
| accent-soft | primaryContainer | 用户气泡 / 徽标底 |
| text-primary | onSurface | 正文 |
| text-muted | onSurfaceVariant | 次要文字 / 时间戳 |
| error | error | 失败 / 拒绝 |

不引入新色板，映射桌面端现有 CSS token 语义。亮/暗跟随系统；形状：气泡 16dp、卡片 12dp、按钮全圆角（24dp 胶囊）。

### 桌面端配套（小改）

- `apps/desktop-tauri/src/utils/time.ts:63` — `imSourceOf` 当前只返回 `"qq" | "wecom" | null`，**必须扩展识别设备来源会话**，否则 §后端设计 1 产出的 `im:device:{device_id}` 在侧栏与 ChatView 里不被识别为远程会话。`ChatView.tsx:971` 已在传 `imSource`，**复用该 prop，不要在 `Composer.tsx` 再调一次 `imSourceOf`**。
- `apps/desktop-tauri/src/components/Composer.tsx` 加 prop `imRemote: boolean`；true 时替换为 `ImConsoleBar.tsx`（新文件）：停止按钮（调 interruptSession）+ 审批计数徽标（复用 `ApprovalDrawer` 的 pending 数据流，按 sid 过滤 `im:` / `device:` 前缀）。
- `apps/desktop-tauri/src/api/rest.ts` 加 `interruptSession(sid): Promise<boolean>`。

## 前后端连接逻辑

| 链路 | 协议 | 认证 |
|---|---|---|
| 配对 | relay HTTPS → 中转 hermix pairing API | 6 位码一次性 + 显式确认 relay host/指纹 |
| 对话流 | relay WS 帧（含 proto_version，内层透传）→ hermix WS 处理 | `Bearer hmxd_*` → `DeviceRegistry::authenticate` + `derive_origin` + `Chat` scope |
| 会话 / 任务 / 审批 resolve | REST over relay，前缀 `/v1/` | 同上 + sid 归属过滤 |
| 断线 | App OkHttp 退避重连 → 按游标拉增量；PC↔relay 断线由 `relay_client` 退避重连；App 明示「连接断 ≠ 任务停」 | — |

## 现状 → 完成后对比

| 能力 | 现状 | 完成后 |
|---|---|---|
| `/stop` | `interrupt_all()` 波及全机 | 只中断发起者会话 |
| 任务结束 | 成功无回执；中断无提示 | 成功 / 失败 / 中断三态回执 |
| >60s 长任务 | 仅 tool 事件刷帧 | 周期心跳带工具轨迹（上限后降频，不静默） |
| 运行中发消息 | 聊天锁阻塞，轮后全部重跑（易炸） | FIFO 排队 / steer 注入，interrupt 三模式 |
| 手机入口 | 无（仅 QQ / 企微 IM） | 原生 App：对话 + 审批 + 任务 + 配对 |
| 远程审批 | IM 回复 1/2/3 | App 卡片（语义同源）+ 倒计时 + expired 处理 |
| 审批「永久允许」 | 仅 IM 1/2/3 通道侧可授予 | 同上（HTTP 侧结构性不可授予） |
| 外网可达 | 经腾讯服务器（仅 QQ） | 自建 relay，任何网络 |
| 桌面端 IM 会话 | 徽标 + 可输入 Composer | 控制台（停止 + 审批），双端不打架 |

## 实施顺序与验证

1. **P1 地基**（§后端设计 1-5）→ `cargo test -p hermix-agent -p hermix-server --lib`（新增：队列入队 / 排水、`/tasks` 渲染、数字互斥 `pending == 0`、`interrupt_session` 单测、`was_interrupted` 只认 redirect 标记）。P1 不依赖任何待拍板项，可立即开工。
2. **对接层**（§后端设计 6-9）→ 同上 + 权限矩阵三态单测（本地 / 配对设备 / 未认证）+ 跨设备 sid 隔离断言。依赖 §待拍板 1。
3. **relay**（`apps/relay/`）→ `node --test` + `scripts/smoke-relay.sh`（起 relay + 模拟两端握手 / 路由 / 断连）。依赖 §待拍板 3。
4. **安卓 App**（`apps/android/`）→ `./gradlew assembleDebug` + ViewModel/Repository 单测；真机冒烟：配对 → 对话 → 审批（含 expired 分支）→ 中断。依赖 §待拍板 1、2、4。
5. **门禁**：全量 `./scripts/ci-gate.sh`；App 与 relay 各接一条 CI（App 至少 `./gradlew test`）。**不接受「本期只到本地构建」**——四屏 + 审批是本方案交互风险最高的部分，零自动化验证等于把 review 压力全压到人工真机冒烟上。

### P1 开工清单（不依赖任何待拍板项，可立即开工）

**PR-1 按会话中断** ✅ 已落地（`feat/auto-20261004-1c1479c0` worktree）
- 改动：`traits.rs:624` 加 trait 方法（默认 `false`）；`local_agent_service.rs` 加固有方法（`active_interrupts:215`）；`channels/session.rs:376` 调用点替换为 `interrupt_session(&session_id(platform, user))`。
- **已核实的爆炸半径**：`interrupt_all()` 全仓只有 2 个真实调用点——`session.rs:376`（本次改）与 `hermix-server/src/lib.rs:583`（关机路径，**保持 `interrupt_all` 不动**，关机语义本就是全停）。改动面 = 1 个调用点，trait 方法不 repurpose。
- 验收：`interrupt_session` 命中 / 未命中两测；`interrupt_all` 改造后行为回归测。
- 对抗：`remove(sid)` 与 run_turn 收尾清理 `active_interrupts`（`:1649/1656/2860`）的竞态；同 sid 双 Stop 幂等。
- 可见变更：`HELP_TEXT:114`「会波及本机所有会话」文案必须同步改 + release note 记一笔。

**PR-2 三态回执 + `was_interrupted`** ✅ 已落地
- 验收：成功 / 失败 / 中断三态各一测；`was_interrupted` 负测（普通错误不得被判成中断）。

**PR-3 心跳** ✅ 已落地
- `ToolTrace` → `Arc<Mutex<_>>`；`Heartbeat { handle: JoinHandle }` 60s；`send_message_stream` 返回后 `handle.abort()`。
- 验收：间隔**参数化**后再跑长任务测（写死 60s 则测不动）；abort 生效、无 task 泄漏；上限 30 次后转 5min 降频或发「仍在运行」，不许静默。

**PR-4 队列** ✅ 已落地（`SessionQueue` 纯数据层 5 测：FIFO / 满队列挤最旧且摘要并进下一轮 / 摘要只并一次 / `cap=0` 不失控 / 长文本按字符截断。**未测项**：`enqueue` / `drain_queue` 挂在 `ChannelRuntime` 上，构造它需要 `HttpServerState` fixture；「不跨 await 持锁」靠代码审查保证（每个 `lock()` 的 guard 都在块内析构），没有自动化断言。）
- `queues` map（key = `Self::key`）；`handle_message:293` running 命中入队；`drain_queue` 锁外取队首、锁内跑轮；cap = 20 + drop:summarize。
- 验收：入队 / 排水 / FIFO 顺序 / cap 超限合并 / 锁序不死锁，各一测。

**PR-5 命令扩展（`Queue` / `Tasks` / 菜单）** ✅ 已落地（5 测：多待批裸数字不是命令 / 字母菜单四映射 / `/queue` 两模式且未知模式不吃掉 / 帮助含新增命令 / `first_line` 跳过空行）

**PR-6 steer（独立设计步）** ✅ 已落地（`steer_buffer 按会话隔离` 1 测 + `/queue steer` 解析 1 测）

实际做法比本文早先设想的简单得多——**没有"唯一需要现场确认的接线点"**：`pending_steer` 虽是 `agent_loop` 模块私有字段，但在 `AgentLoop` 上加一个 3 行公开访问器 `steer_buffer()` 就能交出句柄，而它本来就是 `Arc`（agent 侧 drain 的和外部写的是同一块）。因此也**不必**把整个 `AgentLoop` 存进 service（那会要求 `AgentLoop: Send + Sync`，还把引擎生命周期挂到服务上）。
- 中断控制器与 steer 缓冲生命周期一致，两处登记/注销合并为 `register_active` / `unregister_active`——分开两份注册表必然漂移出一半。
- `interrupt_session` 命中后**顺手摘掉 steer 缓冲**：否则用户 steer 的文本会静静躺着永不落地（比报错更糟的静默丢失）。
- `/queue steer` 用独立变体 `Command::Steer(String)` 而非 `QueueMode::Steer`：steer **不排队**，它改的是当下这一轮。
- 测了**按会话隔离**：给 A 的指令不得进 B 的轮次——"你只管这个文件"被另一个会话的 agent 当指令执行是数据泄漏，不是 bug。

**PR-7 主轮次终态记录 + Resume 带上下文续跑** ✅ 已落地（2026-10-04 定：**先补终态记录**，不用启发式）

- **为什么不先用启发式**：主对话轮次只把开始落库、结束不落库，所以「最后一条 user 之后没有 assistant」永远带一个已知误报源（首次落盘前重启与真中断同形）。既然要动就动到根因。
- **复用既有 sink，不新建表 / 新 trait**：`TaskRecordSink` 已在生产接线（`state.rs:664`），`started` 本就带 `conversation_id`。主轮次用 `turn-{sid}-{nanos}` 前缀与 sub-agent 记录区隔；前缀是**跨 crate 契约**，放 `hermix_core::traits::TURN_ID_PREFIX`（hermix-agent 造 id、hermix-server 过滤 id）——`hermix-agent` 不能依赖 `hermix_task`，读取侧因此落在 hermix-server。
- 三态映射：成功 → `Completed`、用户叫停 → `Aborted`、失败 → `Faulted`。**终态必须在 `let result = result?` 之前落**，否则 Err 提前 return，失败 / 中断的轮次永远停在 `Running`，会被误判成「进程崩了」——正好把 PR-7 存在的意义抹掉。
- 判据 `task_record_sink::last_turn_unfinished`：`TURN_ID_PREFIX` + **非终态**（用 `TaskState::outcome().is_none()`，不是 `is_terminal`——那方法挂在 `TaskOutcome` 上且恒为 true）。用户按 `/stop` 落的是 `Terminal(Aborted)`，所以**主动叫停不算未跑完**，否则会一直被提示「接着来」。
- **提示挂 `/tasks`，不做启动时主动推送**（与本文早先写的「网关启动尾 `check_last_turn_outcome` 逐个 platform/user 发提示」不同）：启动时用户多半不在，只会收到一条无人认领的消息；且枚举「所有配对过的 IM 身份」要另写一遍。用户主动问「现在什么情况」时才给，正是最容易忽略中断的时刻。
- **Resume 不做盲重放**：`/继续` 取回上一条 user 原文后加「请**先检查现状**再决定怎么做，不要假定任何操作都没做过」，原样重发会让上一轮已完成的副作用执行两次。`dedup_key` 必须重新生成，否则会被去重表当成重投丢掉。

**每 PR 门禁**：`cargo check --workspace --all-targets` → `cargo test -p hermix-agent -p hermix-server --lib` → `./scripts/ci-gate.sh`；不引入 `TODO: pre-existing` / `FIXME: pre-existing`；不碰交叉编译禁区。

**被拍板项阻塞的部分**：对接层全部（鉴权复用 / 两 handler / relay 连接器 / 协议映射）← 待拍板 1；屏 3 三键或两键 ← 待拍板 2；relay 实现 ← 待拍板 3。（待拍板 4 已于 2026-10-04 定为「不做直连降级」，不再是待决项。）

对接层 2-3 个 PR（鉴权复用 / 两 handler / 连接器+映射）；relay 1-2 个；App 每屏 1 个。

## Alternatives considered

- **fork Clawket 改造**（曾为首选）：全开源、双端已上架、多后端协议包现成。弃因：引入 Expo/RN 全新栈的维护面；relay 是 Cloudflare Worker 第三方托管；fork 上游合并成本随其活跃迭代（267 commits / 345★）持续存在。
- **fork dshmobile**：安卓端**闭源**（仅签名 APK），插件 / 协议 / relay 开源但手机端不可改。直接排除——这也是放弃「改开源 App」路线的关键证据。
- **Tailscale 直连（零部署）**：曾推荐为默认。弃因：用户拍板不做直连优先。**但「唯一通道」是否排除降级路径仍未决**（§待拍板 4）——拒绝直连优先 ≠ 必须没有直连降级。
- **ClawApp（H5）路线**：免装 App 最快，但协议层绑 OpenClaw Gateway（Ed25519 握手 / 18789），换血≈重写；H5 无后台保活，不满足任务型场景。
- **E2EE（端到端加密）**：一期不做，relay 明文转发会话内容；二期采纳 dshmobile `docs/plan-e2ee.md` 的威胁模型与握手设计。触发条件：relay 部署到不可信主机或多用户共用时。
- **QQ 按键消息 / 群聊**：IM 通道增强，独立后续，不在本篇范围。
- **relay 用 Rust 而非 Node**：可复用现有工具链与 CI，代价是给用户多一条 Linux 构建链（与「Windows MSVC only、不交叉编译」standing order 摩擦）。**弃用 Node 的唯一理由是可从任意主机直接 `node apps/relay/` 启动**；若该理由不成立，Rust 版（axum + ws，零业务逻辑的路由器约 300 行）更符合本仓库的依赖阶梯。
- **维持一篇不拆 P1**：否决。README 要求一篇一决策；且 P1 落地后无法把这篇翻进 `implemented/`——翻了等于宣称手机端存在，而实际只交付了队列与中断。**已拆**：P1 独立为 [IM 远程任务生命周期（implemented）](../../implemented/feature/2026-10-04-im-turn-lifecycle.md)，本篇只留手机端与 relay。

## Consequences

What we give up:

- **relay 永久运维责任**：自建 relay 是新增长期组件（部署 / 升级 / 监控）。relay 宕机 = 手机端不可用，**包括局域网内**；IM / QQ 通道不受影响，仍是备份入口。
- **安卓-only 一期**：iOS 需另行立项（自建 Kotlin 意味着 iOS 要再写一遍）——接受，因一期用户场景是安卓。
- **App 与服务端协议耦合**：wire format 自定义意味着两端同步演进；以单一真相源（`relay_proto.rs`）+ `proto_version` 缓解，放弃「协议独立版本化仓库」的重装备。
- **PC 必须在线**：手机是薄客户端，电脑关机则手机端整体不可用（与 OpenClaw Android 一致；Dots 式云端常驻不在范围）。
- **一期会话内容明文经过 relay**：已写进信任模型，但它是**代价而非细节**——relay 被攻破即会话内容泄露（token 哈希与路由元数据不构成内容保护）。
- **`/stop` 是对已上线 QQ / 企微用户的可见行为变更**（已随 P1 落地，详见 implemented 篇）：从「波及本机所有会话」改为「只中断发起者会话」。这是修正既有隐患（`session.rs:365-369` 记录了「一个闲着的 IM 用户发 /stop 能掐掉本机正在跑的会话」），但属用户可感知行为，须在 release note 说明。
- **两套新供应链**：一个提案引入 npm（relay）与 Gradle（App）两套构建与锁文件，而仓库源码基线已 ≈14 万行、长期目标 10 万行（AGENTS.md）。缓解手段是范围锁死 + PR 切分 + CI 门禁，不是「以后再说」。
- **测试矩阵扩大**：四端（PC / relay / App / IM）组合行为；以冒烟脚本 + 四屏范围锁死 + App 单元测试控制。

## 待拍板

**1、2、4、5、6 已定（2026-10-04），仅 3 是外部依赖。** 未决前不得开工 App 与对接层。

1. **（已定）App 的执行路径 = 选项 A：合成 `ImPort`**
   - 决策：App 以合成 `ImPort` 身份进 `handle_message`（platform = `device`，user = device_id）→ P1 全部复用，sid = `im:device:{device_id}`。
   - 决定性理由：「永久允许」只能在不过能力矩阵的 channel 侧实现（§多端隔离 能力位的硬边界）。选项 B（独立 mobile 通道）语义更干净，但要另做一套队列 / 心跳 / 中断 / 任务态，等于把刚落地的 P1 重写一遍——为一个按钮不值得。
   - 随之而来的实现要求：「设备」被当作平台看待，所以 `IM_SOURCE_LABEL`、`imSourceOf`、侧栏标签、审批数字闸门都要认识 `device` 这个 platform（`utils/time.ts:63` 目前只认 `qq` / `wecom`）。
2. **（已定）手机不授「永久允许」：选 ②**
   - 决策：屏 3 砍成「允许一次 / 拒绝」两键，持久放行走桌面。
   - 理由：与 `2026-09-30:85-87` 的结论一致（远程端不授予持久权限），也避免为一个按钮去动 `origin.rs:65-72` 那条有单测守着的安全矩阵。手动点与语音转写风险不同，但结论相同。
   - 连带：`mobile_protocol.rs` 的审批三态里，App 侧只暴露 deny / once 两态。
3. **relay 实例从哪来**（阻塞 relay 与 App 配对）—— **外部依赖，本篇不自行决定**
   - 问题：自建 relay 意味着用户要先有一台公网主机 + TLS 证书 + 常驻进程。**对「零安装便携」产品，这是采纳门槛，比宕机风险更致命**，而本篇未交代。
   - 选项：① 用户自备 VPS（需给出部署脚本 / 证书续期说明）；② 项目方提供公共 relay（则「数据自主」这个换来的好处消失，且变成运营负担）；③ 复用 dshmobile 已有的部署方式（**本篇未复核，见参考项目表**）。
   - **需要 dshmobile 的 relay 文档才能判**。若它有一键部署，本条降级为「抄部署脚本」；若没有，① 就要额外产出一套可复制的部署物。这项不靠推理能定，本篇标为外部依赖而不编答案。
4. **（已定 2026-10-04）relay 不做直连降级**
   - 决策：选「严格唯一通道」。relay 不可用即手机端不可用，**没有第二条路**，家里 LAN 上也一样。
   - 否掉直连降级的理由记录在案：「App 双模式增加测试矩阵」这个反对意见其实不成立——App 已经有可配的 relay 地址，直连就是同一客户端换 base URL，矩阵成本≈0。真正压过它的是「不要两条链路、两套心智模型」的取向，不是成本。
   - 随之而来的硬约束（实现时必须遵守）：**协议层保持 base-URL 可替换**，不许把 relay 的路由假设写死进客户端——WS URL 拼接、鉴权头、relay 特有帧类型都不得散落进业务代码。这不是给反悔留后门，而是保证若将来要改，成本是改配置而不是重写客户端。
   - 后果已计入 §Consequences：单点且无退路，因此第 3 项从「上手摩擦」升级为「生存前提」——用户拿不到 relay 就等于没有手机端。
5. **（已定）P1 拆篇：选 ①**
   - 已执行：P1 独立为 [IM 远程任务生命周期（implemented）](../../implemented/feature/2026-10-04-im-turn-lifecycle.md)，本篇只留手机端与 relay。README「一篇一决策」与「当场检查是否取代旧笔记」都指向拆。
6. **（已定）认定 2026-09-29 的远程第 1 层已被取代：选 ②**
   - `2026-09-29-stability-and-remote-plan.md:26/293/336` 把远程定为「三层递进，第 1 层几乎免费」，第 1 层是「Dashboard 移动审批 + token 认证，~2 天，⬜ 未做」。
   - 决策：本篇的 P1 确实覆盖了第 1 层所依赖的地基（IM 远程审批 + 队列 + 中断 + 任务可见性），故认定第 1 层被取代。**该篇对应的「⬜ 未做」行应在下次改到它时一并更新**，否则后来人会照着「未做」重复排期。
