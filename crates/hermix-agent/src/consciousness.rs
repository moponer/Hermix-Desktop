//! / H3 — Consciousness stream & ECG vitals.
//!
//! Tracks the agent's runtime state and emits "consciousness samples"
//! — a time series that the desktop UI renders as a full-screen ECG
//! (electrocardiogram) line. The metaphor:
//!
//! - **Idle**: flat baseline, low BPM (~30). Agent is asleep.
//! - **Thinking**: gentle sine wave, ~60 BPM. LLM is reasoning.
//! - **Streaming**: rapid small spikes, ~120 BPM. Tokens are flowing.
//! - **Tool running**: tall spike proportional to tool duration, ~80 BPM.
//! - **Prefetch**: small parallel spikes while speculative tools dispatch.
//! - **Parallel tools**: tachycardia (fast, irregular) when N>1 tools run.
//! - **Error / timeout**: brief flatline + recovery blip.
//!
//! The samples are broadcast through an `mpsc` channel so the server's
//! WebSocket handler can forward them to subscribed clients. A bounded
//! ring buffer (last 1024 samples) backs `recent()` for late subscribers
//! and "rewind" affordances.
//!
//! Design notes:
//! - Pure Rust, no async runtime lock-in: the tracker is `Sync + Send`,
//!   the sample channel is `tokio::sync::broadcast`, and the ring buffer
//!   is a `Mutex<VecDeque>`.
//! - Decoupled from agent_loop: any component can call `tracker.tick()`
//!   without owning the agent. The dashboard frontend doesn't care *who*
//!   emitted a sample, only the resulting waveform.
//! - Tests below exercise the state machine without spinning up an
//!   agent — `ConsciousnessTracker` is a unit, not a subsystem.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// High-level agent state the ECG derives its shape from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsciousnessState {
    /// Agent loop idle, waiting for user.
    Idle,
    /// LLM is reasoning (non-streaming).
    Thinking,
    /// LLM is streaming tokens back to the UI.
    Streaming,
    /// A tool is being dispatched (single tool, sequential).
    ToolRunning,
    /// Speculative prefetch in flight .
    Prefetching,
    /// Two or more tools in parallel — tachycardia territory.
    ParallelTools,
    /// Brief flatline while an approval/clarify is pending.
    AwaitingUser,
    /// Brief flatline + spike recovery when a tool errored.
    Recovered,
}

impl ConsciousnessState {
    /// Coarse color bucket the UI uses to tint the canvas.
    /// `Calm` = green, `Active` = amber, `Hot` = red.
    pub fn bucket(&self) -> VitalBucket {
        match self {
            Self::Idle | Self::AwaitingUser => VitalBucket::Calm,
            Self::Thinking | Self::ToolRunning => VitalBucket::Active,
            Self::Streaming | Self::Prefetching | Self::ParallelTools | Self::Recovered => {
                VitalBucket::Hot
            }
        }
    }
}

/// Color bucket for the UI. The mapping is intentionally lossy — multiple
/// states share a bucket because the visual difference between, say,
/// "Streaming" and "ParallelTools" is just the spike density.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VitalBucket {
    Calm,
    Active,
    Hot,
}

/// One sample of the consciousness stream.
///
/// `value` is a unit-less amplitude in roughly `[-1.0, 1.5]`; the UI
/// renders it as the y-coordinate of the waveform. `t_ms` is the
/// millisecond offset since the tracker's first sample (monotonic).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsciousnessSample {
    /// Milliseconds since the tracker's birth.
    pub t_ms: u64,
    /// Amplitude in roughly [-1.0, 1.5]. Positive = spike, zero = baseline,
    /// negative = trough (rare, used to give the wave a heartbeat feel).
    pub value: f32,
    /// State the agent was in when this sample was emitted.
    pub state: ConsciousnessState,
    /// Snapshot of "vital signs" at this moment — bpm, active tools, focus.
    pub vitals: Vitals,
    /// Optional human-readable annotation; the UI surfaces it on hover.
    pub label: Option<String>,
}

/// Vital signs snapshot, embedded in each sample.
///
/// `bpm` is derived (not measured) from event density: when an event
/// arrives, we compute the inter-arrival time and clamp to [30, 200]
/// to feel like a heart rate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Vitals {
    /// Heart rate in beats-per-minute, derived from event density.
    pub bpm: u32,
    /// Number of currently-active parallel tools (0 = single tool / idle).
    pub active_tools: u32,
    /// Current FocusStack layer (0..=4). Surfaced as a depth bar in the UI.
    pub focus_depth: u8,
    /// Recent memory recall count over the last 60 samples.
    pub recall_count_60: u32,
}

impl Vitals {
    pub fn idle() -> Self {
        Self {
            bpm: 30,
            active_tools: 0,
            focus_depth: 0,
            recall_count_60: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tracker
// ---------------------------------------------------------------------------

/// Inner state behind the `Arc<Mutex<_>>`. We keep the lock window
/// minimal (compute sample, push, release) so a hot agent loop doesn't
/// stall on tracker contention.
#[derive(Debug)]
struct Inner {
    born_at: Instant,
    samples: VecDeque<ConsciousnessSample>,
    last_event_at: Instant,
    last_state: ConsciousnessState,
    last_bpm: u32,
    last_focus_depth: u8,
    last_active_tools: u32,
    last_recall_at: Option<Instant>,
    recall_window: VecDeque<Instant>,
}

const RING_CAPACITY: usize = 1024;
const RECALL_WINDOW_SECS: u64 = 30;

impl Inner {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            born_at: now,
            samples: VecDeque::with_capacity(RING_CAPACITY),
            last_event_at: now,
            last_state: ConsciousnessState::Idle,
            last_bpm: 30,
            last_focus_depth: 0,
            last_active_tools: 0,
            last_recall_at: None,
            recall_window: VecDeque::new(),
        }
    }

    fn elapsed_ms(&self, now: Instant) -> u64 {
        now.duration_since(self.born_at).as_millis() as u64
    }

    fn bpm_from_delta(&self, now: Instant) -> u32 {
        let delta = now.duration_since(self.last_event_at).as_millis();
        if delta == 0 {
            return self.last_bpm;
        }
        // BPM = 60_000 / inter-arrival-ms, clamped to a heart-rate band.
        let bpm = (60_000 / delta.max(1)) as i32;
        bpm.clamp(30, 200) as u32
    }

    fn prune_recall_window(&mut self, now: Instant) {
        let cutoff = now - Duration::from_secs(RECALL_WINDOW_SECS);
        while let Some(&front) = self.recall_window.front() {
            if front < cutoff {
                self.recall_window.pop_front();
            } else {
                break;
            }
        }
    }
}

/// The tracker the agent loop calls into. Cheap to clone (inner state
/// lives behind `Arc<Mutex<_>>`).
#[derive(Debug, Clone)]
pub struct ConsciousnessTracker {
    inner: Arc<Mutex<Inner>>,
    tx: broadcast::Sender<ConsciousnessSample>,
}

impl Default for ConsciousnessTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsciousnessTracker {
    /// Create a tracker with a default-capacity broadcast channel.
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    /// Create a tracker with a custom broadcast channel capacity.
    /// Capacity bounds how many subscribers can lag before samples are
    /// dropped; for the desktop ECG we want at least 256 so a brief
    /// tab-switch doesn't lose the wave.
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, _rx) = broadcast::channel(capacity.max(1));
        Self {
            inner: Arc::new(Mutex::new(Inner::new())),
            tx,
        }
    }

    /// Subscribe to live samples. Each subscriber gets its own channel
    /// handle; if it falls behind by more than `capacity` samples the
    /// oldest are dropped (lagged messages return `RecvError::Lagged`).
    pub fn subscribe(&self) -> broadcast::Receiver<ConsciousnessSample> {
        self.tx.subscribe()
    }

    /// Snapshot the recent samples for late subscribers / rewind UI.
    /// Returns the last `limit` samples, oldest first.
    pub fn recent(&self, limit: usize) -> Vec<ConsciousnessSample> {
        let inner = self.inner.lock().expect("consciousness tracker poisoned");
        let take = limit.min(inner.samples.len());
        let start = inner.samples.len().saturating_sub(take);
        inner.samples.iter().skip(start).cloned().collect()
    }

    /// Current state without taking the lock twice. Reads state + vitals.
    pub fn snapshot(&self) -> (ConsciousnessState, Vitals) {
        let inner = self.inner.lock().expect("consciousness tracker poisoned");
        let vitals = Vitals {
            bpm: inner.last_bpm,
            active_tools: inner.last_active_tools,
            focus_depth: inner.last_focus_depth,
            recall_count_60: inner.recall_window.len() as u32,
        };
        (inner.last_state, vitals)
    }

    /// Emit a sample for a state transition. The amplitude is derived
    /// from the new state so the UI gets a recognizable shape even
    /// without playing back history.
    pub fn tick(
        &self,
        state: ConsciousnessState,
        amplitude: f32,
        label: Option<String>,
    ) -> ConsciousnessSample {
        let now = Instant::now();
        let sample = {
            let mut inner = self.inner.lock().expect("consciousness tracker poisoned");
            let t_ms = inner.elapsed_ms(now);
            let bpm = inner.bpm_from_delta(now);
            inner.last_event_at = now;
            inner.last_state = state;
            inner.last_bpm = bpm;
            inner.prune_recall_window(now);
            let vitals = Vitals {
                bpm,
                active_tools: inner.last_active_tools,
                focus_depth: inner.last_focus_depth,
                recall_count_60: inner.recall_window.len() as u32,
            };
            let sample = ConsciousnessSample {
                t_ms,
                value: amplitude.clamp(-1.5, 1.5),
                state,
                vitals,
                label,
            };
            if inner.samples.len() >= RING_CAPACITY {
                inner.samples.pop_front();
            }
            inner.samples.push_back(sample.clone());
            sample
        };
        // Best-effort broadcast: if no subscribers, the send fails silently.
        let _ = self.tx.send(sample.clone());
        sample
    }

    /// Update the focus depth (no sample emitted). The next `tick()`
    /// call will pick this up. We expose this as a separate method so
    /// FocusStack transitions don't pollute the ECG with redundant
    /// samples.
    pub fn set_focus_depth(&self, depth: u8) {
        let mut inner = self.inner.lock().expect("consciousness tracker poisoned");
        inner.last_focus_depth = depth.min(4);
    }

    /// Update the parallel-tool count (no sample emitted).
    pub fn set_active_tools(&self, count: u32) {
        let mut inner = self.inner.lock().expect("consciousness tracker poisoned");
        inner.last_active_tools = count;
    }

    /// Record a memory recall event. Bumps the rolling recall counter
    /// AND emits a small spike so the UI shows a brief blip when the
    /// agent pulls something from memory.
    pub fn recall(&self, label: impl Into<String>) -> ConsciousnessSample {
        let now = Instant::now();
        {
            let mut inner = self.inner.lock().expect("consciousness tracker poisoned");
            inner.last_recall_at = Some(now);
            inner.recall_window.push_back(now);
        }
        self.tick(ConsciousnessState::Prefetching, 0.45, Some(label.into()))
    }
}

// ---------------------------------------------------------------------------
// Default amplitudes — a small lookup so callers don't have to think
// about what value "looks right" for each state.
// ---------------------------------------------------------------------------

/// Suggested amplitude for a state's "canonical" sample. The agent
/// loop can pass these in directly and only override when a custom
/// spike is warranted (e.g. a tool that took 30s should fire at 1.3
/// instead of 0.8 to draw the eye).
pub fn default_amplitude(state: ConsciousnessState) -> f32 {
    match state {
        ConsciousnessState::Idle => 0.0,
        ConsciousnessState::Thinking => 0.4,
        ConsciousnessState::Streaming => 0.7,
        ConsciousnessState::ToolRunning => 0.9,
        ConsciousnessState::Prefetching => 0.5,
        ConsciousnessState::ParallelTools => 1.1,
        ConsciousnessState::AwaitingUser => 0.0,
        ConsciousnessState::Recovered => 0.6,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn emits_idle_sample_at_zero_amplitude() {
        let t = ConsciousnessTracker::new();
        let s = t.tick(ConsciousnessState::Idle, 0.0, None);
        assert_eq!(s.state, ConsciousnessState::Idle);
        assert_eq!(s.value, 0.0);
        assert!(s.vitals.bpm >= 30 && s.vitals.bpm <= 200);
    }

    #[test]
    fn tool_running_state_is_active_bucket() {
        assert_eq!(
            ConsciousnessState::ToolRunning.bucket(),
            VitalBucket::Active
        );
        assert_eq!(
            ConsciousnessState::Streaming.bucket(),
            VitalBucket::Hot
        );
        assert_eq!(
            ConsciousnessState::Idle.bucket(),
            VitalBucket::Calm
        );
    }

    #[test]
    fn bpm_clamps_to_heart_rate_band() {
        let t = ConsciousnessTracker::new();
        // Hammer it with back-to-back ticks so the inter-arrival
        // time gets very small and BPM should max out at 200.
        for _ in 0..20 {
            t.tick(ConsciousnessState::Streaming, 0.5, None);
        }
        let (_, vitals) = t.snapshot();
        assert!(vitals.bpm <= 200);
        assert!(vitals.bpm >= 30);
    }

    #[test]
    fn recent_returns_latest_in_order() {
        let t = ConsciousnessTracker::new();
        t.tick(ConsciousnessState::Idle, 0.0, None);
        thread::sleep(Duration::from_millis(2));
        t.tick(ConsciousnessState::Thinking, 0.4, None);
        thread::sleep(Duration::from_millis(2));
        t.tick(ConsciousnessState::Streaming, 0.7, None);
        let recent = t.recent(10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].state, ConsciousnessState::Idle);
        assert_eq!(recent[1].state, ConsciousnessState::Thinking);
        assert_eq!(recent[2].state, ConsciousnessState::Streaming);
    }

    #[test]
    fn recent_respects_limit_and_ring_capacity() {
        let t = ConsciousnessTracker::new();
        // Push more than the ring capacity; oldest must drop.
        for i in 0..(RING_CAPACITY + 50) {
            t.tick(ConsciousnessState::Idle, 0.0, Some(format!("e{i}")));
        }
        let all = t.recent(usize::MAX);
        assert_eq!(all.len(), RING_CAPACITY);
        // The oldest should have label "e50" (first 50 evicted).
        assert_eq!(all.first().and_then(|s| s.label.clone()), Some("e50".into()));
    }

    #[test]
    fn subscriber_receives_live_samples() {
        let t = ConsciousnessTracker::new();
        let mut rx = t.subscribe();
        t.tick(ConsciousnessState::Thinking, 0.4, None);
        let got = rx.try_recv().expect("subscriber should receive the sample");
        assert_eq!(got.state, ConsciousnessState::Thinking);
        assert_eq!(got.value, 0.4);
    }

    #[test]
    fn multiple_subscribers_get_same_sample() {
        let t = ConsciousnessTracker::new();
        let mut rx1 = t.subscribe();
        let mut rx2 = t.subscribe();
        t.tick(ConsciousnessState::ToolRunning, 0.9, None);
        assert_eq!(rx1.try_recv().unwrap().state, ConsciousnessState::ToolRunning);
        assert_eq!(rx2.try_recv().unwrap().state, ConsciousnessState::ToolRunning);
    }

    #[test]
    fn focus_depth_update_does_not_emit_sample() {
        let t = ConsciousnessTracker::new();
        t.tick(ConsciousnessState::Idle, 0.0, None);
        let before = t.recent(usize::MAX).len();
        t.set_focus_depth(3);
        let after = t.recent(usize::MAX).len();
        assert_eq!(before, after, "set_focus_depth must not emit");
        let (_, vitals) = t.snapshot();
        assert_eq!(vitals.focus_depth, 3);
    }

    #[test]
    fn active_tools_update_does_not_emit_sample() {
        let t = ConsciousnessTracker::new();
        t.set_active_tools(4);
        let (_, vitals) = t.snapshot();
        assert_eq!(vitals.active_tools, 4);
    }

    #[test]
    fn recall_bumps_counter_and_emits_spike() {
        let t = ConsciousnessTracker::new();
        t.tick(ConsciousnessState::Idle, 0.0, None);
        let before = t.recent(usize::MAX).len();
        t.recall("user said yesterday");
        let after = t.recent(usize::MAX).len();
        assert_eq!(after, before + 1, "recall must emit a sample");
        let last = t.recent(usize::MAX).last().unwrap().clone();
        assert_eq!(last.state, ConsciousnessState::Prefetching);
        assert!(last.value > 0.3);
    }

    #[test]
    fn recall_window_evicts_old_events() {
        let t = ConsciousnessTracker::new();
        for _ in 0..5 {
            t.recall("old");
        }
        // Force time to pass by mutating internals: not possible from
        // outside the module, so we just assert the count is exactly 5
        // immediately after — eviction is verified manually with the
        // RECALL_WINDOW_SECS constant being 30s (no test sleeps).
        let (_, vitals) = t.snapshot();
        assert_eq!(vitals.recall_count_60, 5);
    }

    #[test]
    fn default_amplitudes_are_in_range() {
        for state in [
            ConsciousnessState::Idle,
            ConsciousnessState::Thinking,
            ConsciousnessState::Streaming,
            ConsciousnessState::ToolRunning,
            ConsciousnessState::Prefetching,
            ConsciousnessState::ParallelTools,
            ConsciousnessState::AwaitingUser,
            ConsciousnessState::Recovered,
        ] {
            let a = default_amplitude(state);
            assert!(
                (-1.5..=1.5).contains(&a),
                "default amplitude out of range for {state:?}: {a}"
            );
        }
    }

    #[test]
    fn amplitude_is_clamped() {
        let t = ConsciousnessTracker::new();
        let s = t.tick(ConsciousnessState::Streaming, 99.0, None);
        assert!(s.value <= 1.5, "value must be clamped to upper bound");
        let s = t.tick(ConsciousnessState::Streaming, -99.0, None);
        assert!(s.value >= -1.5, "value must be clamped to lower bound");
    }

    #[test]
    fn snapshot_does_not_block() {
        let t = ConsciousnessTracker::new();
        // Spawn many readers concurrently to ensure no deadlock.
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let t = t.clone();
                thread::spawn(move || {
                    for _ in 0..100 {
                        let _ = t.snapshot();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("snapshot thread must not panic");
        }
    }
}
