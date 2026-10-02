//! 持久终端会话表 —— ConPTY（portable-pty 0.9）之上的跨调用 shell 会话。
//!
//! 真机事实（`tests/pty_conpty_probe.rs` 逐条钉死，改动前先重跑）：
//!
//! 1. **DSR 握手**：ConPTY 启动即向终端发 `ESC[6n`（光标位置请求），
//!    必须在输入管道回复 `ESC[1;1R`，否则 conhost 永不渲染，
//!    cmd/PowerShell 全部静默卡死。读线程内置自动应答。
//! 2. **会话 shell 形态**：`powershell.exe -NoLogo -NoProfile
//!    -NonInteractive -NoExit -Command "function prompt { '<marker>> ' }"`。
//!    `-NonInteractive` 跳过 PSReadLine（默认交互态逐字符语法高亮回显，
//!    噪音不可用）；marker 提示符在 shell 就绪时自渲染（冷启 2-4s），
//!    每条命令结束后重现——这是逐命令的**就绪信号**。
//! 3. **输出编码恒为 UTF-8**（子进程 OEM 字节先被 conhost 解码进
//!    Unicode 缓冲再重编码）。
//! 4. **关闭顺序**：kill → 等退出 → 排干 → drop master。直接 drop
//!    活着 pseudoconsole 会挂死（ClosePseudoConsole 阻塞）。
//! 5. ConPTY 合成噪音（OSC 标题 / SGR / 光标）由 [`crate::pty_filter`] 剥离。
//!
//! 就绪/空闲判定是**尽力而为**的启发式（marker 优先，输出静默 +
//! Toolhelp32 子进程快照兜底，总预算封顶）——Windows 无 /proc，
//! 语义见立项笔记 `docs/notes/proposed/architecture/2026-10-01-persistent-terminal-conpty.md`。

use std::collections::HashMap;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use hermix_core::{AgentError, CommandOutput, PtySessionInfo, PtySessionOutput};

use crate::pty_filter::VtFilter;

/// 单命令输出静默判定：静默超过此时长且 shell 无存活子进程 → 视为完成。
const SILENCE_MS: u64 = 1500;
/// 静默但有存活子进程（REPL 等输入、安静构建阶段）的兜底时长。
const QUIET_ALIVE_MS: u64 = 5000;
/// 读循环默认总预算（对齐 terminal 工具 30s 默认超时）。
const READ_BUDGET_DEFAULT_MS: u64 = 30_000;
/// 读循环总预算硬上限（对齐 TERMINAL_HARD_TIMEOUT_SECS）。
const READ_BUDGET_MAX_MS: u64 = 120_000;
/// 会话创建时等待首个 marker 的上限（探针实测冷启 ~2-4s，AV 重载机器放宽）。
const BOOT_MARKER_WAIT_MS: u64 = 20_000;
/// 读循环轮询间隔。
const READ_TICK_MS: u64 = 80;
/// 空闲会话回收阈值。
const SESSION_TTL_SECS: u64 = 30 * 60;
/// 并发会话上限（超出要求先关闭，防止句柄/进程泄漏）。
const MAX_SESSIONS: usize = 8;
/// 单会话待读文本上限（超出丢最旧，防止静默会话无限吃内存）。
const SESSION_BUFFER_CAP: usize = 1_000_000;
/// ConPTY 列宽：足够宽以减少重排换行（行宽换行是 ConPTY 重编码的固有噪音）。
const PTY_COLS: u16 = 250;
const PTY_ROWS: u16 = 30;
/// 子进程存活检查间隔（Toolhelp32 快照有成本，不必每 tick 做）。
const CHILDREN_POLL_MS: u64 = 500;

type Shared<T> = Arc<Mutex<T>>;

/// 会话累积缓冲：过滤后文本 + 当前未换行片段（marker 剥离在此进行）。
struct SessionBuffer {
    /// 已就绪待读文本（模型可见）。
    text: String,
    /// 当前行累积（marker 可跨 feed 拆开，必须在行缓冲内做匹配）。
    line_buf: String,
    /// 剥离掉的 marker 计数 —— 会话就绪 / 命令完成的信号源。
    marker_seen: u64,
    last_output: Instant,
    eof: bool,
    /// true = 曾因超上限丢弃过最旧文本。
    overflow: bool,
}

impl SessionBuffer {
    fn new() -> Self {
        Self {
            text: String::new(),
            line_buf: String::new(),
            marker_seen: 0,
            last_output: Instant::now(),
            eof: false,
            overflow: false,
        }
    }

    fn touch(&mut self) {
        self.last_output = Instant::now();
    }

    /// 追加过滤后的文本。marker 剥离规则：包含 marker 串的行，从行首
    /// 剥到 marker 之后（提示符形态是 `MARKER> `，连同 `> ` 一并剥掉，
    /// 命令回显保留在同行）。
    fn append_filtered(&mut self, s: &str, marker: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                self.flush_line(marker);
            } else {
                self.line_buf.push(ch);
                // 提示符完整渲染在行中间即可判就绪（回显同行跟随），
                // 不等换行——prompt 后没有输出时永远不换行。
                self.try_strip_marker(marker);
                self.cap();
            }
        }
    }

    /// 会话关闭时把未换行的残余行冲进 text。
    fn flush_partial_line(&mut self, marker: &str) {
        self.try_strip_marker(marker);
        if !self.line_buf.is_empty() {
            self.text.push_str(&self.line_buf);
            self.line_buf.clear();
        }
    }

    fn take_text(&mut self) -> String {
        let mut out = std::mem::take(&mut self.text);
        if self.overflow {
            out.insert_str(0, "[...older output dropped (session buffer overflow)...]\n");
            self.overflow = false;
        }
        out
    }

    fn flush_line(&mut self, marker: &str) {
        self.try_strip_marker(marker);
        self.text.push_str(&self.line_buf);
        self.text.push('\n');
        self.line_buf.clear();
    }

    fn try_strip_marker(&mut self, marker: &str) {
        if marker.is_empty() {
            return;
        }
        // 匹配完整提示符 `{marker}> `（含箭头与空格）：token 单独完成
        // 时 `> ` 尚未渲染，必须等整个提示符形态到齐再剥，否则 prompt
        // 的 `> ` 会残留在模型可见文本里。
        let needle = format!("{marker}> ");
        if let Some(pos) = self.line_buf.find(&needle) {
            self.line_buf = self.line_buf[pos + needle.len()..].to_string();
            self.marker_seen += 1;
        }
    }

    fn cap(&mut self) {
        if self.text.len() + self.line_buf.len() <= SESSION_BUFFER_CAP {
            return;
        }
        // 病态长行（无换行的大块输出）：先把行缓冲冲进 text（放弃该行
        // 的 marker 剥离），再裁最旧的一半。
        let line = std::mem::take(&mut self.line_buf);
        self.text.push_str(&line);
        self.text.push('\n');
        let mut idx = self.text.len().saturating_sub(SESSION_BUFFER_CAP / 2);
        while idx < self.text.len() && !self.text.is_char_boundary(idx) {
            idx += 1;
        }
        if idx > 0 {
            self.text.drain(..idx);
        }
        self.overflow = true;
    }
}

/// 读线程：从 ConPTY master 读原始字节 → VtFilter 剥离 → DSR 自动应答
/// → 会话缓冲。永不被 join：关闭时 master 被 drop，read 报错/EOF 后线程
/// 自行退出（探针 v3 钉死：不能依赖 kill 子进程带来 EOF）。
fn spawn_reader(
    mut reader: Box<dyn std::io::Read + Send>,
    writer: Shared<Box<dyn Write + Send>>,
    buf: Shared<SessionBuffer>,
    marker: String,
) {
    std::thread::spawn(move || {
        let mut filter = VtFilter::new();
        let mut chunk = [0u8; 8192];
        let mut acc = String::new();
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => {
                    filter.flush(&mut acc);
                    {
                        let mut b = lock(&buf);
                        b.append_filtered(&acc, &marker);
                        b.flush_partial_line(&marker);
                        b.eof = true;
                        b.touch();
                    }
                    break;
                }
                Ok(n) => {
                    let events = filter.feed(&chunk[..n], &mut acc);
                    if events.dsr_requests > 0 {
                        // ConPTY 的 DSR 光标位置请求：不回复它永不渲染。
                        let mut w = lock(&writer);
                        let _ = w.write_all(b"\x1b[1;1R");
                        let _ = w.flush();
                    }
                    if !acc.is_empty() {
                        let mut b = lock(&buf);
                        b.append_filtered(&acc, &marker);
                        b.touch();
                        acc.clear();
                    }
                }
            }
        }
    });
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

struct PtySession {
    id: String,
    shell: String,
    workdir: Option<String>,
    created_at: Instant,
    buf: Shared<SessionBuffer>,
    writer: Shared<Box<dyn Write + Send>>,
    /// master 持有 pseudoconsole 句柄，drop 即关闭会话——只在
    /// [`PtySession::shutdown`] 的关闭序列里拿走。
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    child: Mutex<Option<Box<dyn Child + Send + Sync>>>,
}

impl PtySession {
    /// shell 进程是否仍在（`GetExitCodeProcess` 非阻塞）。
    fn child_alive(&self) -> bool {
        self.child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            .and_then(|c| c.try_wait().ok().flatten())
            .is_none()
    }

    fn alive(&self) -> bool {
        self.child_alive() && !lock(&self.buf).eof
    }

    fn info(&self) -> PtySessionInfo {
        // 先取快照再放锁：`std::sync::Mutex` 不可重入，持有 `buf`
        // 锁时调用 `alive()`（内部再锁 `buf`）会自死锁。
        let (last_activity_ms, eof) = {
            let b = lock(&self.buf);
            (unix_millis_from(b.last_output), b.eof)
        };
        PtySessionInfo {
            session_id: self.id.clone(),
            shell: self.shell.clone(),
            workdir: self.workdir.clone(),
            created_at_ms: unix_millis_from(self.created_at),
            last_activity_ms,
            alive: !eof && self.child_alive(),
        }
    }

    /// 关闭序列（探针钉死）：kill → 等退出 → drop master → 排干窗口。
    /// 扔进 blocking 线程后台执行，不阻塞调用方。
    fn shutdown(self: Arc<Self>) {
        if let Ok(mut g) = self.child.lock() {
            if let Some(c) = g.as_mut() {
                let mut killer = c.clone_killer();
                let _ = killer.kill();
            }
        }
        let master = self
            .master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        let child = self.child.lock().unwrap_or_else(|e| e.into_inner()).take();
        std::thread::spawn(move || {
            if let Some(mut child) = child {
                for _ in 0..40 {
                    if child.try_wait().ok().flatten().is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            drop(master);
            std::thread::sleep(Duration::from_millis(200));
        });
    }
}

fn unix_millis_from(t: Instant) -> u64 {
    // Instant 无纪元；用与进程启动点的差值近似 unix 毫秒（展示用途）。
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    now.saturating_sub(t.elapsed().as_millis() as u64)
}

/// 持久终端会话表。挂在 `LocalBackend` 上，随 [`hermix_core::TerminalBackend`]
/// 的会话方法对外暴露。
pub struct PtySessionRegistry {
    sessions: Mutex<HashMap<String, Arc<PtySession>>>,
    seq: AtomicU64,
}

impl Default for PtySessionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PtySessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }
    }

    /// 创建会话并阻塞到 shell 就绪（marker 自渲染）。ConPTY 不可用
    /// （老版本 Windows）或 PowerShell 缺失时返回错误——调用方降级为
    /// 一次性模式，不阻断启动。
    pub async fn create_session(&self, workdir: Option<&str>) -> Result<String, AgentError> {
        self.reap_idle();
        {
            let sessions = lock(&self.sessions);
            if sessions.len() >= MAX_SESSIONS {
                let ids: Vec<&str> = sessions.keys().map(String::as_str).collect();
                return Err(AgentError::Io(format!(
                    "persistent session limit reached ({}): {}. \
                     Close one with the session's close flow before creating another.",
                    MAX_SESSIONS,
                    ids.join(", ")
                )));
            }
        }

        let n = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("pty-{n}");
        let marker = format!("HERMIXPTY-{}", id);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AgentError::Io(format!("ConPTY openpty failed: {:#}", e)))?;

        let mut cmd = CommandBuilder::new("powershell.exe");
        cmd.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-NoExit",
            "-Command",
        ]);
        cmd.arg(format!("function prompt {{ '{marker}> ' }}"));
        if let Some(dir) = workdir {
            cmd.cwd(dir);
        }
        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AgentError::Io(format!("failed to spawn session shell: {:#}", e)))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AgentError::Io(format!("ConPTY reader failed: {:#}", e)))?;
        let writer: Box<dyn Write + Send> = pair
            .master
            .take_writer()
            .map_err(|e| AgentError::Io(format!("ConPTY writer failed: {:#}", e)))?;
        let writer = Arc::new(Mutex::new(writer));
        let buf = Arc::new(Mutex::new(SessionBuffer::new()));
        spawn_reader(reader, writer.clone(), buf.clone(), marker.clone());

        let session = Arc::new(PtySession {
            id: id.clone(),
            shell: "powershell.exe".to_string(),
            workdir: workdir.map(str::to_string),
            created_at: Instant::now(),
            buf: buf.clone(),
            writer,
            master: Mutex::new(Some(pair.master)),
            child: Mutex::new(Some(child)),
        });

        // 等 shell 就绪（首个 marker 自渲染）。
        let boot_deadline = Instant::now() + Duration::from_millis(BOOT_MARKER_WAIT_MS);
        loop {
            if lock(&buf).marker_seen >= 1 {
                break;
            }
            if !session.alive() {
                session.shutdown();
                return Err(AgentError::Io(
                    "session shell exited before becoming ready".to_string(),
                ));
            }
            if Instant::now() >= boot_deadline {
                session.shutdown();
                return Err(AgentError::Io(
                    "persistent session shell did not become ready within 20s \
                     (ConPTY requires Windows 10 1809+; fall back to non-persistent \
                     terminal commands)"
                        .to_string(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        lock(&self.sessions).insert(id.clone(), session);
        Ok(id)
    }

    /// 提交输入并读取新产出。`submit=false` 发送不带换行的原始片段
    /// （REPL 续行、多行粘贴）；空输入 + `submit=false` 即纯读取。
    pub async fn send_session(
        &self,
        session_id: &str,
        input: &str,
        submit: bool,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        let session = self.get_session(session_id)?;
        if !input.is_empty() || submit {
            let payload = if submit {
                format!("{input}\r\n")
            } else {
                input.to_string()
            };
            {
                let mut w = lock(&session.writer);
                w.write_all(payload.as_bytes())
                    .map_err(|e| AgentError::Io(format!("session write failed: {e}")))?;
                w.flush()
                    .map_err(|e| AgentError::Io(format!("session flush failed: {e}")))?;
            }
        }
        self.read_loop(&session, read_budget_ms).await
    }

    /// 纯读取：不提交任何输入。
    pub async fn read_session(
        &self,
        session_id: &str,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        let session = self.get_session(session_id)?;
        self.read_loop(&session, read_budget_ms).await
    }

    pub fn list_sessions(&self) -> Vec<PtySessionInfo> {
        self.reap_idle();
        lock(&self.sessions)
            .values()
            .map(|s| s.info())
            .collect::<Vec<_>>()
    }

    pub async fn close_session(&self, session_id: &str) -> Result<(), AgentError> {
        let session = lock(&self.sessions)
            .remove(session_id)
            .ok_or_else(|| unknown_session(session_id))?;
        session.shutdown();
        Ok(())
    }

    /// `pty: true` 的一次性 ConPTY 执行（替换旧"Windows 不支持 PTY"报错）。
    /// 命令路由复用 local.rs 的 PowerShell 统一路由。
    pub async fn execute_oneshot(
        &self,
        command: &str,
        timeout_secs: u64,
        workdir: Option<&std::path::Path>,
        max_output_size: usize,
    ) -> Result<CommandOutput, AgentError> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: PTY_ROWS,
                cols: PTY_COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| AgentError::Io(format!("ConPTY openpty failed: {:#}", e)))?;

        let (exe, mut args) = crate::local::windows_shell_route(command);
        args.insert(0, "-NoLogo".to_string());
        let mut cmd = CommandBuilder::new(exe);
        cmd.args(args);
        if let Some(dir) = workdir {
            cmd.cwd(dir);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| AgentError::Io(format!("failed to spawn PTY command: {:#}", e)))?;
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| AgentError::Io(format!("ConPTY reader failed: {:#}", e)))?;
        let writer: Box<dyn Write + Send> = pair
            .master
            .take_writer()
            .map_err(|e| AgentError::Io(format!("ConPTY writer failed: {:#}", e)))?;
        let buf = Arc::new(Mutex::new(SessionBuffer::new()));
        spawn_reader(
            reader,
            Arc::new(Mutex::new(writer)),
            buf.clone(),
            String::new(),
        );

        // 等 exit（预算内）；超时 kill 整树外的顶层子进程并返回已收输出。
        let killer = child.clone_killer();
        let wait = tokio::task::spawn_blocking(move || child.wait());
        let status = match tokio::time::timeout(Duration::from_secs(timeout_secs), wait).await {
            Ok(Ok(Ok(status))) => Some(status),
            Ok(Ok(Err(e))) => {
                return Err(AgentError::Io(format!("PTY child wait failed: {e}")));
            }
            Ok(Err(e)) => {
                return Err(AgentError::Io(format!("PTY wait task failed: {e}")));
            }
            Err(_) => {
                let mut k = killer;
                let _ = k.kill();
                None
            }
        };
        // 排干窗口：conhost 把残余缓冲刷出。
        tokio::time::sleep(Duration::from_millis(300)).await;

        let mut text = {
            let mut b = lock(&buf);
            b.flush_partial_line("");
            b.take_text()
        };
        if text.len() > max_output_size {
            tracing::warn!("PTY output exceeded max size ({} bytes), truncating", text.len());
            let mut cut = max_output_size;
            while cut < text.len() && !text.is_char_boundary(cut) {
                cut += 1;
            }
            text.truncate(cut);
        }
        let exit_code = status.map(|s| s.exit_code() as i32).unwrap_or(-1);
        drop(pair.master);
        Ok(CommandOutput {
            exit_code,
            stdout: text,
            stderr: String::new(),
        })
    }

    fn get_session(&self, session_id: &str) -> Result<Arc<PtySession>, AgentError> {
        self.reap_idle();
        lock(&self.sessions)
            .get(session_id)
            .cloned()
            .ok_or_else(|| unknown_session(session_id))
    }

    /// 完成判定读循环（启发式，全部"尽力而为"）：
    /// 1. 本调用期间 marker 重现且见到新输出 → 命令完成（主信号）；
    /// 2. 静默 ≥ SILENCE 且无存活子进程 → 完成；
    /// 3. 静默 ≥ QUIET_ALIVE（有子进程也放行，REPL 等输入场景）→ 完成；
    /// 4. 无新输出且静默 ≥ SILENCE → 空读返回（不烧预算）；
    /// 5. 预算耗尽 → busy=true 部分输出。
    async fn read_loop(
        &self,
        session: &Arc<PtySession>,
        read_budget_ms: Option<u64>,
    ) -> Result<PtySessionOutput, AgentError> {
        let budget = read_budget_ms
            .unwrap_or(READ_BUDGET_DEFAULT_MS)
            .min(READ_BUDGET_MAX_MS);
        let deadline = Instant::now() + Duration::from_millis(budget);
        let started = Instant::now();
        let mut new_text = String::new();
        let marker_baseline = lock(&session.buf).marker_seen;
        let pid = session
            .child
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|c| c.process_id());
        let mut children: Option<bool> = None;
        let mut next_children_poll = Instant::now();
        let mut busy = false;

        loop {
            {
                let mut b = lock(&session.buf);
                new_text.push_str(&b.take_text());
            }
            let (marker_now, eof) = {
                let b = lock(&session.buf);
                (b.marker_seen, b.eof)
            };
            let silence = lock(&session.buf).last_output.elapsed();
            let alive = session.alive();
            let marker_fired = marker_now > marker_baseline;

            if eof || !alive {
                break;
            }
            if next_children_poll <= Instant::now() {
                children = has_live_children(pid);
                next_children_poll = Instant::now() + Duration::from_millis(CHILDREN_POLL_MS);
            }
            if marker_fired && !new_text.is_empty() {
                break;
            }
            if !new_text.is_empty() && silence >= Duration::from_millis(SILENCE_MS) && children != Some(true)
            {
                break;
            }
            if !new_text.is_empty() && silence >= Duration::from_millis(QUIET_ALIVE_MS) {
                break;
            }
            if new_text.is_empty()
                && silence >= Duration::from_millis(SILENCE_MS)
                && started.elapsed() >= Duration::from_millis(SILENCE_MS)
            {
                break;
            }
            if Instant::now() >= deadline {
                busy = alive && !marker_fired;
                break;
            }
            tokio::time::sleep(Duration::from_millis(READ_TICK_MS)).await;
        }

        // 收尾再排一次（循环最后一 tick 之后可能又有新字节）。
        new_text.push_str(&lock(&session.buf).take_text());

        Ok(PtySessionOutput {
            session_id: session.id.clone(),
            text: new_text,
            busy,
            alive: session.alive(),
        })
    }

    /// 回收空闲超 TTL 的会话（挂到各入口做懒清扫）。
    fn reap_idle(&self) {
        let expired: Vec<Arc<PtySession>> = {
            let mut sessions = lock(&self.sessions);
            let ids: Vec<String> = sessions
                .iter()
                .filter(|(_, s)| lock(&s.buf).last_output.elapsed() > Duration::from_secs(SESSION_TTL_SECS))
                .map(|(k, _)| k.clone())
                .collect();
            ids.into_iter().filter_map(|k| sessions.remove(&k)).collect()
        };
        for s in expired {
            tracing::info!(session = %s.id, "reaping idle persistent terminal session");
            s.shutdown();
        }
    }
}

fn unknown_session(session_id: &str) -> AgentError {
    AgentError::Io(format!(
        "unknown persistent session id '{session_id}' — the session may have been \
         closed or reaped after 30min idle. Create a new one with persistent: true."
    ))
}

/// shell 是否还有存活的子进程（Toolhelp32 快照）。
/// `None` = 快照失败（能力不可用），调用方退化为纯静默判定。
#[cfg(windows)]
fn has_live_children(pid: Option<u32>) -> Option<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    let pid = pid?;
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut entry) != 0;
        while ok {
            if entry.th32ParentProcessID == pid {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut entry) != 0;
        }
        CloseHandle(snap);
        Some(found)
    }
}

#[cfg(not(windows))]
fn has_live_children(_pid: Option<u32>) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_stripped_mid_line_keeps_echo() {
        let mut b = SessionBuffer::new();
        // append_filtered 的输入是 VtFilter 之后的文本（CR 已归一为 LF）。
        b.append_filtered("HERMIXPTY-pty-1> echo hi\noutput\n", "HERMIXPTY-pty-1");
        // 提示符被剥掉、回显保留、输出完整。
        assert_eq!(b.text, "echo hi\noutput\n");
        assert_eq!(b.marker_seen, 1);
    }

    #[test]
    fn marker_split_across_feeds_still_stripped() {
        let mut b = SessionBuffer::new();
        b.append_filtered("HERMIXPTY-", "HERMIXPTY-pty-1");
        b.append_filtered("pty-1> ", "HERMIXPTY-pty-1");
        assert_eq!(b.marker_seen, 1);
        assert!(b.line_buf.is_empty());
    }

    #[test]
    fn partial_prompt_line_flushes_without_marker() {
        let mut b = SessionBuffer::new();
        b.append_filtered("plain line", "HERMIXPTY-pty-1");
        b.flush_partial_line("HERMIXPTY-pty-1");
        assert_eq!(b.text, "plain line");
    }

    #[test]
    fn empty_marker_is_noop() {
        let mut b = SessionBuffer::new();
        b.append_filtered("HERMIXPTY-x> keep", "");
        assert_eq!(b.text, "");
        b.flush_partial_line("");
        assert_eq!(b.text, "HERMIXPTY-x> keep");
        assert_eq!(b.marker_seen, 0);
    }

    #[test]
    fn buffer_cap_drops_oldest_and_flags_overflow() {
        let mut b = SessionBuffer::new();
        b.append_filtered(&"x".repeat(SESSION_BUFFER_CAP + 4096), "");
        assert!(b.overflow);
        assert!(b.text.len() <= SESSION_BUFFER_CAP);
        assert!(b.take_text().starts_with("[...older output dropped"));
    }
}
