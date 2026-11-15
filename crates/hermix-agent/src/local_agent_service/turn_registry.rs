//! 轮次句柄注册表：中断控制器、steer 缓冲、轮次终态记录。
//!
//! 从 `local_agent_service` 搬出的第五批（P1-13）。这批方法都只碰两个
//! `Arc<Mutex<HashMap<..>>>` 注册表（`active_interrupts` / `steer_buffers`）
//! 与 `task_record_sink`，与 LLM 调用、工具执行无关。
//!
//! 为什么放子模块而不是继续堆在组合根：中断 / steer / 轮次记录是**同一套
//! 会话生命周期**的三面，合并在一处才看得见"登记与销号必须成对"这条约束
//! （`register_active` 与 `unregister_active` 之间是整轮执行）。
//!
//! 字段仍是父模块私有的 —— 子模块可以访问父模块的私有字段，所以搬家不需要
//! 放宽任何可见性。

use crate::agent_loop::AgentLoop;

use super::LocalAgentService;

/// Take `sid`'s controller out of the interrupt registry and signal it.
///
/// Split out of the `LocalAgentService` methods so the registry semantics
/// ("signal exactly the one named turn, and only while it's still there")
/// are unit-testable without standing up a whole service. A poisoned lock
/// and "no such session" both report "nothing to interrupt".
/// 轮次终态。**不收结果类型**：同步路径 `run` 返 `AgentResult`、流式路径
/// `run_stream` 返 `AgentReply`，两者字段同构但类型不同——终态只需要三态，
/// 所以在调用点折算一次，这里只管三态。
#[derive(Debug, Clone, PartialEq)]
pub(super) enum TurnOutcome {
    Completed,
    Interrupted,
    Failed(String),
}

impl TurnOutcome {
    pub(super) fn from_parts(user_interrupted: bool, last_error: Option<&str>) -> Self {
        if user_interrupted {
            Self::Interrupted
        } else if let Some(e) = last_error {
            Self::Failed(e.to_string())
        } else {
            Self::Completed
        }
    }

    /// 两条执行路径（`run` / `run_stream`）都返 `AgentResult`；`AgentReply`
    /// 是 `send_message_stream` 在末尾从 `AgentResult` 转出来的，所以这里
    /// 根本见不到它。
    ///
    /// `Err` 也要折算——调用点随后就 `result?` 提前 return 了，终态必须在
    /// 解包之前落，否则失败 / 中断的轮次永远停在 `Running`，会被误判成
    /// 「进程崩了没跑完」。
    pub(super) fn of_result(r: &Result<hermix_core::types::AgentResult, hermix_core::AgentError>) -> Self {
        match r {
            Ok(res) => Self::from_parts(
                hermix_core::traits::is_user_interrupted(res.interrupted, res.finish_reason.as_deref()),
                res.last_error.as_deref(),
            ),
            Err(hermix_core::AgentError::Interrupted { .. }) => Self::Interrupted,
            Err(e) => Self::Failed(e.to_string()),
        }
    }
}

pub(super) fn signal_interrupt(
    registry: &std::sync::Mutex<std::collections::HashMap<String, crate::interrupt::InterruptController>>,
    sid: &str,
) -> bool {
    let taken = match registry.lock() {
        Ok(mut guard) => guard.remove(sid),
        Err(_) => None,
    };
    match taken {
        Some(ctrl) => {
            ctrl.interrupt(None);
            true
        }
        None => false,
    }
}

/// 取出该会话这一轮的 steer 缓冲句柄；不在跑就没有。
///
/// 抽成自由函数是为了能直接单测"命中的是不是指定那个会话"——`AgentLoop`
/// 本身构造不出来，而这块注册表就是普通的 `HashMap`。
pub(super) fn take_steer_buffer(
    registry: &std::sync::Mutex<
        std::collections::HashMap<String, std::sync::Arc<std::sync::Mutex<Option<String>>>>,
    >,
    sid: &str,
) -> Option<std::sync::Arc<std::sync::Mutex<Option<String>>>> {
    registry.lock().ok()?.get(sid).cloned()
}

impl LocalAgentService {
    /// Interrupt **one** session's in-flight turn, leaving every other
    /// running turn alone. Returns `true` when a live turn was found and
    /// signalled, `false` when nothing was running for that id.
    ///
    /// The controller leaves the registry as it gets signalled: that turn
    /// is on its way out, and keeping it would let a second `/stop` claim a
    /// hit against a turn that already stopped.
    pub fn interrupt_session(&self, sid: &str) -> bool {
        let hit = signal_interrupt(&self.active_interrupts, sid);
        if hit {
            // 中断之后不该还能 steer：缓冲留着只会让文本悄悄消失——用户以为
            // 改进了这轮，其实它永远不会落地。
            self.steer_buffers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(sid);
        }
        hit
    }

    /// 往该会话**正在跑**的这一轮里注入一条指引（`/queue steer`）。
    ///
    /// 与 `interrupt_session` 是两种不同的手法：轮次不中止，指引会落进
    /// 下一个 tool result，用户可以在不丢掉已完成工作的前提下纠偏。
    /// 返回 `false` = 没有在跑的轮次，或文本是空的。
    pub fn steer_session(&self, sid: &str, text: &str) -> bool {
        match take_steer_buffer(&self.steer_buffers, sid) {
            Some(buf) => crate::steer::steer(&buf, text),
            None => false,
        }
    }

    /// 登记一轮的「可被外部按会话操作」的两个句柄。中断控制器（关机 / `/stop`
    /// 要）和 steer 缓冲（`/queue steer` 要）生命周期完全一致，合并到一处，
    /// 免得两份注册表各自漂移出一半。
    pub(super) fn register_active(&self, sid: &str, agent: &AgentLoop) {
        let mut interrupts = self.active_interrupts.lock().unwrap_or_else(|e| e.into_inner());
        let mut steers = self.steer_buffers.lock().unwrap_or_else(|e| e.into_inner());
        interrupts.insert(sid.to_string(), agent.interrupt.clone());
        steers.insert(sid.to_string(), agent.steer_buffer());
    }

    /// `register_active` 的反面。轮次结束**无论成功失败**都要调。
    pub(super) fn unregister_active(&self, sid: &str) {
        let mut interrupts = self.active_interrupts.lock().unwrap_or_else(|e| e.into_inner());
        let mut steers = self.steer_buffers.lock().unwrap_or_else(|e| e.into_inner());
        interrupts.remove(sid);
        steers.remove(sid);
    }

    /// 开一条**主对话轮次**的 `TaskRecord`，返回记录 id（无 sink 时 `None`）。
    ///
    /// 此前 `task_records` 表只被 sub-agent（`delegate_task`）写，所以
    /// "上一轮对话跑完没有" 只能靠「最后一条 user 消息之后有没有 assistant」
    /// 这种带误报源的启发式判。主轮次也落一条之后，`check_last_turn_outcome`
    /// 就能直接读终态。id 用 `turn-` 前缀与 sub-agent 记录区分。
    pub(super) fn begin_turn_record(&self, sid: &str, task: &str) -> Option<String> {
        let sink = self.task_record_sink.as_ref()?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = format!(
            "{}{sid}-{nanos}",
            hermix_core::traits::TURN_ID_PREFIX
        );
        sink.started(&id, Some(sid), task);
        Some(id)
    }

    /// 收掉 `begin_turn_record` 开的那条记录。三态都落，映射到
    /// `TaskOutcome`：成功 → Completed，用户叫停 → Aborted，失败 → Faulted。
    ///
    /// 进程崩溃 / future 被 drop 时**不会**走到这里，记录停在 `Running`——
    /// 这正是「非终态 = 上一轮没跑完」这条判据要的信号。
    pub(super) fn end_turn_record(&self, turn: Option<&str>, outcome: TurnOutcome) {
        let (Some(id), Some(sink)) = (turn, self.task_record_sink.as_ref()) else {
            return;
        };
        match outcome {
            TurnOutcome::Completed => sink.terminal(id, "completed", None),
            TurnOutcome::Interrupted => sink.terminal(id, "aborted", Some("用户叫停")),
            TurnOutcome::Failed(e) => sink.terminal(id, "failed", Some(&e)),
        }
    }

    /// Interrupt all active agent tasks so they gracefully stop and
    /// persist their partial output. Called by the server's shutdown
    /// path before the process exits. Each registered `InterruptController`
    /// receives `interrupt(None)`, which causes the agent loop's
    /// `tokio::select!` / `take_interrupt_graceful()` check to fire,
    /// returning `graceful_interrupt_result` — and the caller
    /// (`send_message_stream`) then persists the ctx messages to the DB.
    ///
    /// This one stays "all" on purpose: shutdown means stop everything,
    /// whoever started it. Per-session stops go through `interrupt_session`.
    pub(super) fn interrupt_all(&self) {
        let sids: Vec<String> = match self.active_interrupts.lock() {
            Ok(guard) => guard.keys().cloned().collect(),
            Err(_) => return,
        };
        for sid in &sids {
            self.interrupt_session(sid);
        }
        if !sids.is_empty() {
            tracing::info!(
                active_sessions = sids.len(),
                "interrupt_all: signalled all active agent tasks to gracefully stop for persistence"
            );
        }
    }
}
