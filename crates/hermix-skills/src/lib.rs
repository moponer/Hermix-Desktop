#![allow(clippy::ptr_arg)]
//! Hermes Skills Crate
//!
//! Implements the skills system (Requirement 12) for Hermes Agent.
//! Provides skill management, local file storage, hub client, security
//! validation, and versioning.

mod expert;
mod guard;
mod hub;
mod skill;
mod store;
mod version;

pub use expert::{ExpertEntry, ExpertRegistry};
pub use guard::SkillGuard;
pub use hub::{SkillUpdate, SkillsHubClient};
pub use skill::{SkillError, SkillManager};
pub use store::{FileSkillStore, SkillEvent, SkillEventCallback, SkillStore, SkillWatcher};
pub use version::{compare_versions, compute_version, track_change, SkillChange, SkillVersion};
