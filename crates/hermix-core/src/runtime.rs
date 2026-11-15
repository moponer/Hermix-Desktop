//! Cross-platform command-spawning helpers.
//!
//! Windows `CreateProcess` requires the exact filename including
//! extension, but the npm / pnpm / yarn toolchain ships only batch
//! shims (`npx.cmd`, `pnpm.cmd`, `yarn.cmd`, …) without a
//! bare-name fallback. A plain `Command::new("npx")` therefore
//! fails with `program not found` even when npx is on PATH.
//!
//! [`resolve_binary`] appends `.cmd` to a small whitelist of known
//! npm-family shims when running on Windows. Everything else
//! (`git`, `python`, `node`, `ssh`, …) is left alone — those tools
//! either ship as proper `.exe` (Git for Windows, Python.org
//! installer, Node.js MSI) or are user-installed binaries whose
//! path the user is responsible for keeping correct in config.
//!
//! The helper is intentionally conservative: appending `.cmd` to
//! arbitrary commands would silently swallow typos (e.g. `nde`
//! would never resolve to `node.exe`). The whitelist keeps the
//! blast radius to the npm family where the `.cmd` extension is
//! load-bearing on every Windows install.
//!
//! No external dependencies: just a string round-trip. Callers
//! keep their existing `Command::new(resolved_name)` call shape,
//! so adopting the helper is a one-line change per call site.

/// Resolve a binary name to its Windows shim extension (`.cmd`)
/// when applicable. Returns the input unchanged on non-Windows
/// targets and for commands outside the npm-family whitelist.
///
/// Callers should pass the result directly to `Command::new` /
/// `tokio::process::Command::new`. The returned `String` is owned
/// so each call site keeps its existing `.args(...).env(...)`
/// chaining without lifetime gymnastics.
///
/// # Examples
///
/// ```ignore
/// use hermix_core::runtime::resolve_binary;
/// let mut cmd = std::process::Command::new(resolve_binary("npx"));
/// On Windows: spawns `npx.cmd`.
/// On Linux/macOS: spawns `npx`.
/// ```
pub fn resolve_binary(name: &str) -> String {
    #[cfg(target_os = "windows")]
    {
        // Known npm-family shims. `git`, `python`, `node`, `ssh`,
        // `docker` etc. are deliberately NOT in this list — they
        // install as proper `.exe` (or the user points at the
        // binary explicitly via config) so a `.cmd` lookup would be
        // a no-op that still costs a stat() per spawn.
        const NPM_SHIMS: &[&str] = &[
            "npx", "npm", "pnpm", "yarn", "bun", "deno", "tsc", "tsx", "ts-node",
        ];
        if std::path::Path::new(name).extension().is_none()
            && NPM_SHIMS.contains(&name)
        {
            return format!("{}.cmd", name);
        }
        name.to_string()
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Unix: PATH resolution handles bare names natively. We
        // deliberately do NOT call `which::which` here — that would
        // add a startup-time PATH scan and a dependency on the
        // `which` crate in the foundation crate, for zero gain on
        // platforms where bare names already resolve correctly.
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_binary_preserves_names_with_extension() {
        // Inputs that already have an extension must pass through
        // untouched regardless of OS — that includes things like
        // `python.exe`, `bash.exe`, `npx.cmd`.
        assert_eq!(resolve_binary("python.exe"), "python.exe");
        assert_eq!(resolve_binary("bash.exe"), "bash.exe");
        assert_eq!(resolve_binary("npx.cmd"), "npx.cmd");
    }

    #[test]
    fn resolve_binary_preserves_non_whitelisted_names() {
        // `git`, `python`, `ssh`, `docker` etc. are not in the
        // npm-shim whitelist, so the helper leaves them alone on
        // every OS.
        assert_eq!(resolve_binary("git"), "git");
        assert_eq!(resolve_binary("python"), "python");
        assert_eq!(resolve_binary("ssh"), "ssh");
        assert_eq!(resolve_binary("docker"), "docker");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn resolve_binary_adds_cmd_for_npm_shims_on_windows() {
        // The whole point of the helper: bare `npx` becomes
        // `npx.cmd` so `Command::new` can find the npm shim.
        assert_eq!(resolve_binary("npx"), "npx.cmd");
        assert_eq!(resolve_binary("npm"), "npm.cmd");
        assert_eq!(resolve_binary("pnpm"), "pnpm.cmd");
        assert_eq!(resolve_binary("yarn"), "yarn.cmd");
        assert_eq!(resolve_binary("bun"), "bun.cmd");
        assert_eq!(resolve_binary("tsc"), "tsc.cmd");
        assert_eq!(resolve_binary("tsx"), "tsx.cmd");
        assert_eq!(resolve_binary("ts-node"), "ts-node.cmd");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn resolve_binary_passes_through_on_unix() {
        // On Unix, even npm-family names stay bare — `Command::new`
        // resolves them through PATH normally.
        assert_eq!(resolve_binary("npx"), "npx");
        assert_eq!(resolve_binary("npm"), "npm");
        assert_eq!(resolve_binary("pnpm"), "pnpm");
    }

    #[test]
    fn resolve_binary_never_panics_on_empty_or_unicode() {
        // Defensive: empty strings and Unicode must not trigger
        // any panic in the extension-lookup path.
        assert_eq!(resolve_binary(""), "");
        assert_eq!(resolve_binary("中文"), "中文");
        assert_eq!(resolve_binary("🦀"), "🦀");
    }
}