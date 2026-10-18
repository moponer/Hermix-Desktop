//! IM 通道的调度层：谁能说话、说什么、跑哪条会话、审批怎么中继。
//!
//! 平台无关。所有平台差异（怎么发、锚在哪）都关在 `ImPort` 后面。
//!
//! 与 cc-haha 的关键差别：那里 adapter 是**进程外**的，要自带 WS 桥、
//! 令牌、会话映射 JSON 文件；这里跑在 `hermix-server` 内部，直接拿
//! `HttpServerState`，所以那整套桥接全部不存在。会话 id 由平台 + 身份
//! 确定性地推导（`im:{platform}:{user}`），也不需要映射文件——重启后接着
//! 同一条会话。

use std::collections::{HashMap, HashSet, VecDeque};
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

/// 取第一条非空行（按字符截断），用于列表里一眼认出这条是哪件事。
///
/// 跳过开头的空行：用户消息常带一个换行，`lines().next()` 那种写法会
/// 返回空串，列表里就是一排没有内容的行。
fn first_line(text: &str) -> String {
    let head = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    crate::util::truncate_chars(head, 40)
}

/// 会话 id 的**唯一**格式定义：`im:{platform}:{user}`。
///
/// `pub(crate)` 是因为手机端的归属闸门要拼同一条 id（`im:device:{device_id}`），
/// 那边**必须调这个函数**而不是自己拼字符串——手拼过一次，漏了 `im:` 前缀，
/// 结果归属闸门永远匹配不上，手机对自己的会话也 403。
pub(crate) fn session_id(platform: &str, user: &str) -> String {
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

/// 跑够这么久才算「长任务」。两处用同一个阈值：收尾 ✅ 回执的门槛，
/// 以及 PR-3 心跳的第一次跳动。短于此的一轮用户全程盯着屏幕，不需要回执。
const LONG_TURN: std::time::Duration = std::time::Duration::from_secs(60);

/// 心跳前 N 次用 `LONG_TURN` 间隔，之后转 `HEARTBEAT_SLOW`。
const HEARTBEAT_FAST_BEATS: u32 = 30;

/// 心跳降频后的间隔。降频是为了不给长任务刷屏，但**不许静默**——
/// 用户看到的最后一条心跳可能比真实进度落后很多。
const HEARTBEAT_SLOW: std::time::Duration = std::time::Duration::from_secs(300);

/// 心跳 task 的绝对上限。这是**泄漏兜底**，不是产品决策：正常路径由
/// `run_turn` 里的 `abort()` 收尾，这里只防「`run_turn` 的 future 被 drop」
/// 那种情况下 task 变成野生的情形。
const HEARTBEAT_MAX_BEATS: u32 = 60;

/// 单个会话的排队上限。超了挤掉最旧的，并把被挤掉的记进 `SessionQueue::dropped`
/// 一起交给下一轮，不静默丢。20 条够一个人把想说的话说完，又不至于让队列
/// 长到跑一整夜。
const QUEUE_CAP: usize = 20;

/// 手机端在通道里的 platform 名。待拍板 1 定了 App 以合成 `ImPort` 身份进来，
/// 于是设备会话 id 恒为 `im:device:{device_id}`。
///
/// 「设备」当平台看待的代价（`IM_SOURCE_LABEL`、侧栏标签、审批数字闸门都要
/// 认识它）在笔记 §待拍板 1 里列了；这个常量是那份代价的锚点。
pub(crate) const DEVICE_PLATFORM: &str = "device";

/// 心跳的节奏。生产走 `Default`（模块常量），测试传毫秒级值。
#[derive(Clone, Copy)]
struct Heartbeat {
    fast: std::time::Duration,
    slow: std::time::Duration,
    fast_beats: u32,
    max_beats: u32,
}

impl Default for Heartbeat {
    fn default() -> Self {
        Self {
            fast: LONG_TURN,
            slow: HEARTBEAT_SLOW,
            fast_beats: HEARTBEAT_FAST_BEATS,
            max_beats: HEARTBEAT_MAX_BEATS,
        }
    }
}

/// 起一个长任务心跳 task：定期告诉用户"这一轮还活着，并且在忙什么"。
///
/// 退出条件三条，正常路径只用到第一条：
/// 1. `run_turn` 在 `send_message_stream` 返回后 `abort()`；
/// 2. 累计 `max_beats` 次后自行收工（泄漏兜底）；
/// 3. 从快到慢切换的那一刻**明确告知**一次，不静默拉长间隔。
///
/// 轨迹为空说明模型还在想、还没调工具——这时发一句"还在想"比发空的
/// `⏳ 🛠️` 诚实。
fn spawn_heartbeat(
    tools: std::sync::Arc<std::sync::Mutex<ToolTrace>>,
    port: Arc<dyn ImPort>,
    user: String,
    anchor: String,
    hb: Heartbeat,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        for beat in 0..hb.max_beats {
            let fast = beat < hb.fast_beats;
            tokio::time::sleep(if fast { hb.fast } else { hb.slow }).await;
            if beat == hb.fast_beats {
                let _ = port
                    .send_text(&user, "⏳ 任务仍在运行，心跳已降频。", &anchor)
                    .await;
            }
            let line = tools.lock().unwrap_or_else(|e| e.into_inner()).render();
            let text = if line.is_empty() {
                "⏳ 还在想…".to_string()
            } else {
                format!("⏳ {line}")
            };
            let _ = port.send_text(&user, &text, &anchor).await;
        }
    })
}

/// `/queue` 的处理方式。
#[derive(Debug, Clone, Copy, PartialEq)]
enum QueueMode {
    /// 报告当前排队情况（也是 `/queue` 无参时的默认）。
    Report,
    /// 中断在跑的那轮，把队首立刻当新一轮跑。
    Interrupt,
}

/// 单字母快捷菜单。**刻意不用 1-4**：审批的回复就是 1/2/3，两套语义靠
/// pending 计数切换正是 cc-haha 那条闸门要防的事。字母和数字不撞车，
/// 于是菜单任何时候都能用，不需要额外闸门。
#[derive(Debug, Clone, Copy, PartialEq)]
enum MenuAction {
    Status,
    Tasks,
    Stop,
    Clear,
}

// 不派生 `Copy`：`Steer` 带着指引正文，不是 Copy 类型。
#[derive(Debug, Clone, PartialEq)]
enum Command {
    Help,
    Status,
    Clear,
    Stop,
    /// `1` 允许一次 / `2` 本会话内永久允许 / `3` 拒绝。
    Permission(usize),
    Queue(QueueMode),
    /// `/继续`：把上一轮没跑完的任务接着跑。
    Resume,
    /// 往**正在跑**的这一轮注入一条指引。轮次不中止，指引落进下一个 tool
    /// result，用户可以在不丢掉已完成工作的前提下纠偏。之所以单独一个变体
    /// 而不是 `Queue` 的一个模式：steer 不排队，它改的是**当下**这一轮。
    Steer(String),
    Tasks,
    Menu(MenuAction),
}

/// 解析一条入站文本是不是命令。
///
/// 裸数字只有在**恰好有一个**待批请求时才当成审批回复；`pending > 1` 时
/// 数字一律**不是**命令（`2026-09-30` 那篇笔记定的：多个待批时要求指名，
/// 免得「3」被误读成别的意思）。这条闸门是从 cc-haha 抄的
/// （`permission.ts`）。
fn parse_command(text: &str, pending: usize) -> Option<Command> {
    let t = text.trim();
    match t.to_ascii_lowercase().as_str() {
        "/help" | "帮助" => return Some(Command::Help),
        "/status" | "状态" => return Some(Command::Status),
        "/clear" | "清空" => return Some(Command::Clear),
        "/stop" | "停止" => return Some(Command::Stop),
        "/tasks" | "/任务" => return Some(Command::Tasks),
        "/resume" | "/继续" => return Some(Command::Resume),
        "/queue" | "/队列" => return Some(Command::Queue(QueueMode::Report)),
        _ => {}
    }
    if let Some(arg) = t
        .strip_prefix("/queue steer ")
        .or_else(|| t.strip_prefix("/queue　steer　"))
        .map(str::trim)
    {
        // 允许 `/queue steer` 后直接跟正文；空指针交给上层拒绝。
        return Some(Command::Steer(arg.to_string()));
    }
    if let Some(arg) = t
        .strip_prefix("/queue ")
        .or_else(|| t.strip_prefix("/queue　"))
        .map(str::trim)
    {
        return match arg.to_ascii_lowercase().as_str() {
            "interrupt" | "中断" => Some(Command::Queue(QueueMode::Interrupt)),
            _ => None,
        };
    }
    if pending == 1 {
        match t {
            "1" | "/1" | "y" | "yes" | "允许" | "同意" => return Some(Command::Permission(1)),
            "2" | "/2" | "always" | "永久允许" => return Some(Command::Permission(2)),
            "3" | "/3" | "n" | "no" | "拒绝" => return Some(Command::Permission(3)),
            _ => {}
        }
    }
    // 菜单走字母：与审批数字不同形，因此不需要 pending 闸门。
    match t {
        "a" | "/a" => return Some(Command::Menu(MenuAction::Status)),
        "b" | "/b" => return Some(Command::Menu(MenuAction::Tasks)),
        "c" | "/c" => return Some(Command::Menu(MenuAction::Stop)),
        "d" | "/d" => return Some(Command::Menu(MenuAction::Clear)),
        _ => {}
    }
    None
}

const HELP_TEXT: &str = "\
可用命令：
/help      列出本帮助
/status    查看当前会话与运行状态
/tasks     列出运行中与排队的任务
/queue     查看排队情况
/继续      上一轮没跑完时，接着把它跑完
/queue steer <正文>  往正在跑的这一轮注入指引（轮次不中止）
/queue interrupt  中断当前轮次，队首立刻作为新一轮跑
/clear     清空当前会话上下文
/stop      中断本会话正在运行的任务

快捷键（直接回一个字母）：
a 查状态    b 列任务    c 中断当前    d 清空上下文

Agent 要写文件或执行高风险命令时，会在这里发一条审批提示。
届时直接回复 1 允许一次、2 本会话内永久允许、3 拒绝。
多个待批时数字不生效，请指明是哪一条。";


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

/// 一个会话的待跑队列。
///
/// 队列条目自己就是「这个会话正忙」的标记：`run_turn` 结束会把自己从
/// `running` 摘掉，所以排水期间新到的消息如果只看 `running` 就会**插到
/// 已排队的消息前面**。让 `queues` 里的 key 一直留到排空为止，既堵住这个
/// 乱序，也让排水期间到达的消息正确地续在队尾。
#[derive(Default)]
struct SessionQueue {
    items: VecDeque<(Inbound, Arc<dyn ImPort>)>,
    /// 队列满时被挤掉的那些。**不静默丢**：排水时并进下一轮的正文，
    /// agent 和用户都看得见有东西没跑。
    dropped: Vec<String>,
}

impl SessionQueue {
    /// 入队，返回 1-based 位次。超过 `cap` 时挤掉最旧的一条并记进
    /// `dropped`——用户的意图是「我还有话说」，不是「必须按顺序全保」。
    fn push(&mut self, msg: Inbound, port: Arc<dyn ImPort>, cap: usize) -> usize {
        while self.items.len() >= cap {
            if let Some((old, _)) = self.items.pop_front() {
                let head: String = old.text.chars().take(60).collect();
                self.dropped.push(head);
            } else {
                break;
            }
        }
        self.items.push_back((msg, port));
        self.items.len()
    }

    /// 取队首。若此前有被挤掉的条目，把摘要并进这一条的正文开头。
    fn pop(&mut self) -> Option<(Inbound, Arc<dyn ImPort>)> {
        let (mut msg, port) = self.items.pop_front()?;
        if !self.dropped.is_empty() {
            let summary = self.dropped.join(" / ");
            let n = self.dropped.len();
            self.dropped.clear();
            msg.text = format!("（此前 {n} 条因队列已满被丢弃：{summary}）\n{}", msg.text);
        }
        Some((msg, port))
    }
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
    /// 在跑的轮次：`<平台>:<user>` → 这一轮的消息正文。存正文而不只是
    /// 一个标记，是为了让 `/tasks` 能报出"在跑什么"，而不是干巴巴一句
    /// 「运行中」。
    running: Mutex<HashMap<String, String>>,
    /// 每个 `<平台>:<user>` 一条待跑队列。轮次跑着的时候进来的消息进这里，
    /// 不去挤聊天锁——挤锁的后果是消息排在锁上看不见位置，用户以为没收到，
    /// 而且轮后整批重跑。
    queues: Mutex<HashMap<String, SessionQueue>>,
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
            running: Mutex::new(HashMap::new()),
            queues: Mutex::new(HashMap::new()),
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

    /// 这个会话是不是正忙着（在跑轮次，或还有排队的）。
    fn is_busy(&self, key: &str) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(key)
            || self
                .queues
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains_key(key)
    }

    /// 入队，返回 1-based 位次。
    fn enqueue(&self, key: &str, msg: Inbound, port: &Arc<dyn ImPort>) -> usize {
        let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        queues
            .entry(key.to_string())
            .or_default()
            .push(msg, port.clone(), QUEUE_CAP)
    }

    /// 排空队列。聊天锁由 `handle_message` 全程持有（同会话必须串行），
    /// 但 `queues` 这把 std Mutex **绝不能跨 await**——所以锁内只取队首。
    ///
    /// 排空后把 key 从 map 摘掉：摘之前 `is_busy` 一直为真，所以排水期间
    /// 到达的消息会正确地续在队尾，而不是插队。
    async fn drain_queue(&self, platform: &'static str, user: &str) {
        let key = Self::key(platform, user);
        loop {
            let next = {
                let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
                match queues.get_mut(&key).and_then(|q| q.pop()) {
                    Some(item) => Some(item),
                    None => {
                        queues.remove(&key);
                        None
                    }
                }
            };
            let Some((msg, port)) = next else {
                self.publish_session_views();
                return;
            };
            self.run_turn(&port, platform, &msg).await;
        }
    }

    /// 全量重建并发布会话视图，供手机端 `GET /v1/tasks` 读。
    ///
    /// 每次状态变化后调用（入队 / 出队 / 开跑 / 收工）。IM 规模下重建成本
    /// 可忽略，而增量维护意味着每次变更都要记得同步——漏一处就是陈旧数据，
    /// 比慢更糟。
    pub fn publish_session_views(&self) {
        let running: HashMap<String, String> = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut views: Vec<crate::state::ChannelSessionView> = Vec::new();
        for (key, text) in &running {
            let user = key.split_once(':').map(|(_, u)| u).unwrap_or(&key);
            views.push(crate::state::ChannelSessionView {
                key: key.clone(),
                sid: session_id(crate::channels::session::DEVICE_PLATFORM, user),
                state: "running",
                first_line: first_line(text),
                position: None,
            });
        }
        let queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
        for (key, q) in queues.iter() {
            let user = key.split_once(':').map(|(_, u)| u).unwrap_or(&key);
            for (i, (msg, _)) in q.items.iter().enumerate() {
                views.push(crate::state::ChannelSessionView {
                    key: key.clone(),
                    sid: session_id(crate::channels::session::DEVICE_PLATFORM, user),
                    state: "queued",
                    first_line: first_line(&msg.text),
                    position: Some(i + 1),
                });
            }
        }
        drop(queues);
        crate::mobile::publish_channel_sessions(&self.state, views);
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

        // 轮次在跑（或上一轮还在排水）→ 入队，不去挤聊天锁。位次要回执出去，
        // 否则用户无法区分「排队了」和「没收到」。
        let key = Self::key(platform, &user);
        if self.is_busy(&key) {
            let pos = self.enqueue(&key, msg.clone(), &port);
            let _ = port
                .send_text(
                    &user,
                    &format!("📥 已排队（第 {pos} 位）。"),
                    &anchor,
                )
                .await;
            return;
        }

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
        // 本轮跑完，把排队的接着跑完再放走这把锁。放在 `run_turn` 之后而不是
        // 之前，是为了让下一轮的心跳 / 回执 / 审批都走同一条路径。
        self.drain_queue(platform, &user).await;
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
                // 只中断**这个人自己**的轮次。之前这里只能 `interrupt_all()`，
                // 波及本机所有在跑的轮次——一个闲着的 IM 用户发一条 /stop
                // 就能掐掉桌面端正在跑的会话（本文件旧注释记录的隐患）。
                //
                // 命中与否直接用返回值判定，不再先查 `running`：先查后动的
                // 两步之间轮次可能刚好结束，会把「中断成功」误报成「没有在跑」。
                let hit = self
                    .state
                    .agent_service()
                    .interrupt_session(&session_id(platform, user));
                let reply = if hit {
                    "已中断本会话正在运行的任务。"
                } else {
                    "当前没有在运行的任务。"
                };
                let _ = port.send_text(user, reply, anchor).await;
                true
            }
            Command::Permission(_) => {
                self.resolve_permission(port, platform, user, anchor, cmd).await;
                true
            }
            // 菜单里的「中断」必须走**不需要聊天锁**的那条路：轮次在跑时锁
            // 正被持有，等锁就等于排到自己那条队列后面，中断命令本身失效。
            Command::Menu(MenuAction::Stop) => {
                let hit = self
                    .state
                    .agent_service()
                    .interrupt_session(&session_id(platform, user));
                let reply = if hit {
                    "已中断本会话正在运行的任务。"
                } else {
                    "当前没有在运行的任务。"
                };
                let _ = port.send_text(user, reply, anchor).await;
                true
            }
            _ => false,
        }
    }

    /// `/status` 与菜单 `a` 的共用渲染。
    fn render_status(&self, platform: &str, user: &str) -> String {
        let key = Self::key(platform, user);
        let running = self
            .running
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key);
        format!(
            "平台：{platform}\n会话：{}\n状态：{}\n排队：{} 条",
            session_id(platform, user),
            if running { "运行中" } else { "空闲" },
            self.queue_len(&key),
        )
    }

    /// `/tasks`：运行中 + 排队，逐条给首行。
    fn render_tasks(&self, platform: &str, user: &str) -> String {
        let key = Self::key(platform, user);
        let (running, items, dropped) = {
            let running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            let text = running.get(&key).cloned();
            drop(running);
            let queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
            match queues.get(&key) {
                Some(q) => (
                    text,
                    q.items.iter().map(|(m, _)| m.text.clone()).collect::<Vec<_>>(),
                    q.dropped.len(),
                ),
                None => (text, Vec::new(), 0),
            }
        };
        let mut lines: Vec<String> = Vec::new();
        let mut n = 0usize;
        if let Some(text) = running {
            n += 1;
            lines.push(format!("#{n} 运行中：{}", first_line(&text)));
        }
        for text in &items {
            n += 1;
            lines.push(format!("#{n} 排队：{}", first_line(text)));
        }
        if dropped > 0 {
            lines.push(format!("（另有 {dropped} 条因队列已满被丢弃）"));
        }
        if let Some(note) = self.unfinished_turn_note(&session_id(platform, user)) {
            lines.push(note);
        }
        if lines.is_empty() {
            "当前没有任务。".to_string()
        } else {
            lines.join("\n")
        }
    }

    /// `/queue`：只讲排队，不列运行中。
    fn render_queue(&self, platform: &str, user: &str) -> String {
        let key = Self::key(platform, user);
        let (items, dropped) = {
            let queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
            match queues.get(&key) {
                Some(q) => (q.items.len(), q.dropped.len()),
                None => (0, 0),
            }
        };
        let mut out = if items == 0 {
            "队列为空。".to_string()
        } else {
            format!("队列里有 {items} 条。发 /queue interrupt 可中断当前轮次并立刻跑队首。")
        };
        if dropped > 0 {
            out.push_str(&format!("（另有 {dropped} 条因队列已满被丢弃）"));
        }
        out
    }

    fn queue_len(&self, key: &str) -> usize {
        self.queues
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|q| q.items.len())
            .unwrap_or(0)
    }

    /// 上一轮对话没跑完时，返回一条「未跑完」提示；跑完 / 叫停 / 没记录
    /// 都返回 `None`（判据见 `crate::task_record_sink::last_turn_unfinished`）。
    ///
    /// **不做启动时主动推送**：那时用户多半不在，收到的只是一条无人认领的
    /// 消息；而要枚举「所有配对过的 IM 身份」还得另写一遍。挂在 `/tasks`
    /// 上——用户主动问「现在什么情况」时才给，顺带就是最容易忽略的时刻。
    fn unfinished_turn_note(&self, sid: &str) -> Option<String> {
        let store = self.state.task_record_store.as_ref()?;
        let records = store.list_by_conversation(sid).ok()?;
        let rec = crate::task_record_sink::last_turn_unfinished(&records)?;
        let at = chrono::DateTime::from_timestamp_millis(rec.updated_at)
            .map(|t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "时间未知".to_string());
        Some(format!("⏸ 上一轮没跑完（{at} 停在中途），发 /继续 接着来。"))
    }

    /// 需要先拿到聊天锁的命令（会等当前轮跑完）。
    ///
    /// `platform` 是 `&'static str`：`/queue interrupt` 要在这里直接开跑
    /// 一轮，而 `run_turn` 收的就是 `&'static str`（端口表 key 的类型）。
    async fn try_handle_locked(
        &self,
        port: &Arc<dyn ImPort>,
        platform: &'static str,
        user: &str,
        anchor: &str,
        cmd: Command,
    ) -> bool {
        let sid = session_id(platform, user);
        let text = match cmd {
            Command::Help => HELP_TEXT.to_string(),
            Command::Status => self.render_status(platform, user),
            Command::Clear => {
                match self.state.agent_service().reset_session(&sid).await {
                    Ok(()) => "已清空上下文。".to_string(),
                    Err(e) => format!("清空失败：{e}"),
                }
            }
            Command::Tasks => self.render_tasks(platform, user),
            Command::Queue(QueueMode::Report) => self.render_queue(platform, user),
            Command::Queue(QueueMode::Interrupt) => {
                // 先把队首摘出来，否则中断完它还会被排一次。
                let popped = {
                    let mut queues = self.queues.lock().unwrap_or_else(|e| e.into_inner());
                    queues
                        .get_mut(&Self::key(platform, user))
                        .and_then(|q| q.pop())
                };
                let hit = self.state.agent_service().interrupt_session(&sid);
                let status = match (&popped, hit) {
                    (Some(_), true) => "已中断当前轮次，你这条作为新一轮开始。",
                    (Some(_), false) => "当前没有在跑的任务，你这条作为新一轮开始。",
                    (None, true) => "已中断当前轮次。",
                    (None, false) => "当前没有在运行的任务。",
                };
                // 先把状态说清楚再开跑：`run_turn` 自己会发三态回执。
                let _ = port.send_text(user, status, anchor).await;
                if let Some((msg, msg_port)) = popped {
                    self.run_turn(&msg_port, platform, &msg).await;
                }
                return true;
            }
            Command::Steer(guidance) => {
                // steer 不排新轮，而是改进正在跑的那一轮，所以**不进队列**。
                if !self.is_busy(&Self::key(platform, user)) {
                    let _ = port
                        .send_text(user, "当前没有在运行的任务，无需纠偏。", anchor)
                        .await;
                    return true;
                }
                match self.state.agent_service().steer_session(&sid, &guidance) {
                    true => format!("已注入指引：{guidance}"),
                    false => "这条没能注入（没有在跑的轮次，或内容为空）。".to_string(),
                }
            }
            Command::Resume => {
                // 续跑**不做盲重发**：原样重发上一条 user 消息，会让上一轮
                // 已经完成的副作用再执行一次（改过的文件、跑过的命令）。
                // 所以取回原文后加一段中断说明，让 agent 自己判断该从哪接。
                let messages = match self.state.agent_service().get_session_messages(&sid).await {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = port
                            .send_text(
                                user,
                                &format!("读不出上一轮内容（{e}），请把要做的事重说一遍。"),
                                anchor,
                            )
                            .await;
                        return true;
                    }
                };
                let prev = messages
                    .iter()
                    .rev()
                    .find_map(|m| match m.role {
                        hermix_core::MessageRole::User => m.content.as_deref().map(str::trim),
                        _ => None,
                    })
                    .filter(|t| !t.is_empty())
                    .map(str::to_string);
                let Some(prev) = prev else {
                    let _ = port
                        .send_text(user, "找不到上一条你的指令，没法续跑。", anchor)
                        .await;
                    return true;
                };
                // dedup_key 必须重新生成：沿用原文的键会被去重表当成重投丢掉。
                let nonce = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                let resumed = Inbound {
                    user: user.to_string(),
                    display_name: String::new(),
                    text: format!(
                        "上一轮在执行过程中中断了（未跑完）。下面是当时的任务，请**先检查现状**再决定怎么做，\
                         不要假定任何操作都没做过：\n\n{prev}"
                    ),
                    dedup_key: format!("resume-{sid}-{nonce}"),
                    anchor: anchor.to_string(),
                };
                // 走正常轮次路径：心跳、三态回执、审批都复用同一条。
                self.run_turn(port, platform, &resumed).await;
                return true;
            }
            Command::Menu(action) => {
                match action {
                    MenuAction::Status => self.render_status(platform, user),
                    MenuAction::Tasks => self.render_tasks(platform, user),
                    MenuAction::Clear => {
                        match self.state.agent_service().reset_session(&sid).await {
                            Ok(()) => "已清空上下文。".to_string(),
                            Err(e) => format!("清空失败：{e}"),
                        }
                    }
                    // Stop 在 `try_handle_immediate` 就处理掉了，走不到这里。
                    MenuAction::Stop => "当前没有在运行的任务。".to_string(),
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
        let started = std::time::Instant::now();
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

        // `ToolTrace` 提到 pump 外面：心跳 task 要读它，才知道"这一轮到底
        // 在忙什么"，而不是只发一句"还在跑"。锁的粒度是「改完立刻放」——
        // 绝不能把 guard 跨 `.await` 持有（既非 Send，也会把 await 点锁死）。
        let tools = std::sync::Arc::new(std::sync::Mutex::new(ToolTrace::default()));
        let pump = tokio::spawn({
            let tools = tools.clone();
            async move {
                let mut header_shown = String::new();
                // 正文全程直通流式（打字机效果）。新工具开跑时 clear_text 把已
                // 流出的中间叙述（计划、工具名自白、失败现场）清掉——气泡回到
                // 只剩进度行，最终回答随后重新长出来，收尾气泡是干净的。
                // 工具执行期没有正文 delta，"还活着"由进度行刷帧负责。
                while let Some(msg) = rx.recv().await {
                    match msg {
                        StreamMsg::Text(delta) => {
                            tools.lock().unwrap_or_else(|e| e.into_inner()).clear_when_idle();
                            stream.append(&delta).await;
                        }
                        StreamMsg::Tool(name, done) => {
                            if !done {
                                stream.clear_text().await;
                            }
                            let next = {
                                let mut t = tools.lock().unwrap_or_else(|e| e.into_inner());
                                t.push(Some(&name), done);
                                t.render()
                            };
                            // 轨迹没变就不重发，省掉一整轮 HTTP。
                            if next != header_shown {
                                header_shown = next;
                                stream.set_header(&header_shown).await;
                            }
                        }
                    }
                }
                stream.finish().await;
            }
        });

        // 心跳：长任务在 IM 里唯一的"还活着"信号。pump 只在有正文或工具事件
        // 时才发消息，所以「模型在想 / 工具在跑但迟迟没输出」整段静默得由外部
        // 补。生命周期就是本轮——`send_message_stream` 一返回就 abort，所以
        // 不必再去查 `running`（那是多余的一层，反而要求跨 task 共享它）。
        let heartbeat = spawn_heartbeat(
            tools.clone(),
            port.clone(),
            msg.user.clone(),
            msg.anchor.clone(),
            Heartbeat::default(),
        );

        let overrides = self.build_overrides(port, platform, &msg.user, &sid);
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.insert(Self::key(platform, &msg.user), msg.text.clone());
        }
        self.publish_session_views();
        let result = self
            .state
            .agent_service()
            .send_message_stream(&sid, &msg.text, overrides, on_chunk)
            .await;
        {
            let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
            running.remove(&Self::key(platform, &msg.user));
        }
        self.publish_session_views();
        // `overrides`（连同 on_chunk 里的 tx）已随 send_message_stream 一起被
        // 消费掉，这里 future 一返回 tx 就析构，pump 的 recv 拿到 None，自行
        // 收尾。只需等它把最后一帧发完。
        pump.await.ok();
        // 本轮结束，心跳没有存在理由了。必须 abort：task 里的 sleep 会一直
        // 醒着，不收就是每轮漏一个野 task。
        heartbeat.abort();

        // 三态回执。IM 用户发完消息就可能走开，只靠流式卡片，回来时
        // 面对的是"不知道跑完没有"——这正是要补的洞。
        match result {
            Ok(res) if crate::util::was_interrupted(&res) => {
                let _ = port
                    .send_text(
                        &msg.user,
                        "⏹ 已按 /stop 中断，部分结果已保存。",
                        &msg.anchor,
                    )
                    .await;
            }
            // ✅ 回执只给跑得够久的轮次。用户盯着屏幕看着答案滚完，再补一条
            // "完成"是纯噪音；而长任务正是"人已走开、回来要知道结果"的那类。
            // 阈值与 PR-3 的心跳同源，都是 60s。短轮次静默收尾。
            Ok(res) if started.elapsed() >= LONG_TURN => {
                let tail = crate::util::truncate_chars(res.text.trim(), 200);
                let text = if tail.is_empty() {
                    "✅ 完成。".to_string()
                } else {
                    format!("✅ 完成：{tail}")
                };
                let _ = port.send_text(&msg.user, &text, &msg.anchor).await;
            }
            Ok(_) => {}
            Err(hermix_core::AgentError::Interrupted { .. }) => {
                // 旧路径：中断以 Err 回来。别报成"出错了"——用户是自己
                // 叫停的，告诉他系统坏了是在误导。
                let _ = port
                    .send_text(
                        &msg.user,
                        "⏹ 已按 /stop 中断，部分结果已保存。",
                        &msg.anchor,
                    )
                    .await;
            }
            Err(e) => {
                let (user_msg, _) = crate::util::user_facing_agent_error(&e.to_string());
                let _ = port
                    .send_text(
                        &msg.user,
                        &format!("❌ 出错了：{user_msg}"),
                        &msg.anchor,
                    )
                    .await;
            }
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
    use crate::channels::port::ImStream;

    /// 测试替身：把 `send_text` 的内容按序记下来，供心跳断言。
    struct RecordingPort {
        sent: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ImPort for RecordingPort {
        fn platform(&self) -> &'static str {
            "test"
        }
        async fn send_text(&self, _user: &str, content: &str, _anchor: &str) -> Result<(), String> {
            self.sent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(content.to_string());
            Ok(())
        }
        fn start_stream(&self, _user: &str, _anchor: &str) -> Box<dyn ImStream> {
            Box::new(NoopStream)
        }
    }

    struct NoopStream;

    #[async_trait::async_trait]
    impl ImStream for NoopStream {
        async fn append(&mut self, _delta: &str) {}
        async fn set_header(&mut self, _header: &str) {}
        async fn finish(&mut self) {}
    }

    /// 毫秒级节奏，供心跳测试用。
    fn fast_hb() -> Heartbeat {
        Heartbeat {
            fast: std::time::Duration::from_millis(10),
            slow: std::time::Duration::from_millis(10),
            fast_beats: 2,
            max_beats: 1000,
        }
    }

    fn inbound(text: &str) -> Inbound {
        Inbound {
            user: "u".to_string(),
            display_name: "测试".to_string(),
            text: text.to_string(),
            dedup_key: format!("k-{text}"),
            anchor: "a".to_string(),
        }
    }

    fn dummy_port() -> std::sync::Arc<RecordingPort> {
        std::sync::Arc::new(RecordingPort {
            sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    }

    #[test]
    fn 队列是先进先出() {
        let mut q = SessionQueue::default();
        let port = dummy_port();
        assert_eq!(q.push(inbound("一"), port.clone(), 20), 1);
        assert_eq!(q.push(inbound("二"), port.clone(), 20), 2);
        assert_eq!(q.push(inbound("三"), port.clone(), 20), 3);
        assert_eq!(q.pop().unwrap().0.text, "一");
        assert_eq!(q.pop().unwrap().0.text, "二");
        assert_eq!(q.pop().unwrap().0.text, "三");
        assert!(q.pop().is_none(), "排空后不该再吐东西");
    }

    #[test]
    fn 满队列挤掉最旧并把摘要并进下一轮() {
        let mut q = SessionQueue::default();
        let port = dummy_port();
        for t in ["一", "二", "三"] {
            q.push(inbound(t), port.clone(), 3);
        }
        // 满了，挤掉「一」
        assert_eq!(q.push(inbound("四"), port.clone(), 3), 3);
        let first = q.pop().unwrap().0.text;
        assert!(
            first.starts_with("（此前 1 条因队列已满被丢弃：一）"),
            "被丢的那条必须出现在下一轮正文里，实际：{first}"
        );
        assert!(first.contains("二"), "摘要后面接的是真正的队首");
    }

    #[test]
    fn 丢弃摘要只并一次() {
        let mut q = SessionQueue::default();
        let port = dummy_port();
        for t in ["a", "b", "c"] {
            q.push(inbound(t), port.clone(), 3);
        }
        q.push(inbound("d"), port.clone(), 3);
        q.push(inbound("e"), port.clone(), 3);
        let first = q.pop().unwrap().0.text;
        assert!(first.contains("a"));
        let second = q.pop().unwrap().0.text;
        assert!(
            !second.contains("被丢弃"),
            "摘要不该粘到每一条上，实际：{second}"
        );
    }

    #[test]
    fn 上限为零时不失控() {
        let mut q = SessionQueue::default();
        let port = dummy_port();
        // cap=0 的病态情形不该 panic 或无限循环
        let _ = q.push(inbound("x"), port.clone(), 0);
        assert!(q.items.is_empty() || q.items.len() == 1);
    }

    #[test]
    fn 挤掉时按字符截断长文本() {
        let mut q = SessionQueue::default();
        let port = dummy_port();
        let long = "长".repeat(200);
        q.push(inbound(&long), port.clone(), 1);
        q.push(inbound("后"), port.clone(), 1);
        let first = q.pop().unwrap().0.text;
        assert!(first.contains(&"长".repeat(60)), "只取前 60 字");
        assert!(!first.contains(&"长".repeat(61)), "不该整段塞进去");
    }

    // 时间虚拟化：这三个用例只看「心跳有没有按序发出来」，不需要真实等待。
    // 原来用真 sleep + 40/120ms 窗口，全量并发跑时任务排不上队就会偶发变红——
    // 而变红的理由跟被测行为毫无关系，纯粹是调度抖动。
    #[tokio::test(start_paused = true)]
    async fn 心跳持续发且abort后彻底停() {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let port = std::sync::Arc::new(RecordingPort {
            sent: sent.clone(),
        });
        let tools = std::sync::Arc::new(std::sync::Mutex::new(ToolTrace::default()));
        let hb = spawn_heartbeat(
            tools,
            port,
            "u".to_string(),
            "a".to_string(),
            fast_hb(),
        );

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let running = sent.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert!(running >= 3, "长任务应持续收到心跳，实际 {running} 条");
        assert!(
            sent.lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .all(|m| m.starts_with('⏳')),
            "心跳文案统一前缀"
        );

        hb.abort();
        // 等一个心跳周期，确认 abort 真的生效（否则这里会继续涨）。
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let after = sent.lock().unwrap_or_else(|e| e.into_inner()).len();
        assert_eq!(after, running, "abort 后不得再发心跳");
    }

    // 时间虚拟化：这三个用例只看「心跳有没有按序发出来」，不需要真实等待。
    // 原来用真 sleep + 40/120ms 窗口，全量并发跑时任务排不上队就会偶发变红——
    // 而变红的理由跟被测行为毫无关系，纯粹是调度抖动。
    #[tokio::test(start_paused = true)]
    async fn 心跳带出工具轨迹_没有工具时说还在想() {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let port = std::sync::Arc::new(RecordingPort {
            sent: sent.clone(),
        });
        let tools = std::sync::Arc::new(std::sync::Mutex::new(ToolTrace::default()));
        let hb = spawn_heartbeat(
            tools.clone(),
            port,
            "u".to_string(),
            "a".to_string(),
            fast_hb(),
        );

        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(
            sent.lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|m| m.contains("还在想")),
            "没有工具轨迹时要说「还在想」，不能发空的进度行"
        );

        tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Some("read_file"), false);
        sent.lock().unwrap_or_else(|e| e.into_inner()).clear();
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        assert!(
            sent.lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .any(|m| m.contains("read_file")),
            "心跳应带出当前在跑的工具名"
        );
        hb.abort();
    }

    // 时间虚拟化：这三个用例只看「心跳有没有按序发出来」，不需要真实等待。
    // 原来用真 sleep + 40/120ms 窗口，全量并发跑时任务排不上队就会偶发变红——
    // 而变红的理由跟被测行为毫无关系，纯粹是调度抖动。
    #[tokio::test(start_paused = true)]
    async fn 心跳降频会明说而不是静默() {
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let port = std::sync::Arc::new(RecordingPort {
            sent: sent.clone(),
        });
        let hb = spawn_heartbeat(
            std::sync::Arc::new(std::sync::Mutex::new(ToolTrace::default())),
            port,
            "u".to_string(),
            "a".to_string(),
            Heartbeat {
                fast: std::time::Duration::from_millis(5),
                slow: std::time::Duration::from_millis(5),
                fast_beats: 1,
                max_beats: 6,
            },
        );
        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        let got = sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|m| m.contains("降频"));
        assert!(got, "间隔拉长必须告知用户，否则最后一条心跳会误导");
        // 6 次上限后自行收工：这是泄漏兜底
        let total = sent.lock().unwrap_or_else(|e| e.into_inner()).len();
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        assert_eq!(
            sent.lock().unwrap_or_else(|e| e.into_inner()).len(),
            total,
            "达到 max_beats 后须自行收工"
        );
        hb.abort();
    }

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
    fn 多个待批时裸数字不是命令() {
        // 回归红线：`pending > 1` 时用户打「3」是想拒绝某一条，不是选菜单。
        // 数字与审批 1/2/3 同形，这里必须原样落回普通文本。
        for t in ["1", "2", "3"] {
            assert_eq!(parse_command(t, 3), None, "pending=3 时 {t} 不得是命令");
        }
        assert_eq!(parse_command("3", 0), None, "无待批时裸数字是普通文本");
    }

    #[test]
    fn 菜单走字母_不跟审批数字撞车() {
        assert_eq!(parse_command("a", 0), Some(Command::Menu(MenuAction::Status)));
        assert_eq!(parse_command("b", 3), Some(Command::Menu(MenuAction::Tasks)));
        assert_eq!(parse_command("c", 1), Some(Command::Menu(MenuAction::Stop)));
        assert_eq!(parse_command("d", 0), Some(Command::Menu(MenuAction::Clear)));
        assert_eq!(parse_command("e", 0), None);
    }

    #[test]
    fn 队列与任务命令() {
        assert_eq!(parse_command("/queue", 0), Some(Command::Queue(QueueMode::Report)));
        assert_eq!(parse_command("/队列", 0), Some(Command::Queue(QueueMode::Report)));
        assert_eq!(
            parse_command("/queue interrupt", 0),
            Some(Command::Queue(QueueMode::Interrupt))
        );
        assert_eq!(
            parse_command("/queue  中断", 0),
            Some(Command::Queue(QueueMode::Interrupt))
        );
        // 未知模式不吃掉它，当普通文本
        assert_eq!(parse_command("/queue 不存在", 0), None);
        assert_eq!(parse_command("/tasks", 0), Some(Command::Tasks));
        assert_eq!(parse_command("/任务", 0), Some(Command::Tasks));
    }

    #[test]
    fn 继续命令能解析且不与审批撞车() {
        assert_eq!(parse_command("/继续", 0), Some(Command::Resume));
        assert_eq!(parse_command("/resume", 0), Some(Command::Resume));
        // 有待批时 /继续 仍然有效——恢复和审批是两件事
        assert_eq!(parse_command("/继续", 1), Some(Command::Resume));
        assert_eq!(parse_command("继续跑", 0), None, "不带斜杠的当普通文本");
    }

    #[test]
    fn 帮助里列了继续() {
        assert!(HELP_TEXT.contains("/继续"));
    }

    #[test]
    fn steer_带出指引正文() {
        assert_eq!(
            parse_command("/queue steer 只改 README", 0),
            Some(Command::Steer("只改 README".to_string()))
        );
        assert_eq!(
            parse_command("/queue steer 别动 package.json", 1),
            Some(Command::Steer("别动 package.json".to_string())),
            "有待批时 steer 也要能用——纠偏和审批是两件事"
        );
        // 裸 `/queue steer` 没有正文，交给上层拒绝，不静默当普通文本
        assert_eq!(parse_command("/queue steer", 0), None);
    }

    #[test]
    fn 帮助里列了新增命令() {
        for expected in ["/tasks", "/queue", "interrupt", "快捷键"] {
            assert!(HELP_TEXT.contains(expected), "帮助缺 {expected}");
        }
    }

    #[test]
    fn 首行按字符截断() {
        assert_eq!(first_line("第一行\n第二行"), "第一行");
        assert_eq!(first_line(&"长".repeat(60)), format!("{}…", "长".repeat(40)));
        assert_eq!(first_line("   \n  x"), "x", "首行空白时往后找");
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
