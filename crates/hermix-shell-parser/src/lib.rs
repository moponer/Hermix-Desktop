//! Shell command classifier.
//!
//! Splits a raw command line into the program + argv prefix, and detects
//! whether the line carries shell semantics that coreutils dispatch
//! cannot handle on its own (pipes, `&&`, `||`, redirects, env-var
//! expansion `$VAR` / `${VAR}`, globs, backticks, command substitution
//! `$(...)`, semicolons, `&`). When any of these are present the caller
//! must hand the line to a real shell (`bash -lc` on Unix, or via the
//! existing `cmd.exe /c` / `powershell.exe` paths in `LocalBackend`).
//!
//! Quoting rules follow `shell-words` (POSIX-style): double quotes
//! group but expand `$VAR`; single quotes are literal; `\\` is only an
//! escape inside double quotes. That is enough for what LLMs emit in
//! practice; non-POSIX bashisms (arithmetic, arrays, brace expansion
//! without quoting) fall through to the shell-execution path.
//!
//! The parser never invokes the shell; it only inspects the bytes.
//! That keeps the fast path (`ParsedKind::Pure`) cheap and side-effect
//! free.

use thiserror::Error;

/// Classification result returned by [`parse`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedKind {
    /// Empty input — nothing to run.
    Empty,
    /// Single program + args, no shell-only operators detected.
    /// Safe to dispatch to a Rust coreutils implementation without
    /// invoking a shell.
    Pure,
    /// Contains at least one shell operator (`|`, `&&`, `||`, `;`,
    /// `>`, `<`, `` ` ``, `$(`, `$VAR`, `${VAR}`, `&` trailing,
    /// globs). Must be run by a real shell.
    ShellMeta,
}

/// Parsed program + argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedCommand {
    /// `argv[0]` (program name, basename only — paths not stripped
    /// because coreutils::dispatch keys on basename).
    pub program: String,
    /// Remaining args, after shell-words splitting.
    pub args: Vec<String>,
    /// Original raw input, preserved for fallback to shell.
    pub raw: String,
    /// Classification.
    pub kind: ParsedKind,
}

/// Errors from [`parse`]. Currently just a wrapper around
/// `shell-words::ParseError` so the caller can surface "unbalanced
/// quote" without depending on the underlying crate.
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("unbalanced quote in command: {0}")]
    UnbalancedQuote(String),
}

impl From<shell_words::ParseError> for ParseError {
    fn from(_: shell_words::ParseError) -> Self {
        ParseError::UnbalancedQuote("missing closing quote".to_string())
    }
}

/// Split a raw command line into argv tokens (POSIX-style).
///
/// Public so callers can reuse it for things like display, logging, or
/// custom dispatchers.
pub fn split(command: &str) -> Result<Vec<String>, ParseError> {
    Ok(shell_words::split(command)?)
}

/// Classify a command line.
///
/// * Empty (after trim) → `ParsedKind::Empty`.
/// * Contains any shell meta outside single-quoted regions →
///   `ParsedKind::ShellMeta`.
/// * Otherwise → `ParsedKind::Pure` with the program and args filled
///   in via `shell_words::split`.
pub fn parse(command: &str) -> Result<ParsedCommand, ParseError> {
    let raw = command.to_string();
    let trimmed = command.trim();
    if trimmed.is_empty() {
        return Ok(ParsedCommand {
            program: String::new(),
            args: Vec::new(),
            raw,
            kind: ParsedKind::Empty,
        });
    }

    if contains_shell_meta(trimmed) {
        return Ok(ParsedCommand {
            program: String::new(),
            args: Vec::new(),
            raw,
            kind: ParsedKind::ShellMeta,
        });
    }

    let tokens = shell_words::split(trimmed)?;
    if tokens.is_empty() {
        return Ok(ParsedCommand {
            program: String::new(),
            args: Vec::new(),
            raw,
            kind: ParsedKind::Empty,
        });
    }

    let program = tokens[0].clone();
    let args = tokens[1..].to_vec();
    Ok(ParsedCommand {
        program,
        args,
        raw,
        kind: ParsedKind::Pure,
    })
}

/// Scan the raw command line for shell operators that `coreutils::dispatch`
/// cannot handle. Walks the string while tracking quote state so an
/// operator inside single quotes does NOT trigger fallback (e.g.
/// `echo 'a && b'` should stay on the fast path).
fn contains_shell_meta(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;
    while i < bytes.len() {
        let b = bytes[i];
        if in_single {
            if b == b'\'' {
                in_single = false;
            }
            i += 1;
            continue;
        }
        if in_double {
            match b {
                b'"' => in_double = false,
                b'\\' if i + 1 < bytes.len() => {
                    i += 2;
                    continue;
                }
                // Globs inside double quotes still trigger the shell —
                // bash expands `"$X"` and `"*.log"` even with quotes
                // around them. Force fall-through to the meta-detect
                // arm below so callers know to hand the line to the
                // shell rather than dispatch it natively.
                b'*' | b'?' | b'[' => return true,
                _ => {}
            }
            i += 1;
            continue;
        }
        match b {
            b'\'' => in_single = true,
            b'"' => in_double = true,
            b'|' => return true,
            b'&' => return true,
            b';' => return true,
            b'>' => return true,
            b'<' => return true,
            b'`' => return true,
            b'$' => return true,
            b'*' => return true,
            b'?' => return true,
            b'[' => {
                // glob bracket like `[abc]` — treat as meta only if it
                // looks like a glob (not a literal `[` in a flag arg).
                // Heuristic: bracket immediately followed by at least one
                // unquoted `]`.
                if let Some(_close) = find_unescaped_close_bracket(bytes, i + 1) {
                    return true;
                }
            }
            _ => {}
        }
        i += 1;
    }
    false
}

fn find_unescaped_close_bracket(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start;
    while j < bytes.len() {
        match bytes[j] {
            b']' => return Some(j),
            b' ' | b'\t' | b'\n' | b'\r' if j > start => return None,
            _ => j += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_empty() {
        let p = parse("   ").unwrap();
        assert_eq!(p.kind, ParsedKind::Empty);
    }

    #[test]
    fn pure_command_parsed() {
        let p = parse("ls -la /tmp").unwrap();
        assert_eq!(p.kind, ParsedKind::Pure);
        assert_eq!(p.program, "ls");
        assert_eq!(p.args, vec!["-la", "/tmp"]);
    }

    #[test]
    fn quoted_program_does_not_split_words() {
        let p = parse(r#"echo "hello world""#).unwrap();
        assert_eq!(p.kind, ParsedKind::Pure);
        assert_eq!(p.program, "echo");
        assert_eq!(p.args, vec!["hello world"]);
    }

    #[test]
    fn pipe_is_shell_meta() {
        assert_eq!(parse("ls | grep foo").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn and_and_is_shell_meta() {
        assert_eq!(parse("a && b").unwrap().kind, ParsedKind::ShellMeta);
        assert_eq!(parse("a || b").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn redirect_is_shell_meta() {
        assert_eq!(parse("echo hi > out.txt").unwrap().kind, ParsedKind::ShellMeta);
        assert_eq!(parse("cat < in.txt").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn dollar_var_is_shell_meta() {
        assert_eq!(parse("echo $HOME").unwrap().kind, ParsedKind::ShellMeta);
        assert_eq!(parse("echo ${PATH}").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn glob_is_shell_meta() {
        assert_eq!(parse("rm *.log").unwrap().kind, ParsedKind::ShellMeta);
        assert_eq!(parse("cat file?.txt").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn semicolon_is_shell_meta() {
        assert_eq!(parse("a; b").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn backtick_is_shell_meta() {
        assert_eq!(parse("echo `date`").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn quote_around_meta_is_pure() {
        // `&&` inside single quotes is a literal string, not a meta
        // operator, so `echo 'a && b'` stays on the fast path.
        let p = parse("echo 'a && b'").unwrap();
        assert_eq!(p.kind, ParsedKind::Pure);
        assert_eq!(p.args, vec!["a && b"]);
    }

    #[test]
    fn glob_inside_double_quotes_still_meta() {
        // `*` inside double quotes is still a glob from bash's POV,
        // so mark as meta. Callers may choose to unquote then run
        // through the shell; we just flag the line.
        let p = parse(r#"echo "*.log""#).unwrap();
        assert_eq!(p.kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn command_substitution_is_meta() {
        assert_eq!(parse("echo $(date)").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn unbalanced_quote_errors() {
        assert!(parse(r#"echo "hello"#).is_err());
    }

    #[test]
    fn bracket_glob_is_meta() {
        assert_eq!(parse("ls file[12].txt").unwrap().kind, ParsedKind::ShellMeta);
    }

    #[test]
    fn bracket_in_pure_arg_not_meta() {
        // `[1]` without a closing bracket → not a glob pattern.
        assert_eq!(parse("echo [").unwrap().kind, ParsedKind::Pure);
    }

    #[test]
    fn trailing_background_amp_is_meta() {
        assert_eq!(parse("sleep 30 &").unwrap().kind, ParsedKind::ShellMeta);
    }
}
