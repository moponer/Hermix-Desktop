#![allow(clippy::ptr_arg)]
//! Hermes Skills Crate
//!
//! Implements the skills system (Requirement 12) for Hermes Agent.
//! Provides skill management, local file storage, hub client, security
//! validation, and versioning.

mod expert;
mod guard;
mod skill;
mod store;
mod validator;
mod version;

pub use expert::{ExpertEntry, ExpertRegistry};
pub use guard::SkillGuard;
pub use skill::{SkillError, SkillManager};
pub use store::{FileSkillStore, SkillEvent, SkillEventCallback, SkillStore, SkillWatcher};
pub use validator::{
    description_has_trigger, has_section, validate_skill_md, Severity, ValidationFinding,
    ValidationResult,
};
pub use version::{compare_versions, compute_version, track_change, SkillChange, SkillVersion};
