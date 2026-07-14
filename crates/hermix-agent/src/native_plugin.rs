//! Runtime native plugin loader (2026-07-05).
//!
//! Loads `.dll` (Windows) / `.so` (Linux) plugins from
//! `$HERMIX_HOME/plugins/tools/` and `$HERMIX_HOME/plugins/memory/`.
//! Each sub-directory must contain a `plugin.yaml` manifest with
//! a `binary` field pointing to the shared library.
//!
//! Plugins export `hermix_plugin_init() -> *const PluginDescriptor`
//! via the C ABI. The host wraps each `ToolInstance` into a
//! `NativeToolAdapter` implementing `ToolHandler` and registers it
//! into the `ToolRegistry`.

use std::ffi::{CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use hermix_core::plugin_abi::{
    PluginDescriptor, PluginKind, ToolInstance, ToolVtable, PLUGIN_ABI_VERSION,
};
use hermix_core::{ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// LoadedNativePlugin
// ---------------------------------------------------------------------------

/// A loaded native plugin (.dll / .so) and the `PluginDescriptor`
/// pointer it exported at load time.
///
/// # Safety invariants for `Send` / `Sync`
///
/// `unsafe impl Send` / `Sync` is **not** a free pass — the host
/// process is responsible for upholding these invariants:
///
/// 1. The plugin's C ABI (`PluginVtable` + `ToolVtable` functions) must
///    be thread-safe. The host cannot enforce this at load time; a
///    plugin author who ships non-reentrant vtable functions will
///    cause data races when two threads call `execute()` concurrently.
///    This mirrors the contract of any FFI boundary: the host trusts
///    the plugin, but a future tool dispatcher can wrap calls with a
///    per-instance `Mutex` if we need to harden against bad plugins.
/// 2. The `PluginDescriptor*` returned by `hermix_plugin_init()` must
///    remain valid for the lifetime of the loaded `Library`. We
///    guarantee this by holding `lib: libloading::Library` (via
///    `_lib: libloading::Library`) which keeps the .so/.dll mapped
///    until `Drop` runs. **Do not** drop `_lib` while `descriptor` is
///    still dereferenced.
/// 3. `ToolInstance*` pointers created via `desc.create_tool(i)` are
///    owned by the plugin and must outlive any `NativeToolAdapter`
///    borrowing them. The adapter's `Drop` calls `vtable.destroy`
///    which the plugin must implement as the matching deallocator
///    (`free()` for `malloc()`, Rust's `[T]::drop` for `Box<[T]>`,
///    etc.). A mismatched allocator will cause heap corruption.
pub struct LoadedNativePlugin {
    pub name: String,
    pub kind: PluginKind,
    pub tool_count: u32,
    pub dll_path: PathBuf,
    _lib: libloading::Library,
    descriptor: *const PluginDescriptor,
}

// SAFETY: see the "Safety invariants" section on `LoadedNativePlugin`.
// 1. The C ABI contract (per plugin) makes the exported vtable
//    thread-safe. 2. `descriptor` borrows from `lib`, which is held
//    for the lifetime of `self`. 3. The plugin owns the pointer; we
//    only ever dereference through its declared vtable.
unsafe impl Send for LoadedNativePlugin {}
unsafe impl Sync for LoadedNativePlugin {}

impl Drop for LoadedNativePlugin {
    fn drop(&mut self) {
        // Library is dropped automatically via `_lib`.
        // Plugin-owned ToolInstance user_data pointers are destroyed
        // via the vtable's `destroy` by each NativeToolAdapter's Drop.
    }
}

impl LoadedNativePlugin {
    /// # Safety
    ///
    /// Caller must ensure `dll_path` points to a valid shared library
    /// that exports `hermix_plugin_init` with the correct C ABI signature.
    /// The returned pointer from the init function must remain valid for
    /// the lifetime of the `LoadedNativePlugin`.
    pub unsafe fn load(dll_path: &Path) -> Result<Self, String> {
        // SAFETY: Caller guarantees `dll_path` is a valid library path.
        // `libloading::Library::new` loads the library; the `Library`
        // handle keeps it mapped until dropped.
        let lib = unsafe {
            libloading::Library::new(dll_path)
                .map_err(|e| format!("Failed to load {}: {}", dll_path.display(), e))?
        };

        // SAFETY: `lib.get` returns a raw symbol pointer. The caller
        // guarantees the library exports `hermix_plugin_init` with the
        // correct `unsafe extern "C" fn() -> *const PluginDescriptor` signature.
        let init: unsafe extern "C" fn() -> *const PluginDescriptor = unsafe {
            *lib.get(b"hermix_plugin_init\0")
                .map_err(|e| format!("Symbol not found in {}: {}", dll_path.display(), e))?
        };

        // SAFETY: `init` is the plugin-provided entry point. Caller
        // guarantees it returns a valid, non-null `*const PluginDescriptor`
        // (null is checked immediately after).
        let descriptor = unsafe { init() };
        if descriptor.is_null() {
            return Err(format!("hermix_plugin_init returned null in {}", dll_path.display()));
        }

        // SAFETY: `descriptor` is non-null (checked above) and points to
        // a `PluginDescriptor` allocated by the plugin. It remains valid
        // for the lifetime of `lib` (which is held in `self._lib`).
        let desc_ref = unsafe { &*descriptor };

        if desc_ref.abi_version != PLUGIN_ABI_VERSION {
            return Err(format!(
                "Plugin {} ABI v{} != host v{}",
                dll_path.display(),
                desc_ref.abi_version,
                PLUGIN_ABI_VERSION
            ));
        }

        let name = if desc_ref.name.is_null() {
            "unknown".to_string()
        } else {
            // SAFETY: `desc_ref.name` is non-null (checked above) and
            // points to a NUL-terminated C string allocated by the plugin.
            // It remains valid for the lifetime of `lib`.
            unsafe { CStr::from_ptr(desc_ref.name) }.to_string_lossy().into_owned()
        };

        Ok(Self {
            name,
            kind: desc_ref.kind,
            tool_count: desc_ref.tool_count,
            dll_path: dll_path.to_path_buf(),
            _lib: lib,
            descriptor,
        })
    }

    /// Create tool handler #0 (present if `self.kind == Tools && tool_count > 0`).
    /// For multi-tool plugins, callers can pass `index` 0..tool_count.
    /// # Safety
    ///
    /// Caller must ensure `index < self.tool_count` and that the plugin
    /// remains loaded (i.e. the originating `LoadedNativePlugin` is not
    /// dropped) for the lifetime of the returned handler.
    pub unsafe fn create_tool_handler(
        &self,
        index: u32,
    ) -> Result<Arc<dyn ToolHandler>, String> {
        // SAFETY: `self.descriptor` is valid for the lifetime of `self._lib`.
        let desc_ref = unsafe { &*self.descriptor };
        assert!(index < desc_ref.tool_count);

        // SAFETY: `desc_ref.create_tool` is a valid function pointer
        // provided by the plugin. `index` is bounds-checked above.
        let raw = unsafe { (desc_ref.create_tool)(index) };
        if raw.is_null() {
            return Err("create_tool returned null".into());
        }

        // SAFETY: `raw` is non-null (checked above) and points to a
        // `ToolInstance` allocated by the plugin.
        let instance = unsafe { &*raw };
        if instance.vtable.is_null() || instance.user_data.is_null() {
            return Err("ToolInstance has null vtable or user_data".into());
        }

        Ok(Arc::new(NativeToolAdapter { raw, _plugin: () }))
    }

    /// # Safety
    ///
    /// Caller must ensure the returned pointer is properly managed
    /// by the plugin's memory provider contract.
    pub unsafe fn create_memory_provider(&self) -> Option<*mut std::ffi::c_void> {
        // SAFETY: `self.descriptor` is valid for the lifetime of `self._lib`.
        let desc_ref = unsafe { &*self.descriptor };
        // SAFETY: `f` is a valid function pointer provided by the plugin.
        desc_ref.create_memory_provider.map(|f| unsafe { f() })
    }
}

// ---------------------------------------------------------------------------
// NativeToolAdapter — bridges ToolInstance ABI → ToolHandler trait
// ---------------------------------------------------------------------------

/// Holds a `*mut ToolInstance` allocated by the plugin. Implements
/// `ToolHandler` by forwarding every call through the C vtable.
///
/// # Safety invariants for `Send` / `Sync`
///
/// Mirrors `LoadedNativePlugin`'s contract:
/// 1. `raw: *mut ToolInstance` is owned by the plugin; the host does
///    not allocate or free it. The adapter's `Drop` calls
///    `vtable.destroy(user_data)` which must be a valid deallocator.
/// 2. All vtable function pointers (`schema_json`, `execute`,
///    `free_string`) must be thread-safe — concurrent `execute()`
///    calls on the same `ToolInstance` are permitted and expected.
///    If a plugin is non-reentrant, it must serialise inside its
///    own `user_data` (e.g. with a `Mutex`).
/// 3. `raw` is borrowed from the plugin, so the adapter is
///    logically tied to the `LoadedNativePlugin` lifetime. We hold
///    a `_plugin: ()` marker (no actual reference — `LoadedNativePlugin`
///    holds `Library` not a borrow) because the plugin is expected
///    to outlive any adapter it produces; dropping the adapter
///    without dropping the plugin is a usage bug.
struct NativeToolAdapter {
    raw: *mut ToolInstance,
    /// Phantom marker: the adapter's lifetime is logically tied to
    /// the originating `LoadedNativePlugin` (which holds the
    /// `libloading::Library` keeping the vtable valid). The plugin
    /// owns `raw`; we just hold a borrow.
    _plugin: (),
}

// SAFETY: see "Safety invariants" on `NativeToolAdapter`. The plugin
// author is responsible for vtable thread-safety and matching
// allocators; the host guarantees the underlying `libloading::Library`
// stays mapped while the adapter is in use (it would be a usage bug
// to drop the originating `LoadedNativePlugin` before its adapters).
unsafe impl Send for NativeToolAdapter {}
unsafe impl Sync for NativeToolAdapter {}

impl Drop for NativeToolAdapter {
    fn drop(&mut self) {
        if self.raw.is_null() {
            return;
        }
        // SAFETY: `self.raw` is non-null and points to a valid
        // `ToolInstance` allocated by the plugin.
        let inst = unsafe { &*self.raw };
        if !inst.vtable.is_null() {
            // SAFETY: `inst.vtable` is non-null and points to a valid
            // `ToolVtable` allocated by the plugin.
            let vtable = unsafe { &*inst.vtable };
            if let Some(destroy) = vtable.destroy.into() {
                // SAFETY: `destroy` is the plugin-provided deallocator
                // for `user_data`. It is safe to call with the same
                // `user_data` that was provided at creation time.
                unsafe { destroy(inst.user_data) };
            }
        }
    }
}

fn with_vtable<F, R>(raw: *mut ToolInstance, f: F) -> Result<R, ToolError>
where
    F: FnOnce(&ToolVtable, *mut std::ffi::c_void) -> Result<R, ToolError>,
{
    if raw.is_null() {
        return Err(ToolError::ExecutionFailed("ToolInstance is null".into()));
    }
    // SAFETY: `raw` is non-null (checked above) and points to a valid
    // `ToolInstance` allocated by the plugin.
    let inst = unsafe { &*raw };
    if inst.vtable.is_null() {
        return Err(ToolError::ExecutionFailed("vtable is null".into()));
    }
    // SAFETY: `inst.vtable` is non-null and points to a valid
    // `ToolVtable` allocated by the plugin.
    let vtable = unsafe { &*inst.vtable };
    f(vtable, inst.user_data)
}

#[async_trait]
impl ToolHandler for NativeToolAdapter {
    fn schema(&self) -> ToolSchema {
        with_vtable(self.raw, |v, ud| {
            // SAFETY: `v.schema_json` is a valid function pointer
            // provided by the plugin. `ud` is the user_data pointer
            // that was associated with this tool instance at creation.
            let ptr = unsafe { (v.schema_json)(ud) };
            if ptr.is_null() {
                return Err(ToolError::ExecutionFailed("schema_json returned null".into()));
            }
            // SAFETY: `ptr` is non-null (checked above) and points to a
            // NUL-terminated C string allocated by the plugin. It is
            // valid until `v.free_string` is called.
            let raw = unsafe { CStr::from_ptr(ptr) }
                .to_str()
                .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
            serde_json::from_str::<ToolSchema>(raw)
                .map_err(|e| ToolError::ExecutionFailed(format!("schema_json parse: {e}")))
        })
        .unwrap_or_else(|_| {
            // Fallback: a minimal schema.
            use hermix_core::JsonSchema;
            ToolSchema {
                name: "native-plugin".to_string(),
                description: String::new(),
                parameters: JsonSchema::new("object"),
            }
        })
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let params_str = serde_json::to_string(&params)
            .map_err(|e| ToolError::ExecutionFailed(format!("params serialize: {e}")))?;
        let c_params = CString::new(params_str)
            .map_err(|e| ToolError::ExecutionFailed(format!("CString: {e}")))?;

        with_vtable(self.raw, move |v, ud| {
            // SAFETY: `v.execute` is a valid function pointer provided by
            // the plugin. `ud` is the user_data pointer from creation time.
            // `c_params.as_ptr()` is a valid NUL-terminated C string that
            // remains valid for the duration of this call.
            let raw = unsafe { (v.execute)(ud, c_params.as_ptr()) };
            if raw.is_null() {
                return Err(ToolError::ExecutionFailed("execute returned null".into()));
            }
            // SAFETY: `raw` is non-null (checked above) and points to a
            // NUL-terminated C string allocated by the plugin.
            let s = unsafe { CStr::from_ptr(raw) }
                .to_str()
                .map_err(|e| ToolError::ExecutionFailed(format!("execute output: {e}")))?
                .to_owned();

            // SAFETY: `v.free_string` is the plugin-provided deallocator
            // for strings returned by `execute`. `raw` was allocated by
            // the plugin's `execute` and is still valid.
            unsafe { (v.free_string)(raw) };

            // Attempt to parse as JSON; if it has a "result" field
            // unwrap it, else return the raw string.
            if let Ok(val) = serde_json::from_str::<Value>(&s) {
                if let Some(res) = val.get("result").and_then(|v| v.as_str()) {
                    return Ok(res.to_string());
                }
                if let Some(err) = val.get("error").and_then(|v| v.as_str()) {
                    return Err(ToolError::ExecutionFailed(err.to_string()));
                }
            }
            Ok(s)
        })
    }
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

pub fn discover_native_plugins(hermix_home: &Path) -> Vec<PathBuf> {
    let mut dlls = Vec::new();

    for category in &["tools", "memory"] {
        let dir = hermix_home.join("plugins").join(category);
        if !dir.is_dir() {
            continue;
        }

        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let manifest = path.join("plugin.yaml");
            if !manifest.is_file() {
                continue;
            }
            if path.join(".disabled").exists() {
                continue;
            }

            if let Ok(raw) = std::fs::read_to_string(&manifest) {
                if let Ok(parsed) = serde_yaml::from_str::<serde_yaml::Value>(&raw) {
                    if let Some(binary) = parsed.get("binary").and_then(|v| v.as_str()) {
                        let dll = path.join(binary);
                        if dll.is_file() {
                            dlls.push(dll);
                        }
                    }
                }
            }
        }
    }

    dlls
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_native_plugins_returns_empty_on_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = discover_native_plugins(tmp.path());
        assert!(result.is_empty());
    }
}
