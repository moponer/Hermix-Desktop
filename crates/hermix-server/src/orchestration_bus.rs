//! Cross-crate orchestration event bus .
//!
//! Implements 铁律 DD: the four-step transparent event pipeline —
//! transport → tools → register → HttpServerState + WS listener.
//!
//! Any crate that needs to emit orchestration events (Task lifecycle,
//! automation triggers, result delivery) calls
//! [`OrchestrationEventBus::emit`]. The server's WS handler subscribes
//! via [`OrchestrationEventBus::subscribe`] and forwards events to
//! connected clients as `StreamEvent` variants.
//!
//! Design:
//! - `broadcast` channel: cheap clones, multiple subscribers, lagging
//!   receivers miss messages (acceptable for UI events — the HTTP API
//!   is the source of truth for full state).
//! - `OrchestrationEvent` wraps the domain payload; the WS handler
//!   converts it to a `StreamEvent` for wire serialization.
//! - The bus lives in `HttpServerState` as an `Arc`, so tool backends
//!   and the agent loop can hold a clone without forming a cycle.

use std::sync::Arc;

use hermix_core::TaskResultRef;
use hermix_transport::StreamEvent;
use tokio::sync::broadcast;

/// Default channel capacity. 256 is enough for bursty UI events without
/// unbounded memory growth; lagging receivers just miss stale events.
const CHANNEL_CAPACITY: usize = 256;

/// Domain-level orchestration events emitted by the engine and its
/// tool backends. Converted to `StreamEvent` by the WS handler for
/// wire transmission.
#[derive(Debug, Clone)]
pub enum OrchestrationEvent {
    /// A new Task was created.
    TaskCreated {
        task_id: String,
        title: String,
        mode: String,
        workspace_path: String,
    },
    /// A Task's fields were updated.
    TaskUpdated {
        task_id: String,
        title: String,
        status: String,
    },
    /// A Task's status transitioned.
    TaskStatusChanged {
        task_id: String,
        old_status: String,
        new_status: String,
    },
    /// An automation fired and spawned a Task.
    AutomationTriggered {
        automation_id: String,
        automation_name: String,
        task_id: String,
    },
    /// Deliverable artifacts were produced for a Task.
    ResultDelivered {
        task_id: String,
        result_refs: Vec<TaskResultRef>,
    },
    /// Agent loop entered a new phase .
    LoopPhaseChanged {
        session_id: Option<String>,
        task_id: Option<String>,
        phase: String,
        stage: String,
        turn: u32,
    },
    /// Plan mode: agent produced a plan, waiting for user confirmation .
    PlanConfirmationRequired {
        task_id: String,
        plan_text: String,
    },
}

impl OrchestrationEvent {
    /// Convert this domain event into a `StreamEvent` for WS wire
    /// transmission. Returns `None` if the event has no wire
    /// representation (shouldn't happen with the current variants,
    /// but keeps the match exhaustive).
    pub fn to_stream_event(&self) -> StreamEvent {
        match self {
            Self::TaskCreated {
                task_id,
                title,
                mode,
                workspace_path,
            } => StreamEvent::TaskCreated {
                task_id: task_id.clone(),
                title: title.clone(),
                mode: mode.clone(),
                workspace_path: workspace_path.clone(),
            },
            Self::TaskUpdated {
                task_id,
                title,
                status,
            } => StreamEvent::TaskUpdated {
                task_id: task_id.clone(),
                title: title.clone(),
                status: status.clone(),
            },
            Self::TaskStatusChanged {
                task_id,
                old_status,
                new_status,
            } => StreamEvent::TaskStatusChanged {
                task_id: task_id.clone(),
                old_status: old_status.clone(),
                new_status: new_status.clone(),
            },
            Self::AutomationTriggered {
                automation_id,
                automation_name,
                task_id,
            } => StreamEvent::AutomationTriggered {
                automation_id: automation_id.clone(),
                automation_name: automation_name.clone(),
                task_id: task_id.clone(),
            },
            Self::ResultDelivered {
                task_id,
                result_refs,
            } => StreamEvent::ResultDelivered {
                task_id: task_id.clone(),
                result_refs: result_refs.clone(),
            },
            Self::LoopPhaseChanged {
                session_id,
                task_id,
                phase,
                stage,
                turn,
            } => StreamEvent::LoopPhaseChanged {
                session_id: session_id.clone(),
                task_id: task_id.clone(),
                phase: phase.clone(),
                stage: stage.clone(),
                turn: *turn,
            },
            Self::PlanConfirmationRequired {
                task_id,
                plan_text,
            } => StreamEvent::PlanConfirmationRequired {
                task_id: task_id.clone(),
                plan_text: plan_text.clone(),
            },
        }
    }
}

/// Broadcast bus for orchestration events.
/// Cheap to clone (`Arc` inner). Call [`Self::subscribe`] to get a
/// receiver; call [`Self::emit`] to publish. Lagging receivers miss
/// messages — the HTTP API is the source of truth for full state.
#[derive(Clone)]
pub struct OrchestrationEventBus {
    tx: broadcast::Sender<OrchestrationEvent>,
}

impl OrchestrationEventBus {
    /// Create a new bus with the default channel capacity.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self { tx }
    }

    /// Subscribe to the bus. Each subscriber gets its own receiver.
    pub fn subscribe(&self) -> broadcast::Receiver<OrchestrationEvent> {
        self.tx.subscribe()
    }

    /// Emit an event to all subscribers. Returns the number of
    /// receivers that got the message (0 if no subscribers).
    pub fn emit(&self, event: OrchestrationEvent) -> usize {
        self.tx.send(event).unwrap_or(0)
    }
}

impl Default for OrchestrationEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for OrchestrationEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OrchestrationEventBus")
            .field("capacity", &CHANNEL_CAPACITY)
            .finish()
    }
}

/// Convenience wrapper: `Arc<OrchestrationEventBus>` — the form stored
/// in `HttpServerState` and cloned to tool backends / agent loop.
pub type SharedOrchestrationBus = Arc<OrchestrationEventBus>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bus_emit_receive() {
        let bus = OrchestrationEventBus::new();
        let mut rx = bus.subscribe();
        bus.emit(OrchestrationEvent::TaskCreated {
            task_id: "t1".to_string(),
            title: "Test".to_string(),
            mode: "craft".to_string(),
            workspace_path: "/tmp".to_string(),
        });
        let event = rx.try_recv().unwrap();
        match event {
            OrchestrationEvent::TaskCreated { task_id, .. } => {
                assert_eq!(task_id, "t1");
            }
            _ => panic!("wrong event"),
        }
    }

    #[test]
    fn test_bus_no_subscribers_emit_ok() {
        let bus = OrchestrationEventBus::new();
        // emit with no subscribers should not panic
        bus.emit(OrchestrationEvent::TaskCreated {
            task_id: "t1".to_string(),
            title: "Test".to_string(),
            mode: "craft".to_string(),
            workspace_path: "/tmp".to_string(),
        });
    }

    #[test]
    fn test_to_stream_event_conversion() {
        let event = OrchestrationEvent::TaskStatusChanged {
            task_id: "t1".to_string(),
            old_status: "pending".to_string(),
            new_status: "in_progress".to_string(),
        };
        let stream_event = event.to_stream_event();
        match stream_event {
            StreamEvent::TaskStatusChanged {
                task_id,
                old_status,
                new_status,
            } => {
                assert_eq!(task_id, "t1");
                assert_eq!(old_status, "pending");
                assert_eq!(new_status, "in_progress");
            }
            _ => panic!("wrong stream event"),
        }
    }

    #[test]
    fn test_result_delivered_conversion() {
        let refs = vec![TaskResultRef {
            path: "/output/report.html".to_string(),
            artifact_type: hermix_core::TaskArtifactType::Html,
            summary: Some("Report".to_string()),
        }];
        let event = OrchestrationEvent::ResultDelivered {
            task_id: "t1".to_string(),
            result_refs: refs.clone(),
        };
        let stream_event = event.to_stream_event();
        match stream_event {
            StreamEvent::ResultDelivered {
                task_id,
                result_refs,
            } => {
                assert_eq!(task_id, "t1");
                assert_eq!(result_refs, refs);
            }
            _ => panic!("wrong stream event"),
        }
    }

    #[test]
    fn test_multiple_subscribers() {
        let bus = OrchestrationEventBus::new();
        let mut rx1 = bus.subscribe();
        let mut rx2 = bus.subscribe();
        bus.emit(OrchestrationEvent::TaskUpdated {
            task_id: "t1".to_string(),
            title: "Updated".to_string(),
            status: "in_progress".to_string(),
        });
        assert!(rx1.try_recv().is_ok());
        assert!(rx2.try_recv().is_ok());
    }

    #[test]
    fn test_shared_bus_clone() {
        let bus1 = OrchestrationEventBus::new();
        let bus2 = bus1.clone();
        let mut rx = bus2.subscribe();
        bus1.emit(OrchestrationEvent::TaskCreated {
            task_id: "t1".to_string(),
            title: "Test".to_string(),
            mode: "craft".to_string(),
            workspace_path: "/tmp".to_string(),
        });
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn test_loop_phase_changed_conversion() {
        let event = OrchestrationEvent::LoopPhaseChanged {
            session_id: Some("s1".to_string()),
            task_id: Some("t1".to_string()),
            phase: "llm_call".to_string(),
            stage: "reasoning".to_string(),
            turn: 3,
        };
        let stream_event = event.to_stream_event();
        match stream_event {
            StreamEvent::LoopPhaseChanged {
                session_id,
                task_id,
                phase,
                stage,
                turn,
            } => {
                assert_eq!(session_id.as_deref(), Some("s1"));
                assert_eq!(task_id.as_deref(), Some("t1"));
                assert_eq!(phase, "llm_call");
                assert_eq!(stage, "reasoning");
                assert_eq!(turn, 3);
            }
            _ => panic!("wrong stream event"),
        }
    }

    #[test]
    fn test_plan_confirmation_required_conversion() {
        let event = OrchestrationEvent::PlanConfirmationRequired {
            task_id: "t1".to_string(),
            plan_text: "Step 1: Read files\nStep 2: Write output".to_string(),
        };
        let stream_event = event.to_stream_event();
        match stream_event {
            StreamEvent::PlanConfirmationRequired {
                task_id,
                plan_text,
            } => {
                assert_eq!(task_id, "t1");
                assert!(plan_text.contains("Step 1"));
            }
            _ => panic!("wrong stream event"),
        }
    }

    #[test]
    fn test_loop_phase_changed_emit_receive() {
        let bus = OrchestrationEventBus::new();
        let mut rx = bus.subscribe();
        bus.emit(OrchestrationEvent::LoopPhaseChanged {
            session_id: None,
            task_id: None,
            phase: "tool_execution".to_string(),
            stage: "action".to_string(),
            turn: 5,
        });
        let event = rx.try_recv().unwrap();
        match event {
            OrchestrationEvent::LoopPhaseChanged { phase, stage, turn, .. } => {
                assert_eq!(phase, "tool_execution");
                assert_eq!(stage, "action");
                assert_eq!(turn, 5);
            }
            _ => panic!("wrong event"),
        }
    }
}
