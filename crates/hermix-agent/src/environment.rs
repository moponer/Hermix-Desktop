//! Runtime environment snapshot for system-prompt injection.
//!
//! Tells the agent *where* it is running and *what is available*:
//! OS, architecture, hermix home, skills / memories / context /
//! plugin directories, and counts of discovered artifacts. The
//! block is generated once per session (cheap: a few `std::env`
//! reads and `read_dir` calls) and merged into the system prompt
//! by [`SystemPromptBuilder::with_environment`].
//!
//! # Why this exists
//!
//! Before this module, `build_system_prompt` injected the active
//! *workspace* path but nothing about the *host* — the agent had
//! no idea where its hermix home lived, how many skills it had,
//! or which OS it was on. That made it impossible to give
//! grounded answers to questions like "where do I put a new
//! skill?" or "how many built-in skills are installed?".
//!
//! # Read-only by design
//!
//! This module never writes to the hermix home. Directory creation
//! belongs to `hermix-config` (e.g. portable_data_dir
//! initialisation). What we guarantee is that the prompt never
//! lies about the layout: a directory that does not exist is
//! reported as `(missing)` rather than a phantom path the model
//! will then try to read.

use std::path::{Path, PathBuf};

use hermix_config::paths::{
    approval_allowlist_path, auth_json_path, cron_dir, env_path, hermix_home, memory_path,
    sessions_dir, skills_dir, user_path,
};

// ---------------------------------------------------------------------------
// RuntimeEnvironment
// ---------------------------------------------------------------------------

/// Snapshot of the host runtime for system-prompt injection.
///
/// Constructed once at session start (or whenever the system prompt
/// is rebuilt) by [`RuntimeEnvironment::scan`] and consumed by
/// [`build_environment_block`]. All fields are best-effort:
/// failures during discovery (permission errors, missing dirs)
/// are reported as `None` / `(unavailable)` rather than panicking.
#[derive(Debug, Clone)]
pub struct RuntimeEnvironment {
    /// OS family — e.g. `"windows"`, `"linux"`, `"macos"`.
    pub os: String,
    /// CPU architecture — e.g. `"x86_64"`, `"aarch64"`.
    pub arch: String,
    /// Hostname / computer name. `None` if neither `COMPUTERNAME`
    /// (Windows) nor `HOSTNAME` (POSIX) is set.
    pub hostname: Option<String>,
    /// Current OS user. `None` if neither `USERNAME` (Windows) nor
    /// `USER` (POSIX) is set.
    pub username: Option<String>,

    /// Resolved hermix home directory (walks `HERMIX_PORTABLE_DATA_DIR`
    /// → `HERMIX_HOME` → `~/.hermix`).
    pub hermix_home: PathBuf,
    /// Directory where user-facing skills live
    /// (`$hermix_home/skills`).
    pub skills_dir: PathBuf,
    /// Path of `MEMORY.md` (`$hermix_home/MEMORY.md`).
    pub memory_path: PathBuf,
    /// Path of `USER.md` (`$hermix_home/USER.md`).
    pub user_path: PathBuf,
    /// Path of `.env` (LLM credentials, `$hermix_home/.env`).
    pub env_path: PathBuf,
    /// Path of `auth.json` (OAuth credentials,
    /// `$hermix_home/auth.json`).
    pub auth_json_path: PathBuf,
    /// Path of `approval_allowlist.json` (user-allowed command
    /// patterns).
    pub approval_allowlist_path: PathBuf,
    /// Path of the `sessions/` directory.
    pub sessions_dir: PathBuf,
    /// Path of the `cron/` directory.
    pub cron_dir: PathBuf,

    /// Number of subdirectories under `skills_dir/` (i.e. skills
    /// the orchestrator can discover). `None` if the directory
    /// does not exist or could not be read.
    pub skills_count: Option<usize>,

    /// Process working directory at scan time.
    pub current_dir: PathBuf,
    /// Path of the running hermix binary. `None` if `current_exe`
    /// failed (extremely rare).
    pub hermix_binary: Option<PathBuf>,

    /// Whether the current directory is inside a Git repository.
    pub is_git_repo: bool,
    /// Current Git branch name (if in a repo).
    pub git_branch: Option<String>,
    /// Detected project type based on file indicators.
    pub project_type: Option<String>,
}

impl RuntimeEnvironment {
    /// Build a snapshot by reading env vars and probing the hermix
    /// home layout. **Never panics** — every probe is best-effort.
    pub fn scan() -> Self {
        let home = hermix_home();
        let skills = skills_dir();
        let skills_count = count_subdirs(&skills);
        let hermix_binary = std::env::current_exe().ok();
        let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

        // Detect Git status
        let (is_git_repo, git_branch) = detect_git_status(&current_dir);

        // Detect project type
        let project_type = detect_project_type(&current_dir);

        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            hostname: read_hostname(),
            username: read_username(),
            hermix_home: home,
            skills_dir: skills,
            memory_path: memory_path(),
            user_path: user_path(),
            env_path: env_path(),
            auth_json_path: auth_json_path(),
            approval_allowlist_path: approval_allowlist_path(),
            sessions_dir: sessions_dir(),
            cron_dir: cron_dir(),
            skills_count,
            current_dir,
            hermix_binary,
            is_git_repo,
            git_branch,
            project_type,
        }
    }
}

fn read_hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn read_username() -> Option<String> {
    std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .ok()
        .filter(|s| !s.trim().is_empty())
}

fn count_subdirs(dir: &Path) -> Option<usize> {
    if !dir.is_dir() {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    Some(entries.flatten().filter(|e| e.path().is_dir()).count())
}

/// Detect if the current directory is inside a Git repository
/// and return the current branch name.
fn detect_git_status(dir: &Path) -> (bool, Option<String>) {
    // Walk up to find .git directory
    let mut current = dir.to_path_buf();
    loop {
        if current.join(".git").exists() {
            // Found .git directory, try to read branch
            let head_file = current.join(".git/HEAD");
            if let Ok(head) = std::fs::read_to_string(&head_file) {
                let branch = head
                    .trim()
                    .strip_prefix("ref: refs/heads/")
                    .map(|s| s.to_string());
                return (true, branch);
            }
            return (true, None);
        }
        if !current.pop() {
            break;
        }
    }
    (false, None)
}

/// Detect project type based on file indicators in the current directory.
fn detect_project_type(dir: &Path) -> Option<String> {
    let indicators = [
        ("Cargo.toml", "rust"),
        ("package.json", "node"),
        ("pyproject.toml", "python"),
        ("go.mod", "go"),
        ("pom.xml", "java"),
        ("build.gradle", "java"),
        ("*.sln", "dotnet"),
        ("Gemfile", "ruby"),
        ("composer.json", "php"),
    ];

    for (indicator, project_type) in &indicators {
        if indicator.starts_with('*') {
            // Glob pattern - check if any file matches
            let pattern = indicator.trim_start_matches('*');
            if dir.read_dir().ok().map_or(false, |entries| {
                entries.flatten().any(|e| e.path().to_string_lossy().ends_with(pattern))
            }) {
                return Some(project_type.to_string());
            }
        } else if dir.join(indicator).exists() {
            return Some(project_type.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// build_environment_block
// ---------------------------------------------------------------------------

/// Format the environment as a markdown-flavoured block ready to be
/// merged into a system prompt.
///
/// Layout (one bullet per line):
///
/// ```text
/// ## Runtime Environment
/// - OS / Arch: windows / x86_64
/// - Hostname: ALEX-PC
/// - User: Alex
/// - Hermix home: `C:\Users\Alex\.hermix`
/// - Skills dir: `...\skills` (12 skills)            ← or "(missing)"
/// - Memory file: `...\MEMORY.md`                    ← or "(missing)"
/// - User profile: `...\USER.md`
/// - LLM env file: `...\config\.env`
/// - Auth store: `...\auth.json`
/// - Approval allowlist: `...\approval_allowlist.json`
/// - Sessions dir: `...\sessions`
/// - Cron dir: `...\cron`
/// - Hermix binary: `C:\...\hermix.exe`              ← optional
/// - Working dir: `D:\...`
/// ```
pub fn build_environment_block(env: &RuntimeEnvironment) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(15);
    lines.push("## Runtime Environment".to_string());
    lines.push(format!("- OS / Arch: {} / {}", env.os, env.arch));

    if let Some(host) = &env.hostname {
        lines.push(format!("- Hostname: {host}"));
    } else {
        lines.push("- Hostname: (unavailable)".to_string());
    }

    if let Some(user) = &env.username {
        lines.push(format!("- User: {user}"));
    } else {
        lines.push("- User: (unavailable)".to_string());
    }

    lines.push(format!("- Hermix home: `{}`", env.hermix_home.display()));

    lines.push(format!(
        "- Skills dir: `{}`{}",
        env.skills_dir.display(),
        match env.skills_count {
            Some(n) => format!(" ({n} skill{})", if n == 1 { "" } else { "s" }),
            None => " (missing — no skills installed yet)".to_string(),
        }
    ));

    lines.push(format!(
        "- Memory file: `{}`{}",
        env.memory_path.display(),
        if env.memory_path.is_file() {
            ""
        } else {
            " (missing — MEMORY.md will be created on first write)"
        }
    ));

    lines.push(format!("- User profile: `{}`", env.user_path.display()));
    lines.push(format!("- LLM env file: `{}`", env.env_path.display()));
    lines.push(format!("- Auth store: `{}`", env.auth_json_path.display()));
    lines.push(format!(
        "- Approval allowlist: `{}`",
        env.approval_allowlist_path.display()
    ));
    lines.push(format!("- Sessions dir: `{}`", env.sessions_dir.display()));
    lines.push(format!("- Cron dir: `{}`", env.cron_dir.display()));

    if let Some(bin) = &env.hermix_binary {
        lines.push(format!("- Hermix binary: `{}`", bin.display()));
    }

    lines.push(format!("- Working dir: `{}`", env.current_dir.display()));

    // Git status
    if env.is_git_repo {
        let branch_info = env.git_branch.as_deref().unwrap_or("unknown");
        lines.push(format!("- Git: repo detected, branch: {}", branch_info));
    } else {
        lines.push("- Git: not a repository".to_string());
    }

    // Project type
    if let Some(pt) = &env.project_type {
        lines.push(format!("- Project type: {}", pt));
    }

    lines.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `scan()` must never panic and must populate the OS / arch
    /// fields from `std::env::consts`. Other fields may be `None`
    /// in test environments.
    #[test]
    fn scan_does_not_panic() {
        let env = RuntimeEnvironment::scan();
        assert!(!env.os.is_empty());
        assert!(!env.arch.is_empty());
        assert!(!env.hermix_home.as_os_str().is_empty());
        assert!(!env.current_dir.as_os_str().is_empty());
    }

    /// A fully-populated environment renders every key field.
    #[test]
    fn build_block_includes_key_paths() {
        let env = RuntimeEnvironment {
            os: "linux".into(),
            arch: "x86_64".into(),
            hostname: Some("test-host".into()),
            username: Some("tester".into()),
            hermix_home: PathBuf::from("/tmp/hermix"),
            skills_dir: PathBuf::from("/tmp/hermix/skills"),
            memory_path: PathBuf::from("/tmp/hermix/MEMORY.md"),
            user_path: PathBuf::from("/tmp/hermix/USER.md"),
            env_path: PathBuf::from("/tmp/hermix/config/.env"),
            auth_json_path: PathBuf::from("/tmp/hermix/auth.json"),
            approval_allowlist_path: PathBuf::from("/tmp/hermix/approval_allowlist.json"),
            sessions_dir: PathBuf::from("/tmp/hermix/sessions"),
            cron_dir: PathBuf::from("/tmp/hermix/cron"),
            skills_count: Some(3),
            current_dir: PathBuf::from("/tmp"),
            hermix_binary: Some(PathBuf::from("/usr/bin/hermix")),
            is_git_repo: true,
            git_branch: Some("main".into()),
            project_type: Some("rust".into()),
        };

        let block = build_environment_block(&env);
        assert!(block.contains("## Runtime Environment"));
        assert!(block.contains("linux / x86_64"));
        assert!(block.contains("test-host"));
        assert!(block.contains("tester"));
        assert!(block.contains("/tmp/hermix"));
        assert!(block.contains("3 skills"));
        assert!(block.contains("/usr/bin/hermix"));
    }

    /// Missing / unavailable fields render as `(unavailable)` or
    /// `(missing — ...)` rather than empty lines.
    #[test]
    fn build_block_handles_missing() {
        let env = RuntimeEnvironment {
            os: "windows".into(),
            arch: "x86_64".into(),
            hostname: None,
            username: None,
            hermix_home: PathBuf::from("C:/hermix"),
            skills_dir: PathBuf::from("C:/hermix/skills"),
            memory_path: PathBuf::from("C:/hermix/MEMORY.md"),
            user_path: PathBuf::from("C:/hermix/USER.md"),
            env_path: PathBuf::from("C:/hermix/.env"),
            auth_json_path: PathBuf::from("C:/hermix/auth.json"),
            approval_allowlist_path: PathBuf::from("C:/hermix/approval_allowlist.json"),
            sessions_dir: PathBuf::from("C:/hermix/sessions"),
            cron_dir: PathBuf::from("C:/hermix/cron"),
            skills_count: None,
            current_dir: PathBuf::from("C:/"),
            hermix_binary: None,
            is_git_repo: false,
            git_branch: None,
            project_type: None,
        };

        let block = build_environment_block(&env);
        assert!(block.contains("(unavailable)"), "block was: {block}");
        assert!(
            block.contains("(missing — no skills installed yet)"),
            "block was: {block}"
        );
        assert!(
            !block.contains("Hermix binary"),
            "binary line should be omitted when None, got: {block}"
        );
    }

    /// Pluralisation: 1 skill reads "1 skill", N reads "N skills".
    #[test]
    fn build_block_pluralises_skill_count() {
        let make = |n: Option<usize>| RuntimeEnvironment {
            os: "linux".into(),
            arch: "x86_64".into(),
            hostname: None,
            username: None,
            hermix_home: PathBuf::from("/x"),
            skills_dir: PathBuf::from("/x/skills"),
            memory_path: PathBuf::from("/x/MEMORY.md"),
            user_path: PathBuf::from("/x/USER.md"),
            env_path: PathBuf::from("/x/.env"),
            auth_json_path: PathBuf::from("/x/auth.json"),
            approval_allowlist_path: PathBuf::from("/x/approval_allowlist.json"),
            sessions_dir: PathBuf::from("/x/sessions"),
            cron_dir: PathBuf::from("/x/cron"),
            skills_count: n,
            current_dir: PathBuf::from("/"),
            hermix_binary: None,
            is_git_repo: false,
            git_branch: None,
            project_type: None,
        };

        assert!(build_environment_block(&make(Some(1))).contains("1 skill)"));
        assert!(!build_environment_block(&make(Some(1))).contains("1 skills)"));
        assert!(build_environment_block(&make(Some(7))).contains("7 skills)"));
    }
}
