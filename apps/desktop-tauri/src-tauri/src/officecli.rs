use std::path::PathBuf;

const OFFICECLI_BINARY_NAME: &str = "officecli-x86_64-pc-windows-msvc.exe";

/// Resolve the path to the `officecli` binary.
///
/// Resolution order:
/// 1. `OFFICECLI_BIN` env var, if set.
/// 2. Dev mode: `src-tauri/binaries/officecli-x86_64-pc-windows-msvc.exe`
/// 3. Release/bundled mode: `<exe_dir>/binaries/officecli-x86_64-pc-windows-msvc.exe`
pub fn resolve_officecli_path() -> PathBuf {
    // 1. Explicit override
    if let Ok(p) = std::env::var("OFFICECLI_BIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }

    // 2. Dev mode: relative to manifest dir
    if cfg!(debug_assertions) {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        return manifest_dir.join("binaries").join(OFFICECLI_BINARY_NAME);
    }

    // 3. Production: bundled alongside the app in a `binaries/` subdirectory
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    exe_dir.join("binaries").join(OFFICECLI_BINARY_NAME)
}

/// Ensure the officecli binary exists and is accessible. Returns the
/// resolved path on success, or an error string if not found.
pub fn ensure_officecli() -> Result<PathBuf, String> {
    let path = resolve_officecli_path();
    let canonical = path.canonicalize().map_err(|_| {
        format!(
            "officecli binary not found at {}. Please ensure it is bundled with the application.",
            path.display()
        )
    })?;
    Ok(canonical)
}
