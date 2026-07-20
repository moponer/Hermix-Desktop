//! Real backend implementations for tool handlers.
//!
//! Each submodule provides a concrete implementation of a backend trait
//! defined in the `tools` module, connecting to real APIs, file systems,
//! databases, and external services.
//!
//! Cleanup history (2026-06-30, Plan v2):
//! - voice_mode / audio_gen are NOT dead — they're shared infrastructure
//!   used by the `hermix-voice` crate (`transcribe_audio`, `VoicePipeline`).
//!   They were mistakenly marked ❌ in TOOL_CLEANUP.md and we initially
//!   deleted them, which broke the voice crate. Restored via `git restore`.

pub mod browser;
pub mod cua_backend;
pub mod clarify;
pub mod code_execution;
pub mod cronjob;
pub mod delegation;
pub mod file;
pub mod gpt_image;
pub mod homeassistant;
pub mod image_gen;
pub mod memory;
pub mod messaging;
pub mod session_search;
pub mod threat_scan;
pub mod todo;
pub mod tts;
pub mod video_gen;
pub mod web;
