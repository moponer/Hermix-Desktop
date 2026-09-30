//! IM 通道的调度层：谁能说话、说什么、跑哪条会话、审批怎么中继。
//!
//! 平台无关。所有平台差异（怎么发、锚在哪）都关在 `ImPort` 后面。
//!
//! 与 cc-haha 的关键差别：那里 adapter 是**进程外**的，要自带 WS 桥、
//! 令牌、会话映射 JSON 文件；这里跑在 `hermix-server` 内部，直接拿
//! `HttpServerState`，所以那整套桥接全部不存在。会话 id 由平台 + 身份
//! 确定性地推导（`im:{platform}:{user}`），也不需要映射文件——重启后接着
//! 同一条会话。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::sync::mpsc;

use hermix_core::approval::ApprovalOutcome;
use hermix_transport::ApprovalChoice;

use crate::pairing::{AuditEvent, ConsumeOutcome};
use crate::state::HttpServerState;

use super::port::{ImPort, Inbound};

/// 同一个平台消息 id 重复投递的消息在多久内视为同一条。网关重连补发时会
/// 重放，Agent 不该把同一句话跑两遍。
const DEDUP_TTL: Duration = Duration::from_secs(600);

/// 配对成功的 IM 账号在设备表里的名字。复用 `paired_devices` 表，于是它
/// 自动获得审计日志、`chat` scope，以及桌面端「设备」页里的撤销按钮。
fn device_name(platform: &str, user: &str) -> String {
    format!("{platform}:{user}")
}

fn session_id(platform: &str, user: &str) -> String {
    format!("im:{platform}:{user}")
}

// ---------------------------------------------------------------------------
// 入站去重
// ---------------------------------------------------------------------------

/// 按平台消息 id 去重。就是一个带 TTL 的 HashMap——cc-haha 给它写了个定时
/// 清扫器和容量上限，那是给 JS 单线程事件循环补的洞；这里每条消息都自己
/// 触发一次惰性清理。
struct Dedup {
    seen: Mutex<HashMap<String, Instant>>,
}

impl Dedup {
    fn new() -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// 返回 true 表示这是第一次见到这条消息。
    fn first_sighting(&self, key: &str) -> bool {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        seen.retain(|_, at| now.duration_since(*at) < DEDUP_TTL);
        if seen.contains_key(key) {
            return false;
        }
        seen.insert(key.to_string(), now);
        true
    }
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum Command {
    Help,
    Status,
    Clear,
    Stop,
    /// `1` 允许一次 / `2` 本会话内永久允许 / `3` 拒绝。
    Permission(usize),
}

/// 解析一条入站文本是不是命令。
///
/// 裸数字只有在**恰好有一个**待批请求时才当成审批回复——否则它和「选第
/// 1 个」这类语义会撞车。这条闸门是从 cc-haha 抄的（`permission.ts`）。
fn parse_command(text: &str, pending: usize) -> Option<Command> {
    let t = text.trim();
    match t.to_ascii_lowercase().as_str() {
        "/help" | "帮助" => return Some(Command::Help),
        "/status" | "状态" => return Some(Command::Status),
        "/clear" | "清空" => return Some(Command::Clear),
        "/stop" | "停止" => return Some(Command::Stop),
        _ => {}
    }
    if pending == 1 {
        match t {
            "1" | "/1" | "y" | "yes" | "允许" | "同意" => return Some(Command::Permission(1)),
            "2" | "/2" | "always" | "永久允许" => return Some(Command::Permission(2)),
            "3" | "/3" | "n" | "no" | "拒绝" => return Some(Command::Permission(3)),
            _ => {}
        }
    }
    None
}

const HELP_TEXT: &str = "\
可用命令：
/help      列出本帮助
/status    查看当前会话与运行状态
/clear     清空当前会话上下文
/stop      中断本机正在跑的任务（会波及本机所有会话）

Agent 要写文件或执行高风险命令时，会在这里发一条审批提示。
届时直接回复 1 允许一次、2 本会话内永久允许、3 拒绝。";


/// pump task 收到的东西。`on_chunk` 是同步的，正文和工具事件都要从这里
/// 排队给负责发消息的那个 task。
enum StreamMsg {
    Text(String),
    /// 工具名 + 是否已完成。
    Tool(String, bool),
}

/// 一轮里已经发生过的工具调用，渲染成正文上方的一行进度。
///
/// 抄 cc-haha 飞书流式卡片（`adapters/feishu/streaming-card.ts`）的做法：
/// `⚙️` 表示正在跑、`✅` 表示已结束，中间用 `·` 连起来。那份实现特意避开了
/// markdown 列表/引用/标题，因为那些在飞书里会触发 `<br>` 注入和标题降级；
/// 我们没有那个坑，但结论一样——这一行越朴素越好，它的职责是「证明还活着」。
#[derive(Default)]
struct ToolTrace {
    /// `(工具名, 是否已完成)`，按首次出现顺序。
    steps: Vec<(String, bool)>,
}

impl ToolTrace {
    fn push(&mut self, name: Option<&str>, done: bool) {
        let name = name.unwrap_or("tool").to_string();
        match self.steps.iter_mut().find(|(n, _)| *n == name) {
            Some(step) => step.1 = done,
            None => self.steps.push((name, done)),
        }
    }

    /// 正文又开始流了，说明模型已经拿到工具结果、正在写话。此时把已经跑完
    /// 的工具从进度行里撤掉，否则一行五个 ✅ 会把真正的回答挤下去。只要还有
    /// 工具在跑就保留——用户需要知道活儿还没干完。
    fn clear_when_idle(&mut self) {
        if self.steps.iter().all(|(_, done)| *done) {
            self.steps.clear();
        }
    }

    fn render(&self) -> String {
        if self.steps.is_empty() {
            return String::new();
        }
        let parts: Vec<String> = self
            .steps
            .iter()
            .map(|(name, done)| {
                if *done {
                    format!("✅ {name}")
                } else {
                    format!("⚙️ {name}")
                }
            })
            .collect();
        format!("🛠️ {}", parts.join(" · "))
    }
}

#[derive(Debug, Clone)]
struct PendingApproval {
    request_id: String,
    tool: String,
    arguments: Value,
}

// ---------------------------------------------------------------------------
// 运行时
// ---------------------------------------------------------------------------

pub struct ChannelRuntime {
    state: HttpServerState,
    /// 平台名 → 端口。
    ports: HashMap<&'static str, Arc<dyn ImPort>>,
    /// 平台名 → 免配对直通的 userid。
    allowed: HashMap<&'static str, HashSet<String>>,
    /// 每个 `<平台>:<user>` 一把锁，天然把同一段私聊串行化。不同会话之间
    /// 完全并行——所以这里不需要 cc-haha 那套 promise 队列。
    chat_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 待批审批：`平台:user -> 队列`。包一层 Arc 是因为 `on_event` 是
    /// `'static` 闭包，只能持所有权，不能借用 `&self`。
    pending: Arc<Mutex<HashMap<String, Vec<PendingApproval>>>>,
    running: Mutex<HashSet<String>>,
    dedup: Dedup,
}

impl ChannelRuntime {
    pub fn new(
        state: HttpServerState,
        ports: Vec<Arc<dyn ImPort>>,
        allowed: HashMap<&'static str, Vec<String>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            state,
            ports: ports
                .into_iter()
                .map(|p| (p.platform(), p))
                .collect(),
            allowed: allowed
                .into_iter()
                .map(|(k, v)| (k, v.into_iter().collect()))
                .collect(),
            chat_locks: Mutex::new(HashMap::new()),
            pending: Arc::new(Mutex::new(HashMap::new())),
            running: Mutex::new(HashSet::new()),
            dedup: Dedup::new(),
        })
    }

    /// 已启用的平台名。启动日志和端点返回用。
    pub fn platforms(&self) -> Vec<&'static str> {
        self.ports.keys().copied().collect()
    }

    fn key(platform: &str, user: &str) -> String {
        format!("{platform}:{user}")
    }

    // ---------------- 授权 ----------------

    /// 这个身份是否完成过配对。
    fn is_paired(&self, platform: &str, user: &str) -> bool {
        let name = device_name(platform, user);
        self.state
            .device_registry
            .as_ref()
            .map(|r| {
                r.list()
                    .iter()
                    .any(|d| d.device_name == name && d.revoked_at.is_none())
            })
            .unwrap_or(false)
    }

    /// 白名单 ∪ 已配对。两者都空 = 一个人都进不来（默认拒绝）。
    fn is_authorized(&self, platform: &str, user: &str) -> bool {
        self.allowed
            .get(platform)
            .map(|s| s.contains(user))
            .unwrap_or(false)
            || self.is_paired(platform, user)
    }

    // ---------------- 入站 ----------------

    /// 处理一条入站消息。整个过程在一个 task 里跑，网关循环不受影响。
    pub async fn handle_inbound(self: &Arc<Self>, platform: &'static str, msg: Inbound) {
        if msg.text.trim().is_empty() {
            return;
        }
        let dedup_key = format!("{platform}:{}", msg.dedup_key);
        if !self.dedup.first_sighting(&dedup_key) {
            tracing::debug!(platform, "重复投递，已忽略");
            return;
        }
        let Some(port) = self.ports.get(platform).cloned() else {
            return;
        };

        let user = msg.user.clone();
        let anchor = msg.anchor.clone();
        if !self.is_authorized(platform, &user) {
            let verdict = self.try_pair(platform, &msg).await;
            let _ = port.send_text(&user, &verdict, &anchor).await;
            return;
        }

        // 审批回复和 /stop 必须在拿聊天锁**之前**处理：正在跑的那一轮正
        // 攥着这把锁等审批，先拿锁就死锁了。
        if let Some(cmd) = parse_command(&msg.text, self.pending_count(platform, &user)) {
            if self.try_handle_immediate(&port, platform, &user, &anchor, cmd).await {
                return;
            }
        }

        let key = Self::key(platform, &user);
        let lock = {
            let mut locks = self.chat_locks.lock().unwrap_or_else(|e| e.into_inner());
            locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _guard = lock.lock().await;

        if let Some(cmd) = parse_command(&msg.text, self.pending_count(platform, &user)) {
            if self
                .try_handle_locked(&port, platform, &user, &anchor, cmd)
                .await
            {
                return;
            }
        }
        self.run_turn(&port, platform, &msg).await;
    }

    /// 处理未授权者的配对尝试。未配对的人发什么都当配对码试一次——这正是
    /// 错误预算要防的事。
    async fn try_pair(&self, platform: &str, msg: &Inbound) -> String {
        // 失败预算记在这个 IM 身份的桶里。共用浏览器那个桶的话，一个在 IM
        // 里乱发消息的人就能把桌面端扫码配对一起锁死。
        let bucket = format!("im:{}:{}", platform, msg.user);
        match self.state.pairing_sessions.consume_for(msg.text.trim(), &bucket) {
            ConsumeOutcome::OkCode => {
                let Some(registry) = self.state.device_registry.as_ref() else {
                    return "配对失败：设备注册表未就绪。请重启桌面端后重试。".to_string();
                };
                let name = device_name(platform, &msg.user);
                match registry.pair(&name) {
                    Ok((device, _token)) => {
                        registry.record_audit(
                            AuditEvent::Pairing,
                            Some(device.device_id.clone()),
                            None,
                            Some(format!("im:{platform}")),
                        );
                        format!(
                            "配对成功（{}）。\n现在可以直接发消息了，命令用 /help 查看。",
                            device.device_id
                        )
                    }
                    Err(e) => format!("配对失败：{e}"),
                }
            }
            ConsumeOutcome::Locked => {
                "配对码错误次数过多，已锁定。请在桌面端重新生成一枚新码。".to_string()
            }
            ConsumeOutcome::Invalid => {
                "未授权。请先在桌面端生成配对码，然后把 6 位码发给我。".to_string()
            }
            ConsumeOutcome::Ok(_) => {
                "这是一枚网页扫码用的配对码，请在浏览器里打开 /pair 页面使用。".to_string()
            }
        }
    }

    /// 不需要聊天锁就能完成的命令。返回 true 表示消息已被消费。
    async fn try_handle_immediate(
        &self,
        port: &Arc<dyn ImPort>,
        platform: &str,
        user: &str,
        anchor: &str,
        cmd: Command,
    ) -> bool {
        match cmd {
            Command::Stop => {
                // 只在**这个人自己**确实有轮子在跑时才动手：否则一个闲着
                // 的 IM 用户发一条 /stop 就能掐掉桌面端正在跑的会话。
                // 注意 AgentService 只提供 `interrupt_all()`，没有按会话
                // 中断的接口，所以真要中断时波及的是本机所有在跑的轮次
                // （它们会各自把已产生的部分落盘，不会丢数据）。
                let busy = self
                    .running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&Self::key(platform, user));
                if busy {
                    self.state.agent_service().interrupt_all();
                    let _ = port
                        .send_text(user, "已请求中断本机正在运行的任务。", anchor)
                        .await;
                } else {
                    let _ = port.send_text(user, "当前没有在运行的任务。", anchor).await;
                }
                true
            }
            Command::Permission(_) => {
                self.resolve_permission(port, platform, user, anchor, cmd).await;
                true
            }
            _ => false,
        }
    }

    /// 需要先拿到聊天锁的命令（会等当前轮跑完）。
    async fn try_handle_locked(
        &self,
        port: &Arc<dyn ImPort>,
        platform: &str,
        user: &str,
        anchor: &str,
        cmd: Command,
    ) -> bool {
        let sid = session_id(platform, user);
        let text = match cmd {
            Command::Help => HELP_TEXT.to_string(),
            Command::Status => {
                let busy = self
                    .running
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&Self::key(platform, user));
                format!(
                    "平台：{platform}\n会话：{sid}\n状态：{}",
                    if busy { "运行中" } else { "空闲" }
                )
            }
            Command::Clear => {
                match self.state.agent_service().reset_session(&sid).await {
                    Ok(()) => "已清空上下文。".to_string(),
                    Err(e) => format!("清空失败：{e}"),
                }
            }
            _ => return false,
        };
        let _ = port.send_text(user, &text, anchor).await;
        true
    }

    fn pending_count(&self, platform: &str, user: &str) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&Self::key(platform, user))
            .map(Vec::len)
            .unwrap_or(0)
    }

    // ---------------- 审批中继 ----------------

    /// 用户回复了 1/2/3。顺序照抄 `routes::approve_via_rest`：先 resolve
    /// 注册表，再落库，最后**只在真的匹配上**时才写放行副作用——无条件重放
    /// 会让同一个工具被追加两次 allowlist 条目。
    async fn resolve_permission(
        &self,
        port: &Arc<dyn ImPort>,
        platform: &str,
        user: &str,
        anchor: &str,
        cmd: Command,
    ) {
        let Command::Permission(n) = cmd else {
            return;
        };
        let (choice, outcome, label) = match n {
            1 => (ApprovalChoice::Once, ApprovalOutcome::Approved, "✅ 已允许一次"),
            2 => (
                ApprovalChoice::Session,
                ApprovalOutcome::Approved,
                "♾️ 本会话内永久允许",
            ),
            _ => (ApprovalChoice::Deny, ApprovalOutcome::DeniedByUser, "❌ 已拒绝"),
        };

        let key = Self::key(platform, user);
        let pending = {
            let mut map = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            let Some(list) = map.get_mut(&key) else {
                return;
            };
            // 只在唯一待批时才消费队列——多于一个时用户回「1」是有歧义的，
            // 那种情况在 parse_command 那层就已经挡掉了。
            if list.len() != 1 {
                return;
            }
            list.remove(0)
        };

        let sid = session_id(platform, user);
        let matched = self
            .state
            .approval_registry
            .resolve(&sid, &pending.request_id, outcome);
        if let Err(e) = self.state.session_persistence.resolve_approval_message(
            &sid,
            &pending.request_id,
            choice_label_for_db(&choice),
        ) {
            tracing::warn!(error = %e, request_id = %pending.request_id, "审批决定落库失败");
        }
        if matched {
            crate::util::record_approval_side_effect(
                &self.state,
                &sid,
                &pending.tool,
                &pending.arguments,
                choice,
            );
        }
        let _ = port.send_text(user, label, anchor).await;
    }

    // ---------------- 跑一轮 ----------------

    async fn run_turn(&self, port: &Arc<dyn ImPort>, platform: &'static str, msg: &Inbound) {
        let sid = session_id(platform, &msg.user);
        let mut stream = port.start_stream(&msg.user, &msg.anchor);
        let (tx, mut rx) = mpsc::unbounded_channel::<StreamMsg>();

        // `on_chunk` 是同步闭包（Agent 在 tokio worker 上调它），不能 await。
        // 所以它只做「剥壳 + 脱敏 + 塞 channel」，真正发 HTTP 的活儿交给
        // 下面的 pump task。这也顺带把节流放在了一个干净的循环里。
        let think = Mutex::new(hermix_agent::python_alignment::ThinkBlockParser::new());
        let sanitizer = Mutex::new(hermix_core::ToolCallSanitizer::new());
        let on_chunk = Arc::new(move |chunk: hermix_core::types::StreamChunk| {
            let Some(delta) = chunk.delta.as_ref() else {
                return;
            };
            // 工具生命周期事件走 delta.extra（ws.rs 消费的是同一份结构：
            // `{"event":"tool_start"|"tool_complete", "tool":…}`）。IM 用户看
            // 不到桌面端的工具面板，不播报就是几十秒的沉默。
            if let Some(extra) = &delta.extra {
                if let Some(ev) = extra.get("event").and_then(Value::as_str) {
                    let done = match ev {
                        "tool_start" => Some(false),
                        "tool_complete" => Some(true),
                        _ => None,
                    };
                    if let Some(done) = done {
                        let name = extra
                            .get("tool")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string();
                        let _ = tx.send(StreamMsg::Tool(name, done));
                        return;
                    }
                }
            }
            let Some(text) = delta.content.as_ref() else {
                return;
            };
            if text.is_empty() {
                return;
            }
            let (clean, _reasoning) = think
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .feed(text);
            if clean.is_empty() {
                return;
            }
            let cleaned = sanitizer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .feed(&clean);
            if cleaned.is_empty() {
                return;
            }
            let _ = tx.send(StreamMsg::Text(crate::util::redact_sensitive(&cleaned)));
        });

        let pump = tokio::spawn(async move {
            let mut tools = ToolTrace::default();
            let mut header_shown = String::new();
            while let Some(msg) = rx.recv().await {
                match msg {
                    StreamMsg::Text(delta) => {
                        tools.clear_when_idle();
                        stream.append(&delta).await;
                    }
                    StreamMsg::Tool(name, done) => {
                        tools.push(Some(&name), done);
                        let next = tools.render();
                        // 轨迹没变就不重发，省掉一整轮 HTTP。
                        if next != header_shown {
                            header_shown = next;
                            stream.set_header(&header_shown).await;
                        }
                    }
                }
            }
            stream.finish().await;
        });

        let overrides = self.build_overrides(port, platform, &msg.user, &sid);
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.insert(Self::key(platform, &msg.user));
        }
        let result = self
            .state
            .agent_service()
            .send_message_stream(&sid, &msg.text, overrides, on_chunk)
            .await;
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.remove(&Self::key(platform, &msg.user));
        }
        // `overrides`（连同 on_chunk 里的 tx）已随 send_message_stream 一起被
        // 消费掉，这里 future 一返回 tx 就析构，pump 的 recv 拿到 None，自行
        // 收尾。只需等它把最后一帧发完。
        pump.await.ok();

        if let Err(e) = result {
            let (user_msg, _) = crate::util::user_facing_agent_error(&e.to_string());
            let _ = port
                .send_text(
                    &msg.user,
                    &format!("出错了：{user_msg}"),
                    &msg.anchor,
                )
                .await;
        }
    }

    fn build_overrides(
        &self,
        port: &Arc<dyn ImPort>,
        platform: &'static str,
        user: &str,
        sid: &str,
    ) -> hermix_core::traits::AgentOverrides {
        let mut overrides = hermix_core::traits::AgentOverrides::default();

        // 注册进审批表，桌面端「待审批」列表因此也能看到 IM 触发的请求。
        let registry = self.state.approval_registry.clone();
        overrides.approval_requester = Some(Arc::new(move |sess: String, req: String| {
            registry.request_approval(&sess, &req)
        }));

        // 桌面端点过的「本会话允许」不该在 IM 里再问一遍。
        let user_allowlist = self.state.user_allowlist.clone();
        let session_bypass = self.state.session_bypass.clone();
        let sid_owned = sid.to_string();
        overrides.pre_approval_checker = Some(Arc::new(
            move |tool: &str, params: &Value| -> bool {
                let Some(signature) = hermix_tools::extract_trust_signature(tool, params) else {
                    return false;
                };
                if let Ok(map) = session_bypass.lock() {
                    if let Some(patterns) = map.get(&sid_owned) {
                        if patterns.iter().any(|re| re.is_match(&signature)) {
                            return true;
                        }
                    }
                }
                if let Ok(guard) = user_allowlist.lock() {
                    if let Some(al) = guard.as_ref() {
                        return al.matches(tool, &signature);
                    }
                }
                false
            },
        ));

        // 把审批提示推回 IM。同步闭包里不能 await，所以 spawn 出去。
        let port = port.clone();
        let user = user.to_string();
        let pending = self.pending.clone();
        overrides.on_event = Some(Arc::new(move |extra: Value| {
            let Some(request_id) = extra.get("request_id").and_then(Value::as_str) else {
                return;
            };
            let request_id = request_id.to_string();
            let tool = extra
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("未知工具")
                .to_string();
            let arguments = extra.get("arguments").cloned().unwrap_or(Value::Null);
            let text = format_approval_prompt(&tool, &arguments, &request_id);

            {
                let mut map = pending.lock().unwrap_or_else(|e| e.into_inner());
                map.entry(ChannelRuntime::key(platform, &user))
                    .or_default()
                    .push(PendingApproval {
                        request_id: request_id.clone(),
                        tool: tool.clone(),
                        arguments: arguments.clone(),
                    });
            }
            // on_event 是 Fn 闭包（可能被调多次），所以每次都克隆一份
            // Arc 交给 spawn，不能把捕获的 port 直接 move 走。
            let port = port.clone();
            let user = user.clone();
            tokio::spawn(async move {
                if let Err(e) = port.send_text(&user, &text, "").await {
                    tracing::warn!(error = %e, "推送审批提示失败");
                }
            });
        }));

        overrides
    }
}

/// 落库用的决定字符串。
fn choice_label_for_db(choice: &ApprovalChoice) -> &'static str {
    match choice {
        ApprovalChoice::Deny => "denied",
        _ => "approved",
    }
}

fn format_approval_prompt(tool: &str, arguments: &Value, request_id: &str) -> String {
    let summary: String = arguments
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            let raw = arguments.to_string();
            if raw.len() > 300 {
                format!("{}…", &raw[..300])
            } else {
                raw
            }
        });
    format!(
        "🔐 需要权限确认 [{request_id}]\n工具：{tool}\n{summary}\n\n回复 1 允许一次，2 本会话内永久允许，3 拒绝。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 中文命令别名可用() {
        assert_eq!(parse_command("帮助", 0), Some(Command::Help));
        assert_eq!(parse_command("状态", 0), Some(Command::Status));
        assert_eq!(parse_command("清空", 0), Some(Command::Clear));
        assert_eq!(parse_command("停止", 0), Some(Command::Stop));
        assert_eq!(parse_command("/HELP", 0), Some(Command::Help));
    }

    #[test]
    fn 普通文本不是命令() {
        assert_eq!(parse_command("帮我看看 main.rs", 0), None);
        assert_eq!(parse_command("1", 0), None, "没有待批时裸数字是普通文本");
    }

    #[test]
    fn 恰好一个待批时裸数字才是审批() {
        assert_eq!(parse_command("1", 1), Some(Command::Permission(1)));
        assert_eq!(parse_command("2", 1), Some(Command::Permission(2)));
        assert_eq!(parse_command("拒绝", 1), Some(Command::Permission(3)));
    }

    #[test]
    fn 多个待批时裸数字不歧义() {
        // 两个待批时「1」没有唯一指向，宁可当普通文本也不猜。
        assert_eq!(parse_command("1", 2), None);
    }

    #[test]
    fn 命名按平台隔离() {
        assert_eq!(session_id("qq", "abc"), "im:qq:abc");
        assert_eq!(session_id("wecom", "u1"), "im:wecom:u1");
        assert_eq!(device_name("qq", "abc"), "qq:abc");
        assert_eq!(ChannelRuntime::key("wecom", "u1"), "wecom:u1");
    }

    #[test]
    fn 去重只放行第一次() {
        let d = Dedup::new();
        assert!(d.first_sighting("qq:m1"));
        assert!(!d.first_sighting("qq:m1"));
        assert!(d.first_sighting("wecom:m1"), "不同平台不共享去重键");
    }

    #[test]
    fn 审批提示带上工具与截断参数() {
        let args = serde_json::json!({ "reason": "要改配置" });
        let s = format_approval_prompt("write_file", &args, "rid-1");
        assert!(s.contains("write_file"));
        assert!(s.contains("rid-1"));
        assert!(s.contains("要改配置"));
    }

    #[test]
    fn 审批提示对巨大参数做截断() {
        let args = serde_json::json!({ "blob": "x".repeat(2000) });
        let s = format_approval_prompt("terminal", &args, "rid-2");
        assert!(s.len() < 600, "不该把整段参数原样塞进一条 IM 消息");
    }

    #[test]
    fn 拒绝落库标签正确() {
        assert_eq!(choice_label_for_db(&ApprovalChoice::Deny), "denied");
        assert_eq!(choice_label_for_db(&ApprovalChoice::Once), "approved");
        assert_eq!(choice_label_for_db(&ApprovalChoice::Session), "approved");
    }

    #[test]
    fn 工具轨迹按首次出现排序() {
        let mut t = ToolTrace::default();
        t.push(Some("Read"), true);
        t.push(Some("Bash"), false);
        t.push(Some("Read"), true);
        assert_eq!(t.render(), "🛠️ ✅ Read · ⚙️ Bash");
    }

    #[test]
    fn 同一工具再次触发会更新状态而不是重复出现() {
        let mut t = ToolTrace::default();
        t.push(Some("Bash"), false);
        t.push(Some("Bash"), true);
        assert_eq!(t.render(), "🛠️ ✅ Bash");
    }

    #[test]
    fn 缺工具名时有兜底() {
        let mut t = ToolTrace::default();
        t.push(None, false);
        assert_eq!(t.render(), "🛠️ ⚙️ tool");
    }

    #[test]
    fn 正文开始后清掉已完成的轨迹() {
        let mut t = ToolTrace::default();
        t.push(Some("Read"), true);
        t.clear_when_idle();
        assert_eq!(t.render(), "", "工具跑完、正文开写，进度行就该让位");
    }

    #[test]
    fn 有工具还在跑时正文不清轨迹() {
        let mut t = ToolTrace::default();
        t.push(Some("Bash"), false);
        t.clear_when_idle();
        assert_eq!(t.render(), "🛠️ ⚙️ Bash");
    }
}
