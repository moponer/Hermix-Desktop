//! GroupChat module — collaborative multi-agent chat rooms.
//!
//! Provides the backend for group chat rooms where multiple agents
//! collaborate under a designated leader (总管) to complete user tasks.

pub mod persistence;
pub mod manager;
pub mod leader;
pub mod in_memory;
pub mod prompt;
pub mod rules;
pub mod rank;
pub mod dispatcher;
pub mod idle;
pub mod task_types;
pub mod budget;
pub mod context;
pub mod bus;
pub mod scheduler;
pub mod meta;
pub mod yanluo;
pub mod runtime;
pub mod agent_contract;
pub mod handoff;

pub use manager::GroupChatManager;
pub use persistence::RoomPersistence;
pub use in_memory::InMemoryRoomPersistence;
pub use leader::LeaderOrchestrator;
pub use prompt::PromptBuilder;
pub use rules::{RuleEngine, AgentBehavior, Violation, ViolationSeverity};
pub use rank::{RankManager, AgentRank, RankTier, Evaluation};
pub use dispatcher::AgentDispatcher;
pub use idle::IdleChatManager;
pub use task_types::*;
pub use budget::{TokenBudgetManager, effective_budget, base_budget_for_tier, rank_status_card};
pub use context::{ContextAssembler, IdentitySlice, TeammateCard, Turn, Reference, AssembledContext, ReferenceCache, estimate_tokens};
pub use bus::{SharedMessageBus, RoomBus, BusMessage, BusSendReport, BusSendError, AgentMeta, new_event_id};
pub use scheduler::{TaskScheduler, RoomSchedulerState, SchedulerEvent, SchedulerAction, SubtaskSpec};
pub use meta::{MetaBlock, parse_meta, strip_meta_block};
pub use yanluo::{
    AuditEngine, AuditTrail, AuditContext, ToolCallRecord, DetectorHit,
    PenaltyDecision, PenaltyLevel, CriticalAction,
    Detector, ContentQualityDetector, SummaryConsistencyDetector, LoopDetector,
    ActionValidator, CrossRoomGuard, ToneAnalyzer,
    run_detectors, decide_penalty,
};
pub use runtime::{TaskEngineRuntime, RoomRuntime, RoomEvent};
pub use agent_contract::{
    RoomAgentContract, LeaderDecision, SubtaskAssignment, HandoffDirective,
    parse_leader_decision, build_leader_decision_prompt,
};
pub use handoff::{ResolvedHandoff, HandoffKind, resolve_handoff};
