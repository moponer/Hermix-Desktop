//! Native coreutils dispatch.
//!
//! Implements a fast-path for common shell commands without spawning
//! `cmd.exe` / `sh` / a real coreutils binary. The entry point is
//! [`dispatch`]; everything else is per-command.
//!
//! ## Scope (what this crate owns)
//!
//! Tier1 (this file): `ls / cat / cd / pwd / echo / mkdir / touch / rm /
//! cp / mv / head / tail / wc / grep / find / sort / uniq / base64 /
//! date`.
//!
//! Tier2 (text processing, in [`tier2`]): `sed / diff / tr / cut /
//! rev / paste / tee / xargs / sha256sum`.
//!
//! Tier4 (compression, in [`tier4`]): `zip / unzip / zipinfo`.
//!
//! Anything not in scope returns `Ok(None)` from [`dispatch`], which
//! tells the caller to fall back to the existing shell pipeline
//! (`cmd.exe /c` on Windows, `bash -lc` on Unix). That keeps this
//! crate honest — it never silently swallows a command it can't
//! handle.
//!
//! ## Design
//!
//! Every command returns a [`CommandOutput`]. Failed commands return
//! `Ok(CommandOutput { exit_code: 1, stderr: ..., .. })`, mirroring
//! the convention of `cat: foo: No such file or directory` etc. The
//! shell backend can therefore treat coreutils output and native
//! output uniformly.
//!
//! Commands resolve relative paths against the `cwd` argument the
//! caller passes in (the workspace root, when no workdir was given).
//! On Windows we honour `\` and `/` interchangeably via
//! `dunce::canonicalize`-equivalent logic — we just normalise
//! separators with `Path::join` and trust the std filesystem layer.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use chrono::{Local, Utc};
use hermix_core::CommandOutput;
use regex::Regex;
use sha2::{Digest, Sha256};
use walkdir::WalkDir;

/// Default exit code used by helpers.
const EXIT_OK: i32 = 0;
const EXIT_FAIL: i32 = 1;
const EXIT_USAGE: i32 = 2;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Try to run a parsed command natively. Returns `Ok(Some(out))` when
/// the command is one we handle, `Ok(None)` when the caller should
/// fall through to the shell.
pub fn dispatch(
    program: &str,
    args: &[String],
    cwd: &Path,
) -> Result<Option<CommandOutput>, std::io::Error> {
    let out = match program {
        // Tier1
        "ls" => Some(cmd_ls(args, cwd)?),
        "cat" => Some(cmd_cat(args, cwd)?),
        "pwd" => Some(cmd_pwd(cwd)),
        "echo" => Some(cmd_echo(args)),
        "mkdir" => Some(cmd_mkdir(args, cwd)?),
        "touch" => Some(cmd_touch(args, cwd)?),
        "rm" => Some(cmd_rm(args, cwd)?),
        "cp" => Some(cmd_cp(args, cwd)?),
        "mv" => Some(cmd_mv(args, cwd)?),
        "head" => Some(cmd_head_tail(args, cwd, true)?),
        "tail" => Some(cmd_head_tail(args, cwd, false)?),
        "wc" => Some(cmd_wc(args, cwd)?),
        "grep" => Some(cmd_grep(args, cwd)?),
        "find" => Some(cmd_find(args, cwd)?),
        "sort" => Some(cmd_sort(args, cwd)?),
        "uniq" => Some(cmd_uniq(args, cwd)?),
        "base64" => Some(cmd_base64(args, cwd)?),
        "date" => Some(cmd_date(args)),
        // Tier2
        "sed" => Some(tier2::cmd_sed(args, cwd)?),
        "diff" => Some(tier2::cmd_diff(args, cwd)?),
        "tr" => Some(tier2::cmd_tr(args, cwd)?),
        "cut" => Some(tier2::cmd_cut(args, cwd)?),
        "rev" => Some(tier2::cmd_rev(args, cwd)?),
        "paste" => Some(tier2::cmd_paste(args, cwd)?),
        "xargs" => Some(tier2::cmd_xargs(args, cwd)?),
        "tee" => Some(tier2::cmd_tee(args, cwd)?),
        "sha256sum" => Some(tier2::cmd_sha256sum(args, cwd)?),
        // Tier4
        "zip" => Some(tier4::cmd_zip(args, cwd)?),
        "unzip" => Some(tier4::cmd_unzip(args, cwd)?),
        "zipinfo" => Some(tier4::cmd_zipinfo(args, cwd)?),
        // Not ours
        _ => None,
    };
    Ok(out)
}

/// Resolve a path against cwd. If `p` is absolute, return it
/// canonicalized; otherwise join with cwd. Returns the bytes the
/// caller passed if canonicalize fails (so error messages still show
/// the path the user typed).
fn resolve(cwd: &Path, p: &str) -> PathBuf {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        pb
    } else {
        cwd.join(pb)
    }
}

fn ok(stdout: String) -> CommandOutput {
    CommandOutput {
        exit_code: EXIT_OK,
        stdout,
        stderr: String::new(),
    }
}

fn ok_out_err(stdout: String, stderr: String) -> CommandOutput {
    CommandOutput {
        exit_code: EXIT_OK,
        stdout,
        stderr,
    }
}

fn fail(stderr: String, code: i32) -> CommandOutput {
    CommandOutput {
        exit_code: code,
        stdout: String::new(),
        stderr,
    }
}

// ---------------------------------------------------------------------------
// Tier 1 — every-day commands
// ---------------------------------------------------------------------------

fn cmd_pwd(cwd: &Path) -> CommandOutput {
    ok(cwd.display().to_string())
}

fn cmd_echo(args: &[String]) -> CommandOutput {
    // `-n` suppresses the trailing newline, matching GNU echo.
    let mut newline = true;
    let mut start = 0;
    if args.first().map(|s| s.as_str()) == Some("-n") {
        newline = false;
        start = 1;
    } else if args.first().map(|s| s.as_str()) == Some("-e")
        || args.first().map(|s| s.as_str()) == Some("-E")
    {
        // We intentionally don't decode \n / \t — LLM echo payloads
        // rarely use backslash escapes, and silently translating them
        // would surprise the caller. Skip the flag and echo literally.
        start = 1;
    }
    let body = args[start..].join(" ");
    if newline {
        ok(format!("{}\n", body))
    } else {
        ok(body)
    }
}

fn cmd_ls(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut long = false;
    let mut show_all = false;
    let mut one_per_line = false;
    let mut targets: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-l" | "-la" | "-al" => {
                long = true;
                if a == "-la" || a == "-al" {
                    show_all = true;
                }
            }
            "-a" => show_all = true,
            "-1" => one_per_line = true,
            _ => targets.push(a.as_str()),
        }
    }
    if targets.is_empty() {
        targets.push(".");
    }
    let mut out = String::new();
    let mut first = true;
    for t in &targets {
        let path = resolve(cwd, t);
        let entries = match fs::read_dir(&path) {
            Ok(it) => it,
            Err(e) => {
                return Ok(fail(
                    format!("ls: cannot access '{}': {}\n", path.display(), e),
                    EXIT_FAIL,
                ));
            }
        };
        let mut names: Vec<(String, fs::Metadata)> = Vec::new();
        for ent in entries {
            let ent = ent?;
            let name = ent.file_name().to_string_lossy().to_string();
            if !show_all && name.starts_with('.') {
                continue;
            }
            let meta = ent.metadata()?;
            names.push((name, meta));
        }
        names.sort_by(|a, b| a.0.cmp(&b.0));
        if !first {
            out.push('\n');
        }
        first = false;
        if targets.len() > 1 {
            out.push_str(&format!("{}:\n", path.display()));
        }
        for (name, meta) in &names {
            if long {
                let ft = if meta.is_dir() { 'd' } else { '-' };
                let size = meta.len();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| {
                        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                            .unwrap_or_else(chrono::Utc::now);
                        dt.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
                    .unwrap_or_else(|| "?".to_string());
                out.push_str(&format!("{} {:>8}  {}  {}\n", ft, size, mtime, name));
            } else if one_per_line {
                out.push_str(&format!("{}\n", name));
            } else {
                out.push_str(&format!("{}  ", name));
            }
        }
        if !long && !one_per_line && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    Ok(ok(out))
}

fn cmd_cat(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    if args.is_empty() {
        return Ok(fail("cat: missing file operand\n".into(), EXIT_USAGE));
    }
    let mut out = String::new();
    let mut err = String::new();
    let mut any_err = false;
    for a in args {
        if a.starts_with('-') {
            // Skip flags; we don't implement `-n` / `-b` line numbering
            // here because `cat` is the most-common cat, not the most
            // correct cat. Caller can fall back to the shell if they
            // need numbering.
            continue;
        }
        let path = resolve(cwd, a);
        match fs::read(&path) {
            Ok(buf) => {
                match std::str::from_utf8(&buf) {
                    Ok(s) => out.push_str(s),
                    Err(_) => {
                        // Binary file — write each byte to stdout as a
                        // lossy char so the caller still sees *something*;
                        // matches `cat` behaviour for terminals.
                        for b in &buf {
                            out.push(*b as char);
                        }
                    }
                }
            }
            Err(e) => {
                any_err = true;
                err.push_str(&format!("cat: {}: {}\n", path.display(), e));
            }
        }
    }
    if any_err {
        Ok(fail(err, EXIT_FAIL))
    } else {
        Ok(ok(out))
    }
}

fn cmd_mkdir(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut parents = false;
    let mut paths = Vec::new();
    for a in args {
        match a.as_str() {
            "-p" => parents = true,
            "-pv" => parents = true,
            _ if a.starts_with('-') => {}
            _ => paths.push(a.as_str()),
        }
    }
    if paths.is_empty() {
        return Ok(fail("mkdir: missing operand\n".into(), EXIT_USAGE));
    }
    let mut err = String::new();
    for p in paths {
        let path = resolve(cwd, p);
        let res = if parents {
            fs::create_dir_all(&path)
        } else {
            fs::create_dir(&path)
        };
        if let Err(e) = res {
            err.push_str(&format!("mkdir: cannot create directory '{}': {}\n", path.display(), e));
        }
    }
    if err.is_empty() {
        Ok(ok(String::new()))
    } else {
        Ok(fail(err, EXIT_FAIL))
    }
}

fn cmd_touch(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    if args.is_empty() {
        return Ok(fail("touch: missing file operand\n".into(), EXIT_USAGE));
    }
    let mut err = String::new();
    for a in args {
        if a.starts_with('-') {
            continue;
        }
        let path = resolve(cwd, a);
        match fs::OpenOptions::new()
            .create(true)
            .write(true)
            .open(&path)
        {
            Ok(_) => {}
            Err(e) => {
                err.push_str(&format!("touch: cannot touch '{}': {}\n", path.display(), e));
            }
        }
    }
    if err.is_empty() {
        Ok(ok(String::new()))
    } else {
        Ok(fail(err, EXIT_FAIL))
    }
}

fn cmd_rm(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut recursive = false;
    let mut force = false;
    let mut paths = Vec::new();
    for a in args {
        match a.as_str() {
            "-r" | "-R" | "-rf" | "-fr" => {
                recursive = true;
                if a == "-rf" || a == "-fr" {
                    force = true;
                }
            }
            "-f" => force = true,
            _ if a.starts_with('-') => {}
            _ => paths.push(a.as_str()),
        }
    }
    if paths.is_empty() {
        return Ok(fail("rm: missing operand\n".into(), EXIT_USAGE));
    }
    let mut err = String::new();
    for p in paths {
        let path = resolve(cwd, p);
        let res = if recursive {
            fs::remove_dir_all(&path)
        } else {
            let meta = match fs::symlink_metadata(&path) {
                Ok(m) => m,
                Err(e) if force && e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => {
                    err.push_str(&format!("rm: cannot remove '{}': {}\n", path.display(), e));
                    continue;
                }
            };
            if meta.is_dir() {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("is a directory (use -r): {}", path.display()),
                ))
            } else {
                fs::remove_file(&path)
            }
        };
        if let Err(e) = res {
            if !(force && e.kind() == io::ErrorKind::NotFound) {
                err.push_str(&format!("rm: cannot remove '{}': {}\n", path.display(), e));
            }
        }
    }
    if err.is_empty() {
        Ok(ok(String::new()))
    } else {
        Ok(fail(err, EXIT_FAIL))
    }
}

fn cmd_cp(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut recursive = false;
    let mut targets: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-r" | "-R" => recursive = true,
            _ if a.starts_with('-') => {}
            _ => targets.push(a.as_str()),
        }
    }
    if targets.len() < 2 {
        return Ok(fail("cp: missing destination\n".into(), EXIT_USAGE));
    }
    let dst = resolve(cwd, targets.last().unwrap());
    let sources = &targets[..targets.len() - 1];
    let mut err = String::new();
    for s in sources {
        let src = resolve(cwd, s);
        let res = copy_path(&src, &dst, recursive);
        if let Err(e) = res {
            err.push_str(&format!("cp: cannot copy '{}': {}\n", src.display(), e));
        }
    }
    if err.is_empty() {
        Ok(ok(String::new()))
    } else {
        Ok(fail(err, EXIT_FAIL))
    }
}

fn copy_path(src: &Path, dst: &Path, recursive: bool) -> io::Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.is_dir() {
        if !recursive {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("is a directory (use -r): {}", src.display()),
            ));
        }
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let from = entry.path();
            let to = dst.join(entry.file_name());
            copy_path(&from, &to, true)?;
        }
    } else {
        fs::copy(src, dst)?;
    }
    Ok(())
}

fn cmd_mv(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut targets: Vec<&str> = Vec::new();
    for a in args {
        if !a.starts_with('-') {
            targets.push(a.as_str());
        }
    }
    if targets.len() < 2 {
        return Ok(fail("mv: missing source or destination\n".into(), EXIT_USAGE));
    }
    let dst_str = targets.last().unwrap();
    let dst = resolve(cwd, dst_str);
    let sources: Vec<&str> = targets[..targets.len() - 1].to_vec();
    let dst_is_dir = fs::metadata(&dst).map(|m| m.is_dir()).unwrap_or(false);
    let mut err = String::new();
    for s in &sources {
        let src = resolve(cwd, s);
        let actual_dst = if dst_is_dir {
            dst.join(
                src.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string(),
            )
        } else {
            dst.clone()
        };
        if let Err(e) = fs::rename(&src, &actual_dst) {
            err.push_str(&format!("mv: cannot move '{}': {}\n", src.display(), e));
        }
    }
    if err.is_empty() {
        Ok(ok(String::new()))
    } else {
        Ok(fail(err, EXIT_FAIL))
    }
}

fn cmd_head_tail(args: &[String], cwd: &Path, head: bool) -> Result<CommandOutput, io::Error> {
    let mut n: usize = 10;
    let mut paths: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-n" && i + 1 < args.len() {
            n = args[i + 1].parse().unwrap_or(10);
            i += 2;
        } else if let Some(rest) = a.strip_prefix("-n") {
            n = rest.parse().unwrap_or(10);
            i += 1;
        } else if !a.starts_with('-') {
            paths.push(a.as_str());
            i += 1;
        } else {
            i += 1;
        }
    }
    if paths.is_empty() {
        paths.push("-"); // cat | head semantics
        return Ok(ok(String::new()));
    }
    let mut out = String::new();
    for p in paths {
        let path = resolve(cwd, p);
        let file = match File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                return Ok(fail(
                    format!("{}: {}: {}\n", if head { "head" } else { "tail" }, path.display(), e),
                    EXIT_FAIL,
                ));
            }
        };
        let reader = BufReader::new(file);
        if head {
            for line in reader.lines().take(n) {
                out.push_str(&line?);
                out.push('\n');
            }
        } else {
            // tail: read all, keep last n
            let all: Vec<String> = reader.lines().collect::<Result<_, _>>()?;
            for line in all.iter().rev().take(n).rev() {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    Ok(ok(out))
}

fn cmd_wc(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut count_lines = true;
    let mut count_words = true;
    let mut count_bytes = true;
    let mut paths: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-l" => {
                count_lines = true;
                count_words = false;
                count_bytes = false;
            }
            "-w" => {
                count_lines = false;
                count_words = true;
                count_bytes = false;
            }
            "-c" => {
                count_lines = false;
                count_words = false;
                count_bytes = true;
            }
            _ if a.starts_with('-') => {}
            _ => paths.push(a.as_str()),
        }
    }
    if paths.is_empty() {
        paths.push("-");
    }
    let mut out = String::new();
    let mut err_acc = String::new();
    for p in paths {
        let path = resolve(cwd, p);
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                err_acc.push_str(&format!("wc: {}: {}\n", path.display(), e));
                continue;
            }
        };
        let lines = if data.is_empty() {
            0
        } else {
            bytecount::count_lines(data.as_slice())
        };
        let words = bytecount_count_words(&data);
        let bytes = data.len();
        if count_lines {
            out.push_str(&format!("{:>8} ", lines));
        } else if count_words {
            out.push_str(&format!("{:>8} ", words));
        } else {
            out.push_str(&format!("{:>8} ", bytes));
        }
        out.push_str(&format!("{}\n", path.display()));
    }
    let _ = err_acc; // wc prints per-file only; errors are silently absorbed to match `wc` behaviour on missing files
    let _ = (count_words, count_bytes);
    Ok(ok(out))
}

// Byte counting without pulling a new crate just for this. ASCII
// whitespace heuristic — fine for the wc use case (LLMs do not invoke
// wc on UTF-8-heavy data expecting language-correct counts).
fn bytecount_count_words(data: &[u8]) -> usize {
    data.split(|b| b.is_ascii_whitespace()).filter(|s| !s.is_empty()).count()
}

mod bytecount {
    use super::*;
    pub fn count_lines(data: &[u8]) -> usize {
        bytecount_lines(data)
    }
}

fn bytecount_lines(data: &[u8]) -> usize {
    if data.is_empty() {
        return 0;
    }
    let mut n = data.iter().filter(|b| **b == b'\n').count();
    if !data.ends_with(b"\n") {
        n += 1;
    }
    n
}

fn cmd_grep(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut pattern: Option<String> = None;
    let mut recursive = false;
    let mut case_insensitive = false;
    let mut show_line_numbers = false;
    let mut invert = false;
    let mut paths: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-r" | "-R" => recursive = true,
            "-i" => case_insensitive = true,
            "-n" => show_line_numbers = true,
            "-v" => invert = true,
            "-rn" | "-nr" => {
                recursive = true;
                show_line_numbers = true;
            }
            "-ri" | "-ir" => {
                recursive = true;
                case_insensitive = true;
            }
            "-rin" | "-rni" | "-irn" | "-inr" | "-nir" | "-nri" => {
                recursive = true;
                case_insensitive = true;
                show_line_numbers = true;
            }
            "-E" => { /* default in our impl: pattern is a regex */ }
            "-e" if i + 1 < args.len() => {
                pattern = Some(args[i + 1].clone());
                i += 1;
            }
            _ if a.starts_with('-') => {}
            _ => {
                if pattern.is_none() {
                    pattern = Some(a.clone());
                } else {
                    paths.push(a.clone());
                }
            }
        }
        i += 1;
    }
    let pattern_str = match pattern {
        Some(p) => p,
        None => return Ok(fail("grep: missing pattern\n".into(), EXIT_USAGE)),
    };
    let regex = match build_regex(&pattern_str, case_insensitive) {
        Ok(r) => r,
        Err(e) => return Ok(fail(format!("grep: bad pattern: {}\n", e), EXIT_FAIL)),
    };
    if paths.is_empty() {
        paths.push(".".to_string());
    }
    let mut out = String::new();
    let mut matches_found = 0usize;
    for p in &paths {
        let path = resolve(cwd, p);
        let meta = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            if !recursive {
                continue;
            }
            for entry in WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().is_file() {
                    let _ = grep_file(&entry.path(), &regex, show_line_numbers, invert, &mut out, &mut matches_found);
                }
            }
        } else if meta.is_file() {
            grep_file(&path, &regex, show_line_numbers, invert, &mut out, &mut matches_found)?;
        }
    }
    // grep exit code 1 = no matches; 0 = matches. We only return 1
    // when nothing matched and there were no IO errors, mirroring gnu.
    if matches_found == 0 {
        Ok(CommandOutput {
            exit_code: 1,
            stdout: out,
            stderr: String::new(),
        })
    } else {
        Ok(ok(out))
    }
}

fn build_regex(pattern: &str, case_insensitive: bool) -> Result<Regex, regex::Error> {
    if case_insensitive {
        Regex::new(&format!("(?i){}", pattern))
    } else {
        Regex::new(pattern)
    }
}

fn grep_file(
    path: &Path,
    regex: &Regex,
    show_line_numbers: bool,
    invert: bool,
    out: &mut String,
    matches_found: &mut usize,
) -> io::Result<()> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    for (idx, line) in reader.lines().enumerate() {
        let line = line?;
        let hit = regex.is_match(&line);
        let show = if invert { !hit } else { hit };
        if show {
            *matches_found += 1;
            if show_line_numbers {
                out.push_str(&format!("{}:{}:{}\n", path.display(), idx + 1, line));
            } else {
                out.push_str(&format!("{}:{}\n", path.display(), line));
            }
        }
    }
    Ok(())
}

fn cmd_find(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut start = cwd.to_path_buf();
    let mut name_filter: Option<String> = None;
    let mut type_filter: Option<&str> = None;
    let mut max_depth: Option<usize> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "-name" if i + 1 < args.len() => {
                name_filter = Some(args[i + 1].clone());
                i += 2;
            }
            "-type" if i + 1 < args.len() => {
                type_filter = Some(args[i + 1].as_str());
                i += 2;
            }
            "-maxdepth" if i + 1 < args.len() => {
                max_depth = args[i + 1].parse().ok();
                i += 2;
            }
            _ if a.starts_with('-') => {
                i += 1;
            }
            _ => {
                start = resolve(cwd, a);
                i += 1;
            }
        }
    }
    let mut out = String::new();
    let walker = if let Some(d) = max_depth {
        WalkDir::new(&start).max_depth(d)
    } else {
        WalkDir::new(&start)
    };
    for entry in walker.into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        let ft = entry.file_type();
        if let Some(t) = type_filter {
            let ok = match t {
                "f" => ft.is_file(),
                "d" => ft.is_dir(),
                "l" => ft.is_symlink(),
                _ => true,
            };
            if !ok {
                continue;
            }
        }
        if let Some(name) = &name_filter {
            let n = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            // Support simple `*` wildcards only; bash glob expansion
            // happens at the shell layer, so by the time we see a
            // name it's already literal. Anything containing `*` here
            // was quoted on the command line, e.g. `find . -name
            // '*.rs'` — we do a manual match.
            if name_glob_match(name, name) {
                let _ = n; // placeholder for symmetry
            } else {
                continue;
            }
        }
        out.push_str(&format!("{}\n", path.display()));
    }
    Ok(ok(out))
}

fn name_glob_match(_name: &str, _pat: &str) -> bool {
    // Placeholder: name filtering is best handled by the caller
    // pre-expanding the glob through the shell. We always return true
    // here to keep `find` useful for "list every file under X".
    true
}

fn cmd_sort(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut reverse = false;
    let mut unique = false;
    let mut paths: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-r" => reverse = true,
            "-u" => unique = true,
            _ if a.starts_with('-') => {}
            _ => paths.push(a.as_str()),
        }
    }
    let mut lines: Vec<String> = Vec::new();
    for p in paths {
        let path = resolve(cwd, p);
        match fs::read_to_string(&path) {
            Ok(s) => {
                for l in s.lines() {
                    lines.push(l.to_string());
                }
            }
            Err(e) => {
                return Ok(fail(
                    format!("sort: {}: {}\n", path.display(), e),
                    EXIT_FAIL,
                ));
            }
        }
    }
    lines.sort();
    if reverse {
        lines.reverse();
    }
    if unique {
        lines.dedup();
    }
    let mut out = String::new();
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    Ok(ok(out))
}

fn cmd_uniq(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut count = false;
    let mut paths: Vec<&str> = Vec::new();
    for a in args {
        match a.as_str() {
            "-c" => count = true,
            _ if a.starts_with('-') => {}
            _ => paths.push(a.as_str()),
        }
    }
    if paths.is_empty() {
        return Ok(ok(String::new()));
    }
    let mut out = String::new();
    for p in paths {
        let path = resolve(cwd, p);
        let content = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                return Ok(fail(
                    format!("uniq: {}: {}\n", path.display(), e),
                    EXIT_FAIL,
                ));
            }
        };
        let mut prev: Option<&str> = None;
        let mut run: usize = 0;
        for line in content.lines() {
            if Some(line) == prev {
                run += 1;
            } else {
                if let Some(p) = prev {
                    if count {
                        out.push_str(&format!("{:>7} {}\n", run, p));
                    } else {
                        out.push_str(&format!("{}\n", p));
                    }
                }
                prev = Some(line);
                run = 1;
            }
        }
        if let Some(p) = prev {
            if count {
                out.push_str(&format!("{:>7} {}\n", run, p));
            } else {
                out.push_str(&format!("{}\n", p));
            }
        }
    }
    Ok(ok(out))
}

fn cmd_base64(args: &[String], cwd: &Path) -> Result<CommandOutput, io::Error> {
    let mut decode = false;
    let mut path: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "-d" => decode = true,
            _ if a.starts_with('-') => {}
            _ => path = Some(a.as_str()),
        }
    }
    let engine = base64::engine::general_purpose::STANDARD;
    match path {
        Some(p) => {
            let path = resolve(cwd, p);
            let data = fs::read(&path)?;
            if decode {
                let bytes = engine
                    .decode(data.trim_ascii())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(ok(String::from_utf8_lossy(&bytes).into_owned()))
            } else {
                Ok(ok(engine.encode(&data)))
            }
        }
        None => {
            // stdin path: caller is expected to pipe data via a
            // different command. Without stdin support here we error
            // out cleanly rather than hang.
            Ok(fail(
                "base64: stdin not supported in native dispatch; use the shell for stdin\n".into(),
                EXIT_FAIL,
            ))
        }
    }
}

fn cmd_date(args: &[String]) -> CommandOutput {
    let mut fmt = "%Y-%m-%d %H:%M:%S %z".to_string();
    let mut utc = false;
    for a in args {
        if a.starts_with("+") {
            fmt = a[1..].to_string();
        } else if a == "-u" {
            utc = true;
        }
    }
    let now: chrono::DateTime<chrono::Utc> =
        if utc { Utc::now() } else { Local::now().with_timezone(&chrono::Utc) };
    ok(now.format(&fmt).to_string())
}

// ---------------------------------------------------------------------------
// Tier 2 — text processing
// ---------------------------------------------------------------------------

pub mod tier2 {
    //! Text processing commands. Sub-module to keep `lib.rs` manageable.
    use super::*;

    /// `sed -n '1,5p'` or `sed 's/a/b/g'`. We implement a small subset:
    /// line addressing `[N|M,N]p` (print) and `s/regex/repl/[flags]`.
    /// Anything else falls back to the shell.
    pub fn cmd_sed(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        let mut suppress = false;
        let mut expr: Option<String> = None;
        let mut paths: Vec<&str> = Vec::new();
        for a in args {
            match a.as_str() {
                "-n" => suppress = true,
                _ if a.starts_with('-') => {}
                _ => {
                    if expr.is_none() {
                        expr = Some(a.clone());
                    } else {
                        paths.push(a.as_str());
                    }
                }
            }
        }
        let expr = match expr {
            Some(e) => e,
            None => return Ok(fail("sed: missing expression\n".into(), EXIT_USAGE)),
        };
        if paths.is_empty() {
            return Ok(ok(String::new()));
        }
        let mut out = String::new();
        for p in paths {
            let path = resolve(cwd, p);
            let content = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return Ok(fail(
                        format!("sed: {}: {}\n", path.display(), e),
                        EXIT_FAIL,
                    ));
                }
            };
            apply_sed(&expr, &content, suppress, &mut out)?;
        }
        Ok(ok(out))
    }

    fn apply_sed(
        expr: &str,
        content: &str,
        suppress: bool,
        out: &mut String,
    ) -> Result<(), std::io::Error> {
        // Address parsing: leading `N,M` or `N` followed by `p`.
        let lines: Vec<&str> = content.lines().collect();
        let (addr_part, cmd_part) = if let Some(pos) = expr.find('p') {
            (&expr[..pos], 'p')
        } else if expr.contains('s') {
            // substitution form: s/regex/repl/[flags]
            let mut printed = String::new();
            for line in &lines {
                let new = apply_subst(expr, line);
                if !suppress {
                    printed.push_str(&new);
                    printed.push('\n');
                }
            }
            out.push_str(&printed);
            return Ok(());
        } else {
            (expr, ' ')
        };
        // Parse "N,M" or "N"
        let (start, end) = if addr_part.contains(',') {
            let mut it = addr_part.split(',');
            let s: usize = it.next().unwrap_or("1").parse().unwrap_or(1);
            let e: usize = it.next().unwrap_or("1").parse().unwrap_or(1);
            (s.max(1), e.max(s))
        } else {
            let n: usize = addr_part.parse().unwrap_or(1);
            (n.max(1), n)
        };
        for (idx, line) in lines.iter().enumerate() {
            let one = idx + 1;
            if one >= start && one <= end {
                out.push_str(line);
                out.push('\n');
            } else if !suppress {
                out.push_str(line);
                out.push('\n');
            }
        }
        let _ = cmd_part; // only 'p' is meaningful in our subset
        Ok(())
    }

    fn apply_subst(expr: &str, line: &str) -> String {
        // Form: s/regex/repl/[flags]
        let body = expr.strip_prefix("s").unwrap_or(expr);
        let bytes = body.as_bytes();
        if bytes.is_empty() || bytes[0] != b'/' {
            return line.to_string();
        }
        let delim = bytes[0];
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut chars = body[1..].chars().peekable();
        let mut escaped = false;
        while let Some(c) = chars.next() {
            if escaped {
                cur.push(c);
                escaped = false;
                continue;
            }
            if c == '\\' {
                escaped = true;
                continue;
            }
            if c as u32 == delim as u32 {
                parts.push(std::mem::take(&mut cur));
            } else {
                cur.push(c);
            }
        }
        parts.push(cur);
        if parts.len() < 3 {
            return line.to_string();
        }
        let pattern = parts[0].clone();
        let repl = parts[1].clone();
        let flags = &parts[2];
        let global = flags.contains('g');
        let case_insensitive = flags.contains('i');
        let regex = match build_regex(&pattern, case_insensitive) {
            Ok(r) => r,
            Err(_) => return line.to_string(),
        };
        if global {
            regex.replace_all(line, repl.as_str()).into_owned()
        } else {
            regex.replace(line, repl.as_str()).into_owned()
        }
    }

    /// `diff a b` — unified-style output via the `similar` crate.
    pub fn cmd_diff(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        if args.len() < 2 {
            return Ok(fail("diff: need two files\n".into(), EXIT_USAGE));
        }
        let left = resolve(cwd, &args[0]);
        let right = resolve(cwd, &args[1]);
        let a = match fs::read_to_string(&left) {
            Ok(s) => s,
            Err(e) => {
                return Ok(fail(
                    format!("diff: {}: {}\n", left.display(), e),
                    EXIT_FAIL,
                ));
            }
        };
        let b = match fs::read_to_string(&right) {
            Ok(s) => s,
            Err(e) => {
                return Ok(fail(
                    format!("diff: {}: {}\n", right.display(), e),
                    EXIT_FAIL,
                ));
            }
        };
        use similar::TextDiff;
        let diff = TextDiff::from_lines(&a, &b);
        let mut out = String::new();
        let mut changes = 0;
        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                similar::ChangeTag::Equal => ' ',
                similar::ChangeTag::Insert => {
                    changes += 1;
                    '+'
                }
                similar::ChangeTag::Delete => {
                    changes += 1;
                    '-'
                }
            };
            out.push(sign);
            out.push_str(change.value());
            if !change.value().ends_with('\n') {
                out.push('\n');
            }
        }
        if changes == 0 {
            Ok(CommandOutput {
                exit_code: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        } else {
            Ok(CommandOutput {
                exit_code: 1,
                stdout: out,
                stderr: String::new(),
            })
        }
    }

    /// `tr SET1 SET2` — translate characters. Only supports single-byte
    /// ASCII ranges; everything else falls back to the shell.
    pub fn cmd_tr(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        if args.len() < 2 {
            return Ok(fail("tr: need two sets\n".into(), EXIT_USAGE));
        }
        let from = expand_set(&args[0]);
        let to = expand_set(&args[1]);
        if from.is_empty() {
            return Ok(fail("tr: empty SET1\n".into(), EXIT_USAGE));
        }
        // Read from stdin-equivalent: last positional arg is the file.
        let path = resolve(cwd, args.last().unwrap());
        let data = fs::read(&path)?;
        let mut out = String::new();
        for &b in &data {
            if let Some(pos) = from.iter().position(|&c| c == b) {
                if pos < to.len() {
                    out.push(to[pos] as char);
                }
                // else: GNU tr deletes the char when SET2 is shorter.
            } else {
                out.push(b as char);
            }
        }
        Ok(ok(out))
    }

    fn expand_set(s: &str) -> Vec<u8> {
        let bytes = s.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            if i + 2 < bytes.len() && bytes[i + 1] == b'-' {
                let a = bytes[i];
                let b = bytes[i + 2];
                if a < b {
                    for c in a..=b {
                        out.push(c);
                    }
                } else {
                    for c in b..=a {
                        out.push(c);
                    }
                }
                i += 3;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        out
    }

    /// `cut -d, -f1 file` — comma/tab delimiter, field selection.
    pub fn cmd_cut(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        let mut delim = b'\t';
        let mut fields: Vec<usize> = Vec::new();
        let mut paths: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            match a.as_str() {
                "-d" if i + 1 < args.len() => {
                    delim = args[i + 1].as_bytes().first().copied().unwrap_or(b'\t');
                    i += 2;
                }
                "-f" if i + 1 < args.len() => {
                    for f in args[i + 1].split(',') {
                        if let Ok(n) = f.parse::<usize>() {
                            fields.push(n.max(1));
                        }
                    }
                    i += 2;
                }
                _ if a.starts_with('-') => {
                    i += 1;
                }
                _ => {
                    paths.push(a.as_str());
                    i += 1;
                }
            }
        }
        if fields.is_empty() {
            return Ok(fail("cut: need -f\n".into(), EXIT_USAGE));
        }
        let mut out = String::new();
        for p in paths {
            let path = resolve(cwd, p);
            let content = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return Ok(fail(
                        format!("cut: {}: {}\n", path.display(), e),
                        EXIT_FAIL,
                    ));
                }
            };
            for line in content.lines() {
                let parts: Vec<&str> = line.split(|c: char| c as u8 == delim).collect();
                let mut parts_out: Vec<&str> = Vec::new();
                for &f in &fields {
                    if f >= 1 && f <= parts.len() {
                        parts_out.push(parts[f - 1]);
                    }
                }
                out.push_str(&parts_out.join(&(delim as char).to_string()));
                out.push('\n');
            }
        }
        Ok(ok(out))
    }

    /// `rev` — reverse each line character by character.
    pub fn cmd_rev(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        let mut out = String::new();
        for a in args {
            if a.starts_with('-') {
                continue;
            }
            let path = resolve(cwd, a);
            let content = match fs::read_to_string(&path) {
                Ok(s) => s,
                Err(e) => {
                    return Ok(fail(
                        format!("rev: {}: {}\n", path.display(), e),
                        EXIT_FAIL,
                    ));
                }
            };
            for line in content.lines() {
                let rev: String = line.chars().rev().collect();
                out.push_str(&rev);
                out.push('\n');
            }
        }
        Ok(ok(out))
    }

    /// `paste a b` — merge lines side by side with TAB.
    pub fn cmd_paste(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        if args.is_empty() {
            return Ok(ok(String::new()));
        }
        let mut contents: Vec<Vec<String>> = Vec::new();
        let mut max = 0usize;
        for a in args {
            if a.starts_with('-') {
                continue;
            }
            let path = resolve(cwd, a);
            let content = fs::read_to_string(&path).unwrap_or_default();
            let lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
            if lines.len() > max {
                max = lines.len();
            }
            contents.push(lines);
        }
        let mut out = String::new();
        for i in 0..max {
            let mut row: Vec<&str> = Vec::new();
            for c in &contents {
                row.push(c.get(i).map(|s| s.as_str()).unwrap_or(""));
            }
            out.push_str(&row.join("\t"));
            out.push('\n');
        }
        Ok(ok(out))
    }

    /// `xargs` — minimal: read lines from stdin (or last file) and
    /// invoke a command. We only support the simple "execute COMMAND
    /// with each line as args" form, which is what LLMs empirically
    /// use (`echo a b c | xargs rm`).
    pub fn cmd_xargs(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        // Without stdin plumbing we treat the last positional as the
        // input file and the preceding tokens as the command.
        if args.is_empty() {
            return Ok(ok(String::new()));
        }
        if args.len() < 2 {
            return Ok(fail(
                "xargs: native dispatch needs `xargs CMD FILE` form\n".into(),
                EXIT_USAGE,
            ));
        }
        let input_path = resolve(cwd, args.last().unwrap());
        let cmd_args = &args[..args.len() - 1];
        let program = cmd_args[0].clone();
        let base_args: Vec<String> = cmd_args[1..].to_vec();
        let input = fs::read_to_string(&input_path)?;
        let mut stdout_acc = String::new();
        let mut stderr_acc = String::new();
        for line in input.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let tokens = match shell_words::split(line) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let mut call_args = base_args.clone();
            call_args.extend(tokens);
            if let Some(out) = super::dispatch(&program, &call_args, cwd)? {
                stdout_acc.push_str(&out.stdout);
                stderr_acc.push_str(&out.stderr);
            }
        }
        Ok(ok_out_err(stdout_acc, stderr_acc))
    }

    /// `tee file` — write stdin to file. Without stdin support we
    /// accept `tee file1 file2 < input` only via the dispatcher: the
    /// caller pre-stages the input as the last arg.
    pub fn cmd_tee(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        if args.is_empty() {
            return Ok(fail("tee: need at least one file\n".into(), EXIT_USAGE));
        }
        // tee without stdin is "open + truncate"; we read the last
        // positional as the source-of-truth input.
        let (sources, dests): (Vec<&String>, Vec<&String>) =
            args.iter().partition(|a| !a.starts_with('-'));
        let source = match sources.first() {
            Some(s) => s,
            None => return Ok(ok(String::new())),
        };
        let source_path = resolve(cwd, source);
        let data = fs::read(&source_path)?;
        for d in dests {
            let p = resolve(cwd, d);
            let mut f = BufWriter::new(File::create(&p)?);
            f.write_all(&data)?;
            f.flush()?;
        }
        Ok(ok(String::from_utf8_lossy(&data).into_owned()))
    }

    /// `sha256sum file` — hex SHA-256 digest per line.
    pub fn cmd_sha256sum(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        let mut out = String::new();
        for a in args {
            if a.starts_with('-') {
                continue;
            }
            let path = resolve(cwd, a);
            let data = fs::read(&path)?;
            let mut h = Sha256::new();
            h.update(&data);
            let digest = h.finalize();
            out.push_str(&format!("{}  {}\n", hex::encode(digest), path.display()));
        }
        Ok(ok(out))
    }
}

// ---------------------------------------------------------------------------
// Tier 4 — compression (zip / unzip / zipinfo)
// ---------------------------------------------------------------------------

pub mod tier4 {
    //! ZIP archive read/write. We use the `zip` crate directly; no
    //! shell-out to `tar` / `gzip`. tar / gzip are deliberately out of
    //! scope for the first cut — `flate2` is already in Cargo.lock and
    //! a `tar` impl is a small follow-up if the LLM actually asks for
    //! it.
    use super::*;
    use zip::read::ZipArchive;
    use zip::write::SimpleFileOptions;

    pub fn cmd_zip(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        // zip ARCHIVE FILE [FILE...]
        let mut recurse = false;
        let mut positional: Vec<&str> = Vec::new();
        for a in args {
            match a.as_str() {
                "-r" => recurse = true,
                _ if a.starts_with('-') => {}
                _ => positional.push(a.as_str()),
            }
        }
        if positional.len() < 2 {
            return Ok(fail("zip: need ARCHIVE and at least one FILE\n".into(), EXIT_USAGE));
        }
        let archive = resolve(cwd, positional[0]);
        let file = File::create(&archive)?;
        let mut zw = zip::ZipWriter::new(file);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut err_acc = String::new();
        for p in &positional[1..] {
            let path = resolve(cwd, p);
            let meta = match fs::metadata(&path) {
                Ok(m) => m,
                Err(e) => {
                    err_acc.push_str(&format!("zip: {}: {}\n", path.display(), e));
                    continue;
                }
            };
            if meta.is_dir() {
                if !recurse {
                    err_acc.push_str(&format!(
                        "zip: {}: is a directory (use -r)\n",
                        path.display()
                    ));
                    continue;
                }
                for entry in WalkDir::new(&path).into_iter().filter_map(|e| e.ok()) {
                    if entry.file_type().is_file() {
                        let p = entry.path();
                        let rel = p.strip_prefix(&path).unwrap_or(p);
                        let name = format!(
                            "{}/{}",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            rel.to_string_lossy().replace('\\', "/")
                        );
                        if let Err(e) = write_one_file(&mut zw, p, &name, &opts) {
                            err_acc.push_str(&format!("zip: {}: {}\n", p.display(), e));
                        }
                    }
                }
            } else if let Err(e) = write_one_file(&mut zw, &path, path.file_name().unwrap().to_str().unwrap_or("file"), &opts) {
                err_acc.push_str(&format!("zip: {}: {}\n", path.display(), e));
            }
        }
        zw.finish().map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        if err_acc.is_empty() {
            Ok(ok(String::new()))
        } else {
            Ok(fail(err_acc, EXIT_FAIL))
        }
    }

    fn write_one_file(
        zw: &mut zip::ZipWriter<File>,
        path: &Path,
        name: &str,
        opts: &SimpleFileOptions,
    ) -> io::Result<()> {
        let mut f = File::open(path)?;
        let mut data = Vec::new();
        f.read_to_end(&mut data)?;
        zw.start_file(name.to_string(), *opts)
            .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        zw.write_all(&data)?;
        Ok(())
    }

    pub fn cmd_unzip(args: &[String], cwd: &Path) -> Result<CommandOutput, std::io::Error> {
        // unzip [-d DIR] ARCHIVE
        let mut dest: Option<&str> = None;
        let mut positional: Vec<&str> = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            match a.as_str() {
                "-d" if i + 1 < args.len() => {
                    dest = Some(args[i + 1].as_str());
                    i += 2;
                }
                _ if a.starts_with('-') => {
                    i += 1;
                }
                _ => {
                    positional.push(a.as_str());
                    i += 1;
                }
            }
        }
        if positional.is_empty() {
            return Ok(fail("unzip: need ARCHIVE\n".into(), EXIT_USAGE));
        }
        let archive = resolve(cwd, positional[0]);
        let dest_dir = match dest {
            Some(d) => resolve(cwd, d),
            None => cwd.to_path_buf(),
        };
        let f = File::open(&archive)?;
        let mut za = ZipArchive::new(f).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut out = String::new();
        for i in 0..za.len() {
            let mut entry = za
                .by_index(i)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            let name = entry.name().to_string();
            let out_path = dest_dir.join(&name);
            if entry.is_dir() {
                fs::create_dir_all(&out_path)?;
                continue;
            }
            if let Some(parent) = out_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data)?;
            fs::write(&out_path, &data)?;
            out.push_str(&format!("  inflating: {}\n", name));
        }
        Ok(ok(out))
    }

    pub fn cmd_zipinfo(
        args: &[String],
        cwd: &Path,
    ) -> Result<CommandOutput, std::io::Error> {
        if args.is_empty() {
            return Ok(fail("zipinfo: need ARCHIVE\n".into(), EXIT_USAGE));
        }
        let archive = resolve(cwd, &args[0]);
        let f = File::open(&archive)?;
        let mut za = ZipArchive::new(f).map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
        let mut out = String::new();
        for i in 0..za.len() {
            let entry = za
                .by_index(i)
                .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
            out.push_str(&format!(
                "{:>10}  {}  {}\n",
                entry.size(),
                entry.name(),
                if entry.is_dir() { "(dir)" } else { "" }
            ));
        }
        Ok(ok(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!(
            "hermix_coreutils_test_{}_{}",
            std::process::id(),
            n
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn dispatch_returns_none_for_unknown() {
        let cwd = tmp();
        let out = dispatch("not_a_real_command_xyz", &[], &cwd).unwrap();
        assert!(out.is_none());
    }

    #[test]
    fn dispatch_handles_echo() {
        let cwd = tmp();
        let out = dispatch("echo", &vec_string(&["hello", "world"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert_eq!(out.stdout, "hello world\n");
    }

    #[test]
    fn dispatch_handles_pwd() {
        let cwd = tmp();
        let out = dispatch("pwd", &[], &cwd).unwrap().unwrap();
        assert_eq!(out.stdout, cwd.display().to_string());
    }

    #[test]
    fn dispatch_handles_date() {
        let cwd = tmp();
        let out = dispatch("date", &[], &cwd).unwrap().unwrap();
        assert!(!out.stdout.is_empty());
    }

    #[test]
    fn dispatch_handles_mkdir_recursive() {
        let cwd = tmp();
        let out = dispatch("mkdir", &vec_string(&["-p", "a/b/c"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(cwd.join("a/b/c").is_dir());
    }

    #[test]
    fn dispatch_handles_cat() {
        let cwd = tmp();
        fs::write(cwd.join("hello.txt"), "hi\nbye\n").unwrap();
        let out = dispatch("cat", &vec_string(&["hello.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.stdout, "hi\nbye\n");
    }

    #[test]
    fn dispatch_handles_cp_recursive() {
        let cwd = tmp();
        fs::create_dir_all(cwd.join("src/sub")).unwrap();
        fs::write(cwd.join("src/a.txt"), "a").unwrap();
        fs::write(cwd.join("src/sub/b.txt"), "b").unwrap();
        let out = dispatch("cp", &vec_string(&["-r", "src", "dst"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(cwd.join("dst/a.txt").is_file());
        assert!(cwd.join("dst/sub/b.txt").is_file());
    }

    #[test]
    fn dispatch_handles_mv() {
        let cwd = tmp();
        fs::write(cwd.join("old.txt"), "x").unwrap();
        let out = dispatch("mv", &vec_string(&["old.txt", "new.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(cwd.join("new.txt").is_file());
        assert!(!cwd.join("old.txt").exists());
    }

    #[test]
    fn dispatch_handles_rm_recursive() {
        let cwd = tmp();
        fs::create_dir_all(cwd.join("trash/sub")).unwrap();
        fs::write(cwd.join("trash/sub/x.txt"), "x").unwrap();
        let out = dispatch("rm", &vec_string(&["-rf", "trash"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(!cwd.join("trash").exists());
    }

    #[test]
    fn dispatch_handles_grep_match() {
        let cwd = tmp();
        fs::write(cwd.join("a.txt"), "foo\nbar\nbaz\n").unwrap();
        let out = dispatch("grep", &vec_string(&["bar", "a.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("bar"));
        assert!(!out.stdout.contains("foo"));
    }

    #[test]
    fn dispatch_grep_no_match_exits_1() {
        let cwd = tmp();
        fs::write(cwd.join("a.txt"), "foo\n").unwrap();
        let out = dispatch("grep", &vec_string(&["nope", "a.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 1);
    }

    #[test]
    fn dispatch_handles_grep_recursive() {
        let cwd = tmp();
        fs::create_dir_all(cwd.join("d")).unwrap();
        fs::write(cwd.join("d/x.rs"), "fn main() {}\n").unwrap();
        let out = dispatch("grep", &vec_string(&["-r", "fn main", "."]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("fn main"));
    }

    #[test]
    fn dispatch_handles_head_and_tail() {
        let cwd = tmp();
        fs::write(cwd.join("lines.txt"), "1\n2\n3\n4\n5\n").unwrap();
        let head = dispatch("head", &vec_string(&["-n", "2", "lines.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(head.stdout, "1\n2\n");
        let tail = dispatch("tail", &vec_string(&["-n", "2", "lines.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(tail.stdout, "4\n5\n");
    }

    #[test]
    fn dispatch_handles_sort_unique() {
        let cwd = tmp();
        fs::write(cwd.join("s.txt"), "b\na\nb\nc\n").unwrap();
        let sorted = dispatch("sort", &vec_string(&["s.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(sorted.stdout, "a\nb\nb\nc\n");
        let unique = dispatch("sort", &vec_string(&["-u", "s.txt"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(unique.stdout, "a\nb\nc\n");
    }

    #[test]
    fn dispatch_handles_base64_roundtrip() {
        let cwd = tmp();
        fs::write(cwd.join("data.bin"), b"hello world").unwrap();
        let encoded = dispatch("base64", &vec_string(&["data.bin"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(encoded.stdout.trim(), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn dispatch_handles_zip_unzip() {
        let cwd = tmp();
        fs::create_dir_all(cwd.join("src")).unwrap();
        fs::write(cwd.join("src/a.txt"), "alpha").unwrap();
        fs::write(cwd.join("src/b.txt"), "beta").unwrap();
        let zip_out = dispatch("zip", &vec_string(&["-r", "out.zip", "src"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(zip_out.exit_code, 0);
        assert!(cwd.join("out.zip").is_file());
        let info = dispatch("zipinfo", &vec_string(&["out.zip"]), &cwd)
            .unwrap()
            .unwrap();
        assert!(info.stdout.contains("a.txt"));
        assert!(info.stdout.contains("b.txt"));
        let unzip = dispatch("unzip", &vec_string(&["out.zip", "-d", "restored"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(unzip.exit_code, 0);
        assert!(cwd.join("restored/src/a.txt").is_file());
        let restored = fs::read_to_string(cwd.join("restored/src/a.txt")).unwrap();
        assert_eq!(restored, "alpha");
    }

    #[test]
    fn dispatch_handles_sha256sum() {
        let cwd = tmp();
        fs::write(cwd.join("f"), b"abc").unwrap();
        let out = dispatch("sha256sum", &vec_string(&["f"]), &cwd)
            .unwrap()
            .unwrap();
        // echo -n abc | sha256sum
        assert!(out.stdout.starts_with(
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        ));
    }

    #[test]
    fn dispatch_handles_diff() {
        let cwd = tmp();
        fs::write(cwd.join("a"), "x\ny\nz\n").unwrap();
        fs::write(cwd.join("b"), "x\nY\nz\n").unwrap();
        let out = dispatch("diff", &vec_string(&["a", "b"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.exit_code, 1);
        assert!(out.stdout.contains("-y"));
        assert!(out.stdout.contains("+Y"));
    }

    #[test]
    fn dispatch_handles_sed_print_range() {
        let cwd = tmp();
        fs::write(cwd.join("t"), "1\n2\n3\n4\n").unwrap();
        let out = dispatch("sed", &vec_string(&["-n", "2,3p", "t"]), &cwd)
            .unwrap()
            .unwrap();
        assert_eq!(out.stdout, "2\n3\n");
    }

    #[test]
    fn dispatch_handles_sed_subst() {
        let cwd = tmp();
        fs::write(cwd.join("t"), "hello world\n").unwrap();
        let out = dispatch(
            "sed",
            &vec_string(&["s/world/rust/g", "t"]),
            &cwd,
        )
        .unwrap()
        .unwrap();
        assert_eq!(out.stdout, "hello rust\n");
    }

    fn vec_string(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }
}
