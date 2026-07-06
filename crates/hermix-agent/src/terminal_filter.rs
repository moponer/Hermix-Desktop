//! RTK-style terminal output filter — command-aware compression.
//!
//! Detects the command being run and applies a structured compression
//! strategy instead of a generic char-count truncation. Integrates into
//! `tool_result_processor.rs` as a zero-cost pass: unknown commands
//! pass through unchanged (None return).

/// Attempt to detect the command type and apply a matching filter.
/// Returns `Some(filtered)` when the command is recognised and the
/// output was successfully compressed, or `None` to fall through to
/// the generic truncation path.
pub fn detect_and_filter(command: &str, output: &str) -> Option<String> {
    // Strip leading path/prefix noise like `cd /path && ` or `&& `
    let cmd = command.trim();
    let cmd = cmd
        .strip_prefix("&& ")
        .unwrap_or(cmd);
    // Normalise whitespace for pattern matching
    let cmd_normalised = cmd.split_whitespace().collect::<Vec<_>>().join(" ");

    // Dispatch by recognised command prefix
    if cmd_normalised.starts_with("git status") {
        return filter_git_status(output);
    }
    if cmd_normalised.starts_with("git diff") {
        return filter_git_diff(output);
    }
    if cmd_normalised.starts_with("git log") {
        return filter_git_log(output);
    }
    if cmd_normalised.starts_with("git ") {
        return filter_git_generic(output);
    }
    if cmd_normalised.starts_with("ls ") || cmd_normalised == "ls" {
        return filter_ls(output);
    }
    if cmd_normalised.starts_with("dir ") || cmd_normalised == "dir" {
        return filter_dir(output);
    }
    if cmd_normalised.starts_with("cargo test")
        || cmd_normalised.starts_with("pytest")
        || cmd_normalised.starts_with("go test")
        || cmd_normalised.starts_with("npm test")
    {
        return filter_test_output(output);
    }
    if cmd_normalised.starts_with("cargo build") || cmd_normalised.starts_with("cargo check") || cmd_normalised.starts_with("cargo clippy") {
        return filter_rust_compile(output);
    }
    if cmd_normalised.starts_with("find ") {
        return filter_find(output);
    }
    if cmd_normalised.starts_with("grep ") || cmd_normalised.starts_with("rg ") {
        return filter_grep(output);
    }
    // Test command variants with `run` (npm run test, pnpm run test, yarn run test)
    let test_run_patterns = ["npm run ", "pnpm run ", "yarn run ", "pnpm ", "yarn "];
    if test_run_patterns.iter().any(|p| cmd_normalised.starts_with(p))
        && cmd_normalised.contains(" test")
    {
        return filter_test_output(output);
    }
    None
}

// ── Git status ──────────────────────────────────────────

/// "On branch main\nChanges not staged: src/foo.rs, src/bar.rs"
fn filter_git_status(output: &str) -> Option<String> {
    let mut branch = String::new();
    let mut unstaged = Vec::new();
    let mut staged = Vec::new();
    let mut untracked = Vec::new();
    let mut section: &str = "";

    for line in output.lines() {
        let line = line.trim();
        if line.is_empty() { continue; }
        if let Some(b) = line.strip_prefix("On branch ") {
            branch = b.to_string();
            continue;
        }
        if line.contains("Changes to be committed:") {
            section = "staged"; continue;
        }
        if line.contains("Changes not staged") {
            section = "unstaged"; continue;
        }
        if line.contains("Untracked files:") {
            section = "untracked"; continue;
        }
        if line.starts_with("modified:") || line.starts_with("new file:") || line.starts_with("deleted:") {
            let file = line.split(':').nth(1).unwrap_or("").trim();
            match section {
                "staged" => staged.push(file),
                "unstaged" => unstaged.push(file),
                _ => {}
            }
            continue;
        }
        if section == "untracked" && !line.starts_with("no changes") && !line.starts_with('(') {
            let file = line.trim_start_matches('\t').trim();
            if !file.is_empty() && !file.starts_with("use \"git") {
                untracked.push(file);
            }
        }
    }

    let mut result = String::new();
    if !branch.is_empty() {
        result.push_str(&format!("On branch {}\n", branch));
    }
    if !staged.is_empty() {
        result.push_str(&format!("Staged: {} file(s)\n", staged.len()));
    }
    if !unstaged.is_empty() {
        result.push_str(&format!("Unstaged: {} file(s)\n", unstaged.len()));
        for f in unstaged.iter().take(20) {
            result.push_str(&format!("  {}\n", f));
        }
        if unstaged.len() > 20 {
            result.push_str(&format!("  ... and {} more\n", unstaged.len() - 20));
        }
    }
    if !untracked.is_empty() {
        result.push_str(&format!("Untracked: {} file(s)\n", untracked.len()));
    }
    if result.is_empty() { None } else { Some(result) }
}

// ── Git diff ────────────────────────────────────────────

/// "3 files changed, +45/-12"
fn filter_git_diff(output: &str) -> Option<String> {
    let mut files = Vec::new();
    let mut total_added = 0i64;
    let mut total_removed = 0i64;

    for line in output.lines() {
        let line = line.trim();
        if let Some(f) = line.strip_prefix("diff --git a/") {
            let parts: Vec<&str> = f.splitn(2, ' ').collect();
            files.push(parts[0].to_string());
            continue;
        }
        if line.len() > 2 && line.as_bytes()[0] == b'@' {
            // unified hunk header: @@ -1,5 +1,6 @@ ...
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            total_added += 1;
        }
        if line.starts_with('-') && !line.starts_with("---") {
            total_removed += 1;
        }
    }

    if files.is_empty() && total_added == 0 && total_removed == 0 {
        return None;
    }

    let mut result = String::new();
    result.push_str(&format!(
        "{} file(s) changed, +{}/-{}\n",
        files.len(),
        total_added,
        total_removed,
    ));
    for f in files.iter().take(15) {
        result.push_str(&format!("  {}\n", f));
    }
    if files.len() > 15 {
        result.push_str(&format!("  ... and {} more\n", files.len() - 15));
    }
    Some(result)
}

// ── Git log ─────────────────────────────────────────────

/// "abc1234 Fix login bug (2 hours ago)\ndef5678 Add tests (3 hours ago)"
fn filter_git_log(output: &str) -> Option<String> {
    let mut commits = Vec::new();
    let mut current_hash = String::new();
    let mut current_subject = String::new();

    for line in output.lines() {
        let line = line.trim();
        if line.starts_with("commit ") {
            if !current_hash.is_empty() && !current_subject.is_empty() {
                commits.push(format!("{} {}", &current_hash[..7], current_subject));
            }
            current_hash = line.strip_prefix("commit ").unwrap_or("").to_string();
            current_subject.clear();
            continue;
        }
        if line.starts_with("Date:") || line.starts_with("Author:") {
            continue;
        }
        if line.starts_with("    ") && current_subject.is_empty() {
            current_subject = line.trim().to_string();
        }
    }
    if !current_hash.is_empty() && !current_subject.is_empty() {
        commits.push(format!("{} {}", &current_hash[..7], current_subject));
    }

    if commits.is_empty() { return None; }
    Some(commits.join("\n"))
}

// ── Git generic ─────────────────────────────────────────

/// For unknown git subcommands: keep error/result lines only.
fn filter_git_generic(output: &str) -> Option<String> {
    let result: Vec<&str> = output.lines()
        .filter(|l| {
            let t = l.trim();
            !t.is_empty()
                && (t.starts_with("fatal:")
                    || t.starts_with("error:")
                    || t.starts_with("warning:")
                    || t.starts_with("remote:")
                    || t.starts_with("To ")
                    || t.starts_with("Already up")
                    || t.len() < 200)
        })
        .collect();
    if result.is_empty() { None } else { Some(result.join("\n")) }
}

// ── ls ──────────────────────────────────────────────────

/// File/directory names only, no permission/size/date lines.
fn filter_ls(output: &str) -> Option<String> {
    let result: Vec<&str> = output.lines()
        .map(|l| l.trim_end())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("total ")
                && !l.starts_with('-')   // -rw-r--r-- permission lines
                && !l.starts_with('d')   // drwxr-xr-x directory lines
                && !l.starts_with('l')   // lrwxrwxrwx symlink lines
        })
        .collect();
    if result.is_empty() { None } else { Some(result.join("\n")) }
}

// ── dir (Windows) ───────────────────────────────────────

/// Directory listing: keep file/dir names, strip headers/footers.
fn filter_dir(output: &str) -> Option<String> {
    let result: Vec<&str> = output.lines()
        .map(|l| l.trim())
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with(" Volume")
                && !l.starts_with(" Directory")
                && !l.starts_with("---")
                && l.len() < 200
        })
        .collect();
    if result.is_empty() { None } else { Some(result.join("\n")) }
}

// ── Test output (cargo test / pytest / go test / npm test) ──

/// Keep only failure lines + final summary line.
fn filter_test_output(output: &str) -> Option<String> {
    let mut results = Vec::new();
    let mut summary = String::new();
    let mut in_failures = false;

    for line in output.lines() {
        let t = line.trim();
        // Detect failure section start
        if t == "failures:" || t.starts_with("failures:") {
            in_failures = true;
            continue;
        }
        // Detect test result summary line
        if t.contains("test result:")
            || t.starts_with("test result:")
            || (t.contains("passed") && t.contains("failed"))
        {
            summary = t.to_string();
            in_failures = false;
            continue;
        }
        // Detect explicit FAILED lines (uppercase, from pytest/go test)
        if t.starts_with("FAILED ") || t.starts_with("FAIL:") || t.starts_with("--- FAIL:") {
            results.push(t);
            in_failures = true;
            continue;
        }
        if in_failures {
            if !t.is_empty() && t.len() < 300 {
                results.push(t);
            }
        }
    }

    // Fallback: if nothing found, grab the last non-empty line with summary
    if results.is_empty() && summary.is_empty() {
        for line in output.lines().rev() {
            let t = line.trim();
            if (t.contains("test result") || t.contains("passed") || t.contains("failed"))
                && !t.contains("---")
            {
                summary = t.to_string();
                break;
            }
        }
    }

    let mut out = String::new();
    for r in results.iter().take(30) {
        out.push_str(r);
        out.push('\n');
    }
    if results.len() > 30 {
        out.push_str(&format!("... and {} more failures\n", results.len() - 30));
    }
    if !summary.is_empty() && !out.contains(summary.trim()) {
        out.push_str(&summary);
        out.push('\n');
    }
    if out.is_empty() { None } else { Some(out) }
}

// ── Rust compile output (cargo build / cargo check) ─────

/// Keep only error lines (filter out "Compiling", "Checking", "Finished")
fn filter_rust_compile(output: &str) -> Option<String> {
    let result: Vec<&str> = output.lines()
        .filter(|l| {
            let t = l.trim();
            t.contains("error[")
                || t.contains("error:")
                || t.starts_with("warning[")
                || t.starts_with("warning: ")
                || t.starts_with("  --> ")
                || t.starts_with("  = ")
                || t.starts_with("  |")
                || t.starts_with("   ")
        })
        .collect();
    if result.is_empty() { None } else { Some(result.join("\n")) }
}

// ── find ────────────────────────────────────────────────

/// Compact: strip leading "./" prefix, one per line.
fn filter_find(output: &str) -> Option<String> {
    let result: Vec<&str> = output.lines()
        .map(|l| l.trim_start_matches("./"))
        .filter(|l| !l.is_empty())
        .collect();
    if result.is_empty() { None } else { Some(result.join("\n")) }
}

// ── grep / rg ───────────────────────────────────────────

/// Group by file path, collapse multiple matches per file.
fn filter_grep(output: &str) -> Option<String> {
    let mut files: Vec<(String, usize)> = Vec::new();
    let mut last_file = String::new();
    let mut count = 0usize;

    for line in output.lines() {
        if line.is_empty() { continue; }
        // rg/grep output: filepath:lineno:content or filepath:content
        // Split on first colon — if the part before is a plausible path
        // (contains separator or has a dot extension), treat it as a file.
        if let Some(idx) = line.find(':') {
            let candidate = &line[..idx];
            if !candidate.is_empty()
                && (candidate.contains('/') || candidate.contains('\\') || candidate.contains('.'))
            {
                if candidate != last_file && !last_file.is_empty() {
                    files.push((std::mem::take(&mut last_file), std::mem::replace(&mut count, 0)));
                }
                last_file = candidate.to_string();
                count += 1;
                continue;
            }
        }
    }
    if !last_file.is_empty() {
        files.push((last_file, count));
    }

    if files.is_empty() { return None; }
    let mut result = String::new();
    for (f, c) in files.iter().take(30) {
        result.push_str(&format!("{} ({} match)\n", f, c));
    }
    if files.len() > 30 {
        result.push_str(&format!("... and {} more files\n", files.len() - 30));
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_git_status_parses_branch_and_files() {
        let out = "On branch main\nChanges not staged for commit:\n  modified:   src/foo.rs\n  modified:   src/bar.rs\n";
        let result = detect_and_filter("git status", out);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.contains("On branch main"));
        assert!(r.contains("src/foo.rs"));
        assert!(!r.contains("modified:"));
    }

    #[test]
    fn test_git_diff_filters_hunks() {
        let out = "diff --git a/src/main.rs b/src/main.rs\n@@ -1,5 +1,6 @@\n fn main() {\n+    println!(\"hello\");\n ";
        let result = detect_and_filter("git diff", out);
        assert!(result.is_some());
        let r = result.unwrap();
        assert!(r.contains("+1/-0"));
    }

    #[test]
    fn test_test_output_keeps_summary() {
        let out = "test result: ok. 10 passed; 0 failed; 0 ignored\n";
        let result = detect_and_filter("cargo test", out);
        assert!(result.is_some());
        assert!(result.unwrap().contains("10 passed"));
    }

    #[test]
    fn test_unknown_command_returns_none() {
        let result = detect_and_filter("echo hello", "hello world");
        assert!(result.is_none());
    }
}
