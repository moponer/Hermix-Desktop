//! Stable C-ABI plugin interface .
//!
//! Native plugins (.dll on Windows / .so on Linux) export a single
//! entry-point:
//!
//! ```ignore
//! #[no_mangle]
//! pub extern "C" fn hermix_plugin_init() -> *const PluginDescriptor {
//!     &MY_PLUGIN
//! }
//! ```
//!
//! # Tool plugin (kind = Tools)
//!
//! The `create_tool(index)` factory returns `*mut ToolInstance`:
//!
//! ```ignore
//! ToolInstance { vtable: &TOOL_VTABLE, user_data: boxed_ptr }
//! ```
//!
//! The host casts `(vtable, user_data)` to a `Box<dyn ToolHandler>`
//! through an adapter that forwards each vtable call.
//!
//! # Directory layout
//!
//! - `$HERMIX_HOME/plugins/tools/<name>/plugin.yaml` + `<name>.dll`
//! - `$HERMIX_HOME/plugins/memory/<name>/plugin.yaml` + `<name>.dll`
//!
//! Each plugin directory must contain both a `plugin.yaml` manifest
//! and the binary; the loader scans `plugin.yaml` for the `binary`
//! field that points to the DLL name.

use std::os::raw::{c_char, c_void};

/// Magic version tag — must match exactly between host and plugin.
pub const PLUGIN_ABI_VERSION: u32 = 2;

/// What kind of extension the plugin provides.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginKind {
    Tools = 0,
    Memory = 1,
}

// ---------------------------------------------------------------------------
// Tool ABI: vtable-based dispatch
// ---------------------------------------------------------------------------

/// Opaque serialized JSON string passed from plugin → host.
/// The caller owns the returned pointer and must free it with
/// the companion `free_string` function.
pub type JsonStr = *const c_char;

/// Vtable for a tool handler. Every function pointer follows the
/// same calling convention across the C boundary.
#[repr(C)]
pub struct ToolVtable {
    /// Return the tool name (null-terminated, static lifetime).
    pub name: unsafe extern "C" fn(user_data: *mut c_void) -> *const c_char,
    /// Return the tool description (null-terminated, static lifetime).
    pub description: unsafe extern "C" fn(user_data: *mut c_void) -> *const c_char,
    /// Return the JSON schema of the tool's parameters as a
    /// null-terminated string (static lifetime).
    pub schema_json: unsafe extern "C" fn(user_data: *mut c_void) -> *const c_char,
    /// Execute the tool. `params_json` is the tool-call arguments.
    /// Returns a malloc'd JSON string `{"result": "..."}` or
    /// `{"error": "..."}`. The host calls `free_string` on it.
    pub execute: unsafe extern "C" fn(
        user_data: *mut c_void,
        params_json: *const c_char,
    ) -> JsonStr,
    /// Free a string previously returned by `execute`.
    pub free_string: unsafe extern "C" fn(s: JsonStr),
    /// Drop the user_data pointer.
    pub destroy: unsafe extern "C" fn(user_data: *mut c_void),
}

/// An allocated tool instance returned by `create_tool`.
#[repr(C)]
pub struct ToolInstance {
    pub vtable: *const ToolVtable,
    pub user_data: *mut c_void,
}

/// The one-and-only entry-point struct a plugin DLL must export.
///
/// All pointers herein are `'static` — the plugin is expected to
/// embed its descriptor in a static or leak-box it.
#[repr(C)]
pub struct PluginDescriptor {
    /// Must equal [`PLUGIN_ABI_VERSION`]; the host refuses to load
    /// mismatched versions.
    pub abi_version: u32,
    /// Human-readable plugin name (UTF-8, null-terminated).
    pub name: *const c_char,
    /// Plugin category — determines which subsystem consumes it.
    pub kind: PluginKind,
    /// Null-terminated description string.
    pub description: *const c_char,
    /// Number of tool handlers this plugin provides (0 for memory-only).
    pub tool_count: u32,
    /// Factory: given index 0..tool_count, returns `*mut ToolInstance`
    /// which the host wraps as `Box<dyn ToolHandler>`.
    pub create_tool: unsafe extern "C" fn(index: u32) -> *mut ToolInstance,
    /// Factory: returns `*mut c_void` which the host casts to
    /// `Box<dyn SkillProvider>` (memory-plugin). NULL if kind != Memory.
    pub create_memory_provider:
        Option<unsafe extern "C" fn() -> *mut std::ffi::c_void>,
}
