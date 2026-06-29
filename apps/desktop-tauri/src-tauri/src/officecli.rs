use std::path::PathBuf;

const OFFICECLI_BINARY_NAME: &str = "officecli-x86_64-pc-windows-msvc.exe";

/// Resolve the path to the `officecli` binary.
///
/// Resolution order:
/// 1. `OFFICECLI_BIN` env var (explicit override).
/// 2. Dev mode: `src-tauri/binaries/officecli-x86_64-pc-windows-msvc.exe`
///    relative to `CARGO_MANIFEST_DIR`.
/// 3. Release/bundled mode: `<exe_dir>/binaries/officecli-x86_64-pc-windows-msvc.exe`
///    (next to the running Tauri executable).
pub fn resolve_officecli_path() -> PathBuf {
    if let Ok(p) = std::env::var("OFFICECLI_BIN") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }

    if cfg!(debug_assertions) {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dev_path = manifest_dir.join("binaries").join(OFFICECLI_BINARY_NAME);
        if dev_path.exists() {
            return dev_path;
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default();
    exe_dir.join("binaries").join(OFFICECLI_BINARY_NAME)
}

/// Ensure the officecli binary exists and is accessible. Returns the
/// resolved path or an error message.
pub fn ensure_officecli() -> Result<PathBuf, String> {
    let path = resolve_officecli_path();
    if path.exists() {
        Ok(path)
    } else {
        Err(format!(
            "officecli binary not found at {}. Please ensure it is bundled with the application.",
            path.display()
        ))
    }
}
