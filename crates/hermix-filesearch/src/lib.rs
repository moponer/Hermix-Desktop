//! hermix-filesearch — FilePulse (github.com/ning696/FilePulse) 的 NTFS MFT/USN
//! 搜索核心，vendor 进本仓库并去 Tauri 化，作为独立 exe 与宿主共用的 library。
//!
//! 分层：core/ 六模块（FilePulse 原样 + 最小过滤补丁）、state（单卷运行时）、
//! monitor（每卷 USN 监听循环）、ops（构建+落盘）、service（多卷聚合）。

pub mod core;
mod monitor;
mod ops;
pub mod service;
mod state;

pub use service::SearchService;
pub use state::VolumeState;
