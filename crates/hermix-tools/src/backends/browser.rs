//! Browser backends: local CDP plus provider-selected cloud sessions.
//!
//! Python resolves browser execution in two layers:
//! 1. choose a provider (`browser-use`, `browserbase`, `camofox`, `local`)
//! 2. obtain a concrete CDP endpoint for that provider
//!
//! The Rust port keeps the existing `BrowserBackend` tool surface, but routes
//! calls through an auto-selecting backend that lazily provisions cloud
//! sessions when needed.
//!
//! The concrete implementations live in sibling submodules (`cdp`, `cloud`,
//! `a11y`, `registry`, `session_wrapper`); this file only declares them and
//! re-exports the public surface so existing `backends::browser::*` paths
//! keep resolving unchanged.

pub mod a11y;
pub mod cdp;
pub mod cloud;
pub mod registry;
pub mod session_wrapper;

pub use cdp::{CamoFoxBrowserBackend, CdpBrowserBackend};
pub use cloud::{AutoBrowserBackend, CloudBrowserBackendAdapter};
pub use registry::BrowserRegistry;
pub use session_wrapper::SessionedBrowserBackend;
