//! The single thread that owns the UIA COM apartment.
//!
//! COM objects are bound to the apartment they were created on, and a
//! UIA provider can block for as long as the target app takes to answer.
//! Calling UIA straight from the agent's async executor is what hung a
//! whole run in the deleted `hermix-cua` (commit `c6442032`), so every call
//! is funnelled through one thread with a hard deadline; when the deadline
//! passes the caller gets an error and the thread is retired.

use super::uia::Session;
use hermix_core::ToolError;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::oneshot;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Deadline for one UIA operation. Generous for a real app to answer,
/// short enough that a wedged one costs the agent one failed call.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(8);

/// Queue depth. Bounded on purpose: an unbounded queue would let a caller
/// keep piling work onto a thread that will never drain it.
const QUEUE: usize = 16;

/// How often an idle worker refreshes its heartbeat.
const TICK: Duration = Duration::from_millis(250);

/// Two timeouts in a row is not a slow app, it's a dead apartment.
const TIMEOUTS_BEFORE_RESTART: u32 = 2;

/// Liveness tick. It only goes stale if the thread is wedged *inside* a COM
/// call or died, because the thread refreshes it while idle too.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub type Job = Box<dyn FnOnce(&mut Session) + Send>;

pub struct UiaWorker {
    tx: SyncSender<Job>,
    heartbeat: Arc<AtomicU64>,
    /// Bumped whenever a job panics, so the error the caller sees can name
    /// the apartment it died in.
    generation: Arc<AtomicU64>,
    timeouts: Arc<AtomicU32>,
}

static WORKER: OnceLock<Mutex<Option<Arc<UiaWorker>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Arc<UiaWorker>>> {
    WORKER.get_or_init(|| Mutex::new(None))
}

/// The process-wide worker, started on first use.
///
/// `call` restarts it on demand, so a caller never has to check liveness.
pub fn worker() -> Result<Arc<UiaWorker>, ToolError> {
    let mut g = slot()
        .lock()
        .map_err(|_| ToolError::ExecutionFailed("UIA 全局 worker 锁已中毒".into()))?;
    if let Some(w) = g.as_ref() {
        return Ok(w.clone());
    }
    let fresh = Arc::new(spawn()?);
    *g = Some(fresh.clone());
    Ok(fresh)
}

fn spawn() -> Result<UiaWorker, ToolError> {
    let (tx, rx) = sync_channel::<Job>(QUEUE);
    let heartbeat = Arc::new(AtomicU64::new(now_ms()));
    let generation = Arc::new(AtomicU64::new(0));
    let (hb, gen) = (heartbeat.clone(), generation.clone());
    std::thread::Builder::new()
        .name("hermix-uia".into())
        .spawn(move || serve(rx, hb, gen))
        .map_err(|e| ToolError::ExecutionFailed(format!("无法创建 UIA 工作线程：{e}")))?;
    Ok(UiaWorker {
        tx,
        heartbeat,
        generation,
        timeouts: Arc::new(AtomicU32::new(0)),
    })
}

/// Replace `dead` with a fresh thread, unless someone else already did.
///
/// The old thread is retired by **dropping** its `SyncSender`: an idle one
/// exits on its next tick, and one blocked inside COM finishes its call
/// first. Killing it outright is not an option — releasing COM objects from
/// another thread is exactly how you turn one wedged call into a process
/// crash.
fn respawn(dead: &UiaWorker) -> Option<Arc<UiaWorker>> {
    let mut g = slot().lock().ok()?;
    match g.as_ref() {
        Some(cur) if std::ptr::eq(Arc::as_ptr(cur), dead as *const UiaWorker) => {}
        // Another caller already replaced it; use theirs rather than
        // starting a second thread that would race for the same apartment.
        Some(cur) => return Some(cur.clone()),
        None => return None,
    }
    let fresh = Arc::new(spawn().ok()?);
    *g = Some(fresh.clone());
    Some(fresh)
}

/// Drop a dead worker from the slot so the next call starts a new thread.
fn forget(dead: &UiaWorker) {
    if let Ok(mut g) = slot().lock() {
        if g.as_ref().is_some_and(|cur| std::ptr::eq(Arc::as_ptr(cur), dead as *const UiaWorker)) {
            *g = None;
        }
    }
}

fn serve(rx: Receiver<Job>, hb: Arc<AtomicU64>, gen: Arc<AtomicU64>) {
    // MTA is what the UIA client guidance recommends: an STA worker needs a
    // message pump, and a provider that re-enters our apartment
    // synchronously deadlocks a pump-less STA outright.
    // `is_err()` also rejects RPC_E_CHANGED_MODE, the one case where this
    // thread inherited an STA from something else — UIA misbehaves there.
    let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if hr.is_err() {
        tracing::error!(hr = hr.0, "CoInitializeEx(COINIT_MULTITHREADED) 失败，UIA 工作线程退出");
        return;
    }
    let mut session = match Session::new() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "UIA 会话初始化失败，工作线程退出");
            unsafe { CoUninitialize() };
            return;
        }
    };

    loop {
        let job = match rx.recv_timeout(TICK) {
            Ok(job) => job,
            // Idle, not stuck: keep the watchdog fed.
            Err(RecvTimeoutError::Timeout) => {
                hb.store(now_ms(), Ordering::Release);
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        hb.store(now_ms(), Ordering::Release);

        // A panic can leave the apartment holding a proxy to a dead
        // provider. Reusing the session would fail every later call the
        // same way, so rebuild it; dying here would take the whole agent
        // process down for one bad element.
        if panic::catch_unwind(AssertUnwindSafe(|| job(&mut session))).is_err() {
            gen.fetch_add(1, Ordering::SeqCst);
            match Session::new() {
                Ok(fresh) => session = fresh,
                Err(e) => {
                    tracing::error!(error = %e, "UIA 会话重建失败，工作线程退出");
                    break;
                }
            }
        }
        hb.store(now_ms(), Ordering::Release);
    }

    // Release COM before leaving the apartment, or the finalizer runs in an
    // apartment that is already gone.
    drop(session);
    unsafe { CoUninitialize() };
}

impl UiaWorker {
    /// Run `f` on the UIA thread and wait at most `timeout` for it.
    ///
    /// `f` takes `&mut Session` because COM handles are apartment-bound:
    /// they must be created, used and released on this thread.
    pub async fn call<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&mut Session) -> Result<T, ToolError> + Send + 'static,
    ) -> Result<T, ToolError> {
        if self.unhealthy(timeout) {
            tracing::warn!(
                timeouts = self.timeouts.load(Ordering::Relaxed),
                idle_ms = now_ms().saturating_sub(self.heartbeat.load(Ordering::Acquire)),
                "UIA 工作线程疑似卡死，重建线程"
            );
            if let Some(fresh) = respawn(self) {
                return fresh.dispatch(timeout, f).await;
            }
        }
        self.dispatch(timeout, f).await
    }

    fn unhealthy(&self, timeout: Duration) -> bool {
        self.timeouts.load(Ordering::Relaxed) >= TIMEOUTS_BEFORE_RESTART
            || now_ms().saturating_sub(self.heartbeat.load(Ordering::Acquire))
                > timeout.as_millis() as u64
    }

    async fn dispatch<T: Send + 'static>(
        &self,
        timeout: Duration,
        f: impl FnOnce(&mut Session) -> Result<T, ToolError> + Send + 'static,
    ) -> Result<T, ToolError> {
        let (tx, rx) = oneshot::channel();
        let job: Job = Box::new(move |s| {
            let _ = tx.send(f(s));
        });

        // try_send, never send: a *blocking* send on a full queue is the
        // exact "the agent loop never returns" failure this worker exists to
        // prevent.
        if let Err(err) = self.tx.try_send(job) {
            return Err(match err {
                TrySendError::Full(_) => ToolError::Timeout(format!(
                    "UIA 工作线程忙（队列 {QUEUE} 已满），请稍后重试"
                )),
                TrySendError::Disconnected(_) => {
                    forget(self);
                    ToolError::ExecutionFailed(
                        "UIA 工作线程已退出（COM/UIA 初始化失败），本次调用未执行".into(),
                    )
                }
            });
        }
        self.heartbeat.store(now_ms(), Ordering::Release);

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => {
                self.timeouts.store(0, Ordering::Relaxed);
                result
            }
            // The job owned the sender, so a closed channel means the thread
            // panicked mid-call.
            Ok(Err(_)) => Err(ToolError::ExecutionFailed(format!(
                "UIA 工作线程在执行中崩溃（会话代次 {}），本次调用未完成",
                self.generation.load(Ordering::SeqCst)
            ))),
            Err(_) => {
                self.timeouts.fetch_add(1, Ordering::Relaxed);
                Err(ToolError::Timeout("UIA 调用超时（目标程序无响应）".into()))
            }
        }
    }
}
