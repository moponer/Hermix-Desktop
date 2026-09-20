//! stdio_bridge — Tauri 主进程与 hermix.exe（`--stdio` 模式）之间的桥。
//!
//! 职责：
//! - 持有子进程 stdin（写入帧）与 stdout（独立线程逐行读帧）
//! - `http` 通道：request-response 配对（request_id → oneshot 应答）
//! - 其余通道：`AppHandle::emit("stdio-frame", …)` 推给前端订阅
//!
//! 协议见 `hermix-server/src/stdio_session.rs` 模块文档。

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// 当前活跃的桥句柄。用 `RwLock` 而不是 `OnceLock`：sidecar 重启会换一个
/// 子进程，stdin/stdout 随之失效，桥必须能被**替换**。`OnceLock` 只允许
/// 写入一次，重启后 `init_global` 永久失败 → 旧桥（连着已死子进程的
/// stdin）继续被 `global()` 返回，所有请求写进黑洞，
/// "断开连接" 也就永远重连不上。
static BRIDGE: RwLock<Option<StdioBridge>> = RwLock::new(None);

/// 最近一次 `init_global` 是否成功接管了一个**存活**的子进程。
/// 与 `BRIDGE` 分开存：`BRIDGE` 里的桥可能指向刚退出的子进程，
/// 而 `global().is_some()` 无法区分这一点。
static BRIDGE_LIVE: AtomicBool = AtomicBool::new(false);

/// 桥的代次序号。每次 `init_global` 递增，赋给新建的桥。
/// 用于让旧代的读线程在退出时无法误杀新一代（见 `mark_dead_if_current`）。
static BRIDGE_GEN: AtomicU64 = AtomicU64::new(0);

/// 全局桥句柄。sidecar 以 `--stdio` 模式 spawn 成功后调用 `init_global`；
/// tauri command 与流式上行统一走 `global()`。
///
/// 重启场景：新一代替换旧一代。旧桥的 stdout 读线程会因子进程退出而结束，
/// 其 `pending_http` 等待者由 `spawn_reader` 的收尾逻辑统一唤醒，不会悬挂。
pub fn init_global(app: AppHandle, stdin: ChildStdin, stdout: ChildStdout) -> bool {
    // 先推进代次再建桥，保证读线程拿到的是自己那一代。
    let gen = BRIDGE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let bridge = StdioBridge::launch(app, stdin, stdout, gen);
    match BRIDGE.write() {
        Ok(mut slot) => {
            // 替换即视为接管成功：旧代（若有）由调用方 `stop()` 负责收尸。
            *slot = Some(bridge);
            BRIDGE_LIVE.store(true, Ordering::SeqCst);
            true
        }
        Err(_) => false,
    }
}

/// 桥未初始化（backend 非 stdio 模式 / 尚未 spawn）时返回 None。
/// 返回克隆而非引用：调用方常在持有期间做 `.await`，持锁会阻塞重启。
pub fn global() -> Option<StdioBridge> {
    BRIDGE.read().ok().and_then(|g| g.clone())
}

/// 当前桥是否指向一个仍存活的子进程。`false` 时所有经桥的请求都会失败，
/// 调用方应据此报告离线并触发重启，而不是假装健康。
pub fn is_live() -> bool {
    BRIDGE_LIVE.load(Ordering::SeqCst)
}

/// 子进程退出 / 桥被弃用时调用，标记当前代已死。
pub fn mark_dead() {
    BRIDGE_LIVE.store(false, Ordering::SeqCst);
}

/// 只有调用方仍是**当前代**时才标记死亡。旧代读线程在新代接管后退出
/// 是正常现象（它属于已被替换的那一代），不能因此把新代判死。
fn mark_dead_if_current(gen: u64) {
    if BRIDGE_GEN.load(Ordering::SeqCst) == gen {
        BRIDGE_LIVE.store(false, Ordering::SeqCst);
    }
}

/// stdout 帧的信封（出站方向由 server 生成）。
#[derive(Debug, Clone, Deserialize)]
pub struct OutboundFrame {
    pub channel: String,
    #[serde(default)]
    pub request_id: Option<String>,
    pub payload: String,
}

/// 推送给前端的事件载荷（`stdio-frame` 事件）。
#[derive(Debug, Clone, Serialize)]
pub struct FrontendFrame {
    pub channel: String,
    pub payload: String,
}

struct BridgeInner {
    stdin: Mutex<Option<ChildStdin>>,
    /// http 通道的等待者：request_id → 应答发送端。读线程命中即配对完成。
    pending_http: Mutex<HashMap<String, tokio::sync::oneshot::Sender<String>>>,
}

/// 可全局共享的桥句柄。`http_request` 供 tauri command 调用；
/// `write_frame` 供流式通道（stream/groupchat）的前端上行消息使用。
#[derive(Clone)]
pub struct StdioBridge {
    inner: Arc<BridgeInner>,
    app: AppHandle,
    next_id: Arc<AtomicU64>,
    /// 本代的序号（见 `BRIDGE_GEN`）。读线程用它判断自己是否已被替换。
    generation: u64,
}

impl StdioBridge {
    /// 接管子进程的 stdin/stdout 并启动读线程。必须在 spawn 后、
    /// 写入第一帧前调用一次。
    pub fn launch(app: AppHandle, stdin: ChildStdin, stdout: ChildStdout, generation: u64) -> Self {
        let bridge = Self {
            inner: Arc::new(BridgeInner {
                stdin: Mutex::new(Some(stdin)),
                pending_http: Mutex::new(HashMap::new()),
            }),
            app,
            next_id: Arc::new(AtomicU64::new(1)),
            generation,
        };
        bridge.spawn_reader(stdout);
        bridge
    }

    fn spawn_reader(&self, stdout: ChildStdout) {
        let inner = self.inner.clone();
        let app = self.app.clone();
        // 本代序号：读线程退出时只有"仍是当前代"才允许标记 bridge 已死。
        // 没有这个守卫会出现竞态——旧代读线程在新代接管后才退出，
        // 顺手把新代误判为死亡，前端立刻报离线。
        let my_gen = self.generation;
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) => break, // stdout 关闭（子进程退出）
                    Ok(_) => {}
                    Err(_) => break,
                }
                let Ok(frame) = serde_json::from_str::<OutboundFrame>(line.trim()) else {
                    eprintln!("[stdio-bridge] unparseable frame, ignored");
                    continue;
                };
                if frame.channel == "http" {
                    // REST 应答：按 request_id 配对交付。
                    let id = frame.request_id.unwrap_or_default();
                    let sender = inner
                        .pending_http
                        .lock()
                        .ok()
                        .and_then(|mut p| p.remove(&id));
                    if let Some(tx) = sender {
                        let _ = tx.send(frame.payload);
                    } else {
                        eprintln!("[stdio-bridge] http reply without waiter, id={id}");
                    }
                } else {
                    // 流式/主通道推送：转发给前端订阅者。
                    let _ = app.emit(
                        "stdio-frame",
                        FrontendFrame {
                            channel: frame.channel,
                            payload: frame.payload,
                        },
                    );
                }
            }
            // stdout 关闭：唤醒所有等待者以免永久挂起，并把本代标记为已死。
            // 标记必须在这里做（子进程退出的唯一可观测点），否则
            // `backend_ready` 会继续对一个已经死掉的子进程报 "在线"。
            mark_dead_if_current(my_gen);
            if let Ok(mut p) = inner.pending_http.lock() {
                for (_, tx) in p.drain() {
                    let _ = tx.send("{\"status\":503,\"body\":\"backend exited\",\"enc\":\"utf8\"}".into());
                }
            }
            eprintln!("[stdio-bridge] stdout reader exited");
        });
    }

    /// 写一帧到子进程 stdin。
    fn write_frame(&self, channel: &str, request_id: Option<&str>, payload: &str) -> Result<(), String> {
        let envelope = serde_json::json!({
            "channel": channel,
            "request_id": request_id,
            "payload": payload,
        })
        .to_string();
        let mut guard = self
            .inner
            .stdin
            .lock()
            .map_err(|_| "stdio bridge poisoned".to_string())?;
        let stdin = guard
            .as_mut()
            .ok_or_else(|| "backend stdin closed".to_string())?;
        stdin
            .write_all(envelope.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("write stdin: {e}"))
    }

    /// REST 请求-响应：写 http 帧 → 等 request_id 配对的应答 payload。
    pub async fn http_request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<String, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let payload = serde_json::json!({
            "method": method,
            "path": path,
            "body": body,
            "request_id": id,
        })
        .to_string();

        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        self.inner
            .pending_http
            .lock()
            .map_err(|_| "stdio bridge poisoned".to_string())?
            .insert(id.clone(), tx);

        self.write_frame("http", Some(&id), &payload)?;

        match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(_)) => Err("http waiter dropped".into()),
            Err(_) => {
                // 超时清理挂起项，避免泄漏。
                if let Ok(mut p) = self.inner.pending_http.lock() {
                    p.remove(&id);
                }
                Err("backend request timed out".into())
            }
        }
    }

    /// 流式通道上行（前端 → server）：payload 原样透传。
    pub fn send_to_channel(&self, channel: &str, payload: &str) -> Result<(), String> {
        self.write_frame(channel, None, payload)
    }
}
