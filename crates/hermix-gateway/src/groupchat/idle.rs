use std::sync::Arc;
use tokio::sync::RwLock;
use std::collections::HashMap;
use std::time::Duration;
use chrono::Utc;

use super::dispatcher::AgentDispatcher;
use super::persistence::*;

/// P1.3 — fires after an idle chat message is successfully produced by
/// the dispatcher. Receives `(room_id, agent_id, content)`. The hook
/// is the place where the message gets persisted to the room and any
/// side effects (rank adjustments, etc.) are applied. Implementations
/// must be cheap to call (the dispatcher response is already in hand);
/// long work should be spawned into a separate task.
pub type IdlePublishHook = Arc<dyn Fn(&str, &str, &str) + Send + Sync>;

/// Manages idle chat mode for group chat rooms.
///
/// When `allow_idle_chat` is enabled, periodically triggers selected agents
/// to generate casual messages, share insights, or discuss topics.
/// Messages are saved to the room and each agent's memory.
pub struct IdleChatManager {
    dispatcher: Arc<dyn AgentDispatcher>,
    /// P1.3 — fired on every successful dispatch. Set by the owner via
    /// `set_publish_hook`. When unset, idle chat still runs but the
    /// generated message is dropped (preserves pre-P1.3 behaviour).
    publish_hook: parking_lot::Mutex<Option<IdlePublishHook>>,
    /// Map of room_id -> JoinHandle for the idle chat timer.
    timers: RwLock<HashMap<String, tokio::task::JoinHandle<()>>>,
}

impl IdleChatManager {
    pub fn new(dispatcher: Arc<dyn AgentDispatcher>) -> Self {
        Self {
            dispatcher,
            publish_hook: parking_lot::Mutex::new(None),
            timers: RwLock::new(HashMap::new()),
        }
    }

    /// P1.3 — install a hook that fires after every successful idle
    /// chat dispatch. Replaces any previous hook. The hook receives
    /// `(room_id, agent_id, content)` and is expected to be quick —
    /// heavier work (db writes, score adjustments) should be done
    /// inside `tokio::spawn` from the hook body so the dispatcher
    /// future returns promptly.
    pub fn set_publish_hook(&self, hook: IdlePublishHook) {
        *self.publish_hook.lock() = Some(hook);
    }

    /// Start idle chat timer for a room.
    pub async fn start_room(&self, room_id: &str, interval_secs: u64, agents: Vec<AgentRoomProfile>) {
        let dispatcher = self.dispatcher.clone();
        let room_id_owned = room_id.to_string();
        let interval = Duration::from_secs(interval_secs);
        let agents_owned = agents.clone();
        // P1.3 — pre-clone the publish hook once at start so the tick
        // loop can fire it without borrowing `self` across the
        // `tokio::spawn` boundary (which would be a compile error).
        // Snapshotted at start; if the manager replaces the hook
        // mid-loop the in-flight timer continues with the old hook
        // (by design — replacing the hook mid-room is rare, calling
        // `start_room` again picks up the new hook).
        let hook_for_tick: Option<IdlePublishHook> = self.publish_hook.lock().clone();

        let handle = tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                tick_idle_round(&dispatcher, hook_for_tick.as_ref(), &room_id_owned, &agents_owned).await;
            }
        });

        let mut timers = self.timers.write().await;
        // Stop existing timer for this room
        if let Some(old) = timers.remove(room_id) {
            old.abort();
        }
        timers.insert(room_id.to_string(), handle);
    }

    /// Stop idle chat for a room.
    pub async fn stop_room(&self, room_id: &str) {
        let mut timers = self.timers.write().await;
        if let Some(handle) = timers.remove(room_id) {
            handle.abort();
        }
    }

    /// Stop all idle timers.
    pub async fn stop_all(&self) {
        let mut timers = self.timers.write().await;
        for (_, handle) in timers.drain() {
            handle.abort();
        }
    }
}

/// P1.3 — one round of idle chat. Selects 0-2 agents, dispatches the
/// prompt to each, fires the publish hook on success. Free function
/// (no `&self`) so the long-lived timer in `start_room` can call it
/// inside `tokio::spawn` without borrowing the manager across the
/// `'static` boundary. Hook failures are swallowed (traced) so a
/// broken rank writer can't kill the idle loop.
pub(super) async fn tick_idle_round(
    dispatcher: &Arc<dyn AgentDispatcher>,
    hook: Option<&IdlePublishHook>,
    room_id: &str,
    agents: &[AgentRoomProfile],
) {
    let selected = select_idle_agents(agents);
    if selected.is_empty() {
        return;
    }
    for agent in &selected {
        let prompt = build_idle_prompt(&agent.role);
        match dispatcher.dispatch_to_agent(
            room_id,
            &agent.agent_id,
            &agent.session_id,
            &format!(
                "你是{}，现在是闲聊时间。你可以分享想法、总结经验、或学习新内容。消息会保存到你的记忆中。",
                agent.agent_id
            ),
            &prompt,
            "",
        ).await {
            Ok(response) => {
                tracing::debug!(
                    "Idle chat from {} in room {}: {}",
                    agent.agent_id, room_id,
                    &response[..response.len().min(100)]
                );
                if let Some(hook) = hook {
                    hook(room_id, &agent.agent_id, &response);
                }
            }
            Err(e) => {
                tracing::warn!("Idle chat failed for {}: {}", agent.agent_id, e);
            }
        }
    }
}

/// Select 1-2 agents for idle chat based on role.
/// — uses round-robin instead of `timestamp % len` so
/// consecutive ticks always pick a different member when possible.
fn select_idle_agents(agents: &[AgentRoomProfile]) -> Vec<AgentRoomProfile> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static RR: AtomicUsize = AtomicUsize::new(0);

    if agents.is_empty() {
        return vec![];
    }

    let mut selected = Vec::new();

    // Prefer members for idle chat
    let members: Vec<_> = agents.iter().filter(|a| a.role == AgentRole::Member).collect();
    if !members.is_empty() {
        let idx = RR.fetch_add(1, Ordering::Relaxed) % members.len();
        selected.push(members[idx].clone());
    }

    // Maybe add a second agent (30% chance)
    if selected.len() < 2 && agents.len() > 1 {
        let step = RR.load(Ordering::Relaxed);
        let candidates: Vec<_> = agents.iter()
            .filter(|a| !selected.iter().any(|s| s.agent_id == a.agent_id))
            .collect();
        if !candidates.is_empty() && step % 3 == 0 {
            let idx = step % candidates.len();
            selected.push(candidates[idx].clone());
        }
    }

    selected
}

/// Build an idle chat prompt based on agent role.
fn build_idle_prompt(role: &AgentRole) -> String {
    match role {
        AgentRole::Member => {
            let topics = [
                "分享一下最近学到的有用技巧",
                "总结一下最近完成的任务经验",
                "思考一下如何提高工作效率",
                "讨论一下当前项目的改进想法",
                "回顾一下最近的对话，有什么值得记录的",
            ];
            let idx = (Utc::now().timestamp_millis() as usize) % topics.len();
            topics[idx].to_string()
        }
        AgentRole::Leader => {
            "回顾一下团队近期的表现，思考如何优化协作流程".to_string()
        }
        AgentRole::Observer => {
            "观察团队的讨论，记录有价值的见解".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    //! P1.3 — verify the publish hook fires exactly once per
    //! successful dispatch, and is NOT fired when the dispatcher
    //! returns an error. These exercise the extracted `tick_idle_round`
    //! free function directly (no `tokio::time::interval`, no real
    //! dispatcher, no db) so they finish in microseconds.

    use super::*;
    use async_trait::async_trait;
    use parking_lot::Mutex as PlMutex;
    use std::sync::Arc as StdArc;

    /// Minimal dispatcher stub — records every `dispatch_to_agent`
    /// call, returns a fixed `output` string. `dispatch_subtask` is
    /// unused by the idle path so it just returns an empty string.
    struct FakeDispatcher {
        output: String,
        calls: StdArc<PlMutex<Vec<(String, String)>>>,
    }

    #[async_trait]
    impl AgentDispatcher for FakeDispatcher {
        async fn dispatch_to_agent(
            &self,
            room_id: &str,
            agent_id: &str,
            _session_id: &str,
            _system_prompt: &str,
            _message: &str,
            _room_context: &str,
        ) -> Result<String, String> {
            self.calls.lock().push((room_id.to_string(), agent_id.to_string()));
            Ok(self.output.clone())
        }
        async fn dispatch_subtask(
            &self,
            _room_id: &str,
            _agent_id: &str,
            _session_id: &str,
            _task_description: &str,
            _system_prompt: &str,
        ) -> Result<String, String> {
            Ok(String::new())
        }
        async fn dispatch_subtask_stream(
            &self,
            room_id: &str,
            agent_id: &str,
            _session_id: &str,
            _system_prompt: &str,
            _message: &str,
            _role: &str,
            _on_chunk: Box<dyn Fn(super::super::dispatcher::StreamDelta) + Send + Sync>,
        ) -> Result<String, String> {
            // Idle tests don't exercise streaming — just record the call
            // and return the canned output (no chunks fired).
            self.calls.lock().push((room_id.to_string(), agent_id.to_string()));
            Ok(self.output.clone())
        }
    }

    /// Always-failing dispatcher. Records calls just like the fake
    /// one above, but returns Err so the publish hook must NOT fire.
    struct FailingDispatcher {
        calls: StdArc<PlMutex<Vec<String>>>,
    }

    #[async_trait]
    impl AgentDispatcher for FailingDispatcher {
        async fn dispatch_to_agent(
            &self,
            _room_id: &str,
            agent_id: &str,
            _session_id: &str,
            _system_prompt: &str,
            _message: &str,
            _room_context: &str,
        ) -> Result<String, String> {
            self.calls.lock().push(agent_id.to_string());
            Err("upstream model down".to_string())
        }
        async fn dispatch_subtask(
            &self,
            _room_id: &str,
            _agent_id: &str,
            _session_id: &str,
            _task_description: &str,
            _system_prompt: &str,
        ) -> Result<String, String> {
            Err(String::new())
        }
        async fn dispatch_subtask_stream(
            &self,
            _room_id: &str,
            agent_id: &str,
            _session_id: &str,
            _system_prompt: &str,
            _message: &str,
            _role: &str,
            _on_chunk: Box<dyn Fn(super::super::dispatcher::StreamDelta) + Send + Sync>,
        ) -> Result<String, String> {
            // Mirror `dispatch_to_agent`'s failure path for the streaming
            // entry point — idle tests assert the upstream-error hook
            // contract, so the streaming variant has to honour it too.
            self.calls.lock().push(agent_id.to_string());
            Err("upstream model down".to_string())
        }
    }

    fn make_member(id: &str) -> AgentRoomProfile {
        AgentRoomProfile {
            agent_id: id.to_string(),
            session_id: format!("{}-session", id),
            role: AgentRole::Member,
            display_name: None,
            room_id: "test-room".to_string(),
            model_override: None,
        }
    }

    /// P1.3 happy path: dispatcher returns Ok → hook fires with
    /// (room_id, agent_id, response). We pass a 2-agent room so
    /// the deterministic `select_idle_agents` picks exactly one
    /// (the "second agent" branch is gated by `now_ms % 3 == 0`,
    /// which we don't rely on).
    #[tokio::test]
    async fn publish_hook_fires_on_successful_dispatch() {
        let calls = StdArc::new(PlMutex::new(Vec::new()));
        let disp: Arc<dyn AgentDispatcher> = Arc::new(FakeDispatcher {
            output: "hello from agent".to_string(),
            calls: calls.clone(),
        });

        let hook_log: StdArc<PlMutex<Vec<(String, String, String)>>> =
            StdArc::new(PlMutex::new(Vec::new()));
        let hook_log_inner = hook_log.clone();
        let hook: IdlePublishHook = Arc::new(move |room, agent, content| {
            hook_log_inner
                .lock()
                .push((room.to_string(), agent.to_string(), content.to_string()));
        });

        let agents = vec![make_member("alice"), make_member("bob")];
        tick_idle_round(&disp, Some(&hook), "room-42", &agents).await;

        // Dispatcher should have been called at least once. We don't
        // assert on the exact count because `select_idle_agents`
        // includes a probabilistic second agent — what we care about
        // is that the hook fired at least once and matched one of
        // the calls.
        let recorded_calls = calls.lock().len();
        assert!(
            recorded_calls >= 1,
            "dispatcher should be invoked at least once, got {}",
            recorded_calls
        );

        let recorded_hooks = hook_log.lock();
        assert!(
            !recorded_hooks.is_empty(),
            "publish hook must fire after a successful dispatch"
        );
        // Each hook call must reference room-42 + a real agent + a real response.
        for (room, _agent, content) in recorded_hooks.iter() {
            assert_eq!(room, "room-42");
            assert_eq!(content, "hello from agent");
        }
    }

    /// P1.3 — when no publish hook is installed, the dispatcher
    /// still runs (idle chat keeps working) but the response is
    /// simply dropped. Preserves the pre-P1.3 behaviour for
    /// callers that haven't installed a hook yet.
    #[tokio::test]
    async fn no_hook_means_response_is_dropped() {
        let calls = StdArc::new(PlMutex::new(Vec::new()));
        let disp: Arc<dyn AgentDispatcher> = Arc::new(FakeDispatcher {
            output: "dropped".to_string(),
            calls: calls.clone(),
        });

        let agents = vec![make_member("alice")];
        tick_idle_round(&disp, None, "room-x", &agents).await;

        assert!(
            !calls.lock().is_empty(),
            "dispatcher should still be invoked even with no hook installed"
        );
    }

    /// P1.3 — dispatcher returns Err → publish hook MUST NOT fire.
    /// We must never award rank points for a message we don't have.
    #[tokio::test]
    async fn publish_hook_does_not_fire_on_dispatch_error() {
        let calls = StdArc::new(PlMutex::new(Vec::new()));
        let disp: Arc<dyn AgentDispatcher> = Arc::new(FailingDispatcher {
            calls: calls.clone(),
        });

        let hook_log: StdArc<PlMutex<Vec<(String, String, String)>>> =
            StdArc::new(PlMutex::new(Vec::new()));
        let hook_log_inner = hook_log.clone();
        let hook: IdlePublishHook = Arc::new(move |room, agent, content| {
            hook_log_inner
                .lock()
                .push((room.to_string(), agent.to_string(), content.to_string()));
        });

        let agents = vec![make_member("alice")];
        tick_idle_round(&disp, Some(&hook), "room-fail", &agents).await;

        assert_eq!(
            calls.lock().len(),
            1,
            "failing dispatcher should be called exactly once"
        );
        assert!(
            hook_log.lock().is_empty(),
            "publish hook MUST NOT fire when dispatcher returns Err"
        );
    }

    /// P1.3 — `set_publish_hook` replaces any previous hook.
    /// Regression guard against accidental re-introduction of the
    /// "two hooks fire per dispatch" bug if someone re-adds an
    /// `iter().for_each` loop.
    #[tokio::test]
    async fn set_publish_hook_replaces_previous() {
        let disp: Arc<dyn AgentDispatcher> = Arc::new(FakeDispatcher {
            output: "x".to_string(),
            calls: StdArc::new(PlMutex::new(Vec::new())),
        });
        let mgr = IdleChatManager::new(disp);

        let count_a = StdArc::new(PlMutex::new(0u32));
        let count_b = StdArc::new(PlMutex::new(0u32));
        let a_inner = count_a.clone();
        let b_inner = count_b.clone();

        mgr.set_publish_hook(Arc::new(move |_, _, _| {
            *a_inner.lock() += 1;
        }));
        mgr.set_publish_hook(Arc::new(move |_, _, _| {
            *b_inner.lock() += 1;
        }));

        // The currently-installed hook is `b`; running a round
        // should increment only `b`.
        let agents = vec![make_member("alice")];
        tick_idle_round(&mgr.dispatcher, mgr.publish_hook.lock().as_ref(), "r", &agents).await;

        assert_eq!(*count_a.lock(), 0, "replaced hook must not fire");
        assert_eq!(*count_b.lock(), 1, "newest hook must fire exactly once");
    }
}
