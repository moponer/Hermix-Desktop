//! # hermes-environments
//!
//! Terminal backend systems and environment management for Hermes Agent.
//!
//! This crate provides local command execution and (optionally) SSH-based
//! remote execution and Docker container isolation. The [`BackendManager`]
//! orchestrates which backend is active and allows runtime switching. The
//! cloud-only backends (Daytona, Modal, Singularity) remain as enum
//! variants so user config files don't break, but their implementations
//! were never wired up — those are out of scope for the desktop
//! "user's own machine" philosophy.
//!
//! ## Training / benchmarks (planned)
//!
//! See [`training`] for traits and notes aligned with `PARITY_PLAN.md` Week 3.

pub mod file_sync;
pub mod jobs;
mod output;
pub mod local;
pub mod local_isolation;
pub mod pty_filter;
pub mod pty_session;
pub mod sandbox;
pub mod manager;

#[cfg(feature = "ssh")]
pub mod ssh;

#[cfg(feature = "docker")]
pub mod docker;

pub mod training;

// Re-export core trait and local types
pub use file_sync::FileSync;
pub use hermix_core::TerminalBackend;
pub use local::LocalBackend;
pub use pty_session::PtySessionRegistry;
pub use local_isolation::{HostedAgentPool, LocalIsolationBackend};
pub use manager::BackendManager;
pub use training::benchmarks::swe_bench::SweBenchEnv;
pub use training::benchmarks::terminal_bench::TerminalBenchEnv;
pub use training::benchmarks::yc_bench::YcBenchEnv;
pub use training::parsers::{parse_tool_calls, ParsedToolCall, ParserKind};
pub use training::{EnvConfig, EnvTask, HermesBaseEnv, HermesEpisode, Trajectory, TrajectoryStep};
