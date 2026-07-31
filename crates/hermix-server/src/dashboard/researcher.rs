//! Researcher OS API endpoints .
//!
//! Exposes the Researcher OS continuous loop operations through
//! the Hermix Dashboard API (axum). Frontend can call these endpoints
//! to trigger discover/step/daily/status/promote operations.

use axum::extract::State;
use axum::{Json, Router, routing::{get, post}};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use hermix_agent::researcher_loop::{
    self, DailyReport, DiscoverResult, ResearcherState, StatusReport, StepOutcome,
};
use hermix_agent::researcher_queue::{QueueCounts, ResearcherQueue};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// Application state for the Researcher API.
pub struct ResearcherStateHolder {
    /// The researcher state (guarded by async mutex for thread safety).
    pub state: Mutex<ResearcherState>,
    /// The queue infrastructure.
    pub queue: Mutex<ResearcherQueue>,
}

impl ResearcherStateHolder {
    /// Create a new holder with the given data directory.
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Self {
        let data_dir = data_dir.into();
        let queue_dir = data_dir.join("queue");
        Self {
            state: Mutex::new(ResearcherState::new(data_dir)),
            queue: Mutex::new(ResearcherQueue::new(queue_dir)),
        }
    }
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct PromoteRequest {
    pub item_id: String,
    pub reviewed_by: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoverResponse {
    pub result: DiscoverResult,
}

#[derive(Debug, Serialize)]
pub struct StepResponse {
    pub outcome: StepOutcome,
}

#[derive(Debug, Serialize)]
pub struct StatusResponse {
    pub status: StatusReport,
    pub queue_counts: QueueCounts,
}

#[derive(Debug, Serialize)]
pub struct DailyResponse {
    pub report: DailyReport,
}

#[derive(Debug, Serialize)]
pub struct PromoteResponse {
    pub item_id: String,
    pub reviewed_by: String,
    pub success: bool,
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// Build the researcher API router.
pub fn researcher_router(state: Arc<ResearcherStateHolder>) -> Router {
    Router::new()
        .route("/api/researcher/discover", get(discover))
        .route("/api/researcher/step", get(step))
        .route("/api/researcher/daily", get(daily))
        .route("/api/researcher/status", get(status))
        .route("/api/researcher/promote", post(promote))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// Trigger a discover operation.
async fn discover(
    State(holder): State<Arc<ResearcherStateHolder>>,
) -> Result<Json<DiscoverResponse>, Json<ErrorResponse>> {
    let mut state = holder.state.lock().await;
    match researcher_loop::loop_discover(&mut state).await {
        Ok(result) => Ok(Json(DiscoverResponse { result })),
        Err(e) => Err(Json(ErrorResponse { error: e })),
    }
}

/// Process one item from the inbox.
async fn step(
    State(holder): State<Arc<ResearcherStateHolder>>,
) -> Result<Json<StepResponse>, Json<ErrorResponse>> {
    let mut state = holder.state.lock().await;
    match researcher_loop::loop_step(&mut state).await {
        Ok(outcome) => Ok(Json(StepResponse { outcome })),
        Err(e) => Err(Json(ErrorResponse { error: e })),
    }
}

/// Run daily maintenance tasks.
async fn daily(
    State(holder): State<Arc<ResearcherStateHolder>>,
) -> Result<Json<DailyResponse>, Json<ErrorResponse>> {
    let mut state = holder.state.lock().await;
    match researcher_loop::loop_daily(&mut state).await {
        Ok(report) => Ok(Json(DailyResponse { report })),
        Err(e) => Err(Json(ErrorResponse { error: e })),
    }
}

/// Get the current researcher status.
async fn status(
    State(holder): State<Arc<ResearcherStateHolder>>,
) -> Result<Json<StatusResponse>, Json<ErrorResponse>> {
    let status_report = {
        let state = holder.state.lock().await;
        researcher_loop::loop_status(&state)
    };

    let queue_counts = {
        let queue = holder.queue.lock().await;
        queue.counts()
    };

    Ok(Json(StatusResponse {
        status: status_report,
        queue_counts,
    }))
}

/// Promote a parked item after human review.
async fn promote(
    State(holder): State<Arc<ResearcherStateHolder>>,
    Json(body): Json<PromoteRequest>,
) -> Result<Json<PromoteResponse>, Json<ErrorResponse>> {
    let queue = holder.queue.lock().await;
    match queue.promote(&body.item_id, &body.reviewed_by) {
        Ok(_) => Ok(Json(PromoteResponse {
            item_id: body.item_id,
            reviewed_by: body.reviewed_by,
            success: true,
        })),
        Err(e) => Err(Json(ErrorResponse { error: e })),
    }
}
