//! Node.js + neu CLI environment detection and setup.
//!
//! Provides auto-detection of the local environment to determine whether
//! the optional `neu build` (professional packaging) mode is available.
//! If Node.js is present but neu CLI is missing, it can be auto-installed.

use std::process::Command;

/// State of the neu build environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NeuEnvironment {
    /// Node.js + neu CLI both available — `neu build` is ready.
    Ready,
    /// Node.js is not installed — only sidecar mode available.
    MissingNode,
    /// Node.js is installed but neu CLI is missing — can auto-install.
    MissingNeu,
}

/// Check if Node.js is available and return its version string.
pub fn check_node_available() -> Option<String> {
    let out = Command::new("node")
        .arg("--version")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Check if the @neutralinojs/neu CLI is installed globally.
pub fn check_neu_available() -> bool {
    let out = if cfg!(windows) {
        Command::new("cmd")
            .args(["/c", "neu", "--version"])
            .output()
    } else {
        Command::new("neu")
            .arg("--version")
            .output()
    };

    match out {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Check the full neu build environment.
pub fn check_neu_environment() -> NeuEnvironment {
    if check_node_available().is_none() {
        return NeuEnvironment::MissingNode;
    }
    if !check_neu_available() {
        return NeuEnvironment::MissingNeu;
    }
    NeuEnvironment::Ready
}

/// Install the @neutralinojs/neu CLI globally via npm.
///
/// Requires Node.js to be already installed.
pub fn install_neu_cli() -> Result<String, String> {
    if check_node_available().is_none() {
        return Err("Node.js is not installed. Please install Node.js first.".into());
    }

    let out = Command::new("npm")
        .args(["install", "-g", "@neutralinojs/neu"])
        .output()
        .map_err(|e| format!("Failed to run npm: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(format!(
            "npm install -g @neutralinojs/neu failed (exit {}):\nstderr: {}\nstdout: {}",
            out.status,
            stderr.chars().take(1000).collect::<String>(),
            stdout.chars().take(500).collect::<String>()
        ));
    }

    // Verify installation
    if !check_neu_available() {
        return Err(
            "neu CLI was installed but `neu --version` still fails. \
             You may need to restart your terminal or add npm global bin to PATH."
                .into(),
        );
    }

    Ok("@neutralinojs/neu installed successfully".into())
}

/// Get a structured status report for the frontend.
pub fn get_environment_status() -> serde_json::Value {
    let node_version = check_node_available();
    let neu_available = check_neu_available();
    let env = check_neu_environment();

    serde_json::json!({
        "node": node_version,
        "neu_available": neu_available,
        "neu_build_ready": env == NeuEnvironment::Ready,
        "status": match env {
            NeuEnvironment::Ready => "ready",
            NeuEnvironment::MissingNode => "missing_node",
            NeuEnvironment::MissingNeu => "missing_neu",
        },
        "message": match env {
            NeuEnvironment::Ready => "Node.js + neu CLI 已就绪，可使用 neu-build 专业打包",
            NeuEnvironment::MissingNode => "未检测到 Node.js。安装后可启用 neu-build 专业打包（当前使用 sidecar 模式）",
            NeuEnvironment::MissingNeu => "检测到 Node.js，但 neu CLI 未安装。点击安装后可使用 neu-build 专业打包",
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_node_returns_some_or_none() {
        // This test will pass on systems with Node.js installed,
        // and is harmless on systems without it.
        let result = check_node_available();
        // Just ensure it doesn't panic
        let _ = result.is_some();
    }

    #[test]
    fn environment_status_is_serializable() {
        let status = get_environment_status();
        assert!(status.is_object());
        assert!(status["status"].is_string());
        assert!(status["message"].is_string());
    }
}
