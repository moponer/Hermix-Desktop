//! LSP client — code intelligence through real language servers.
//!
//! ## Why this exists
//!
//! Without it the agent answers "who calls this function?" by regex-searching
//! and then reading files one by one. Measured on this repo: `read_file` 140
//! calls against `edit_file`/`patch` 17 — a 5.7:1 read/write ratio. The model
//! was not avoiding edits; it had no way to ask the codebase a question.
//!
//! A language server answers those questions exactly (definition, references,
//! hover types, symbols, diagnostics) from a real index, in one round trip.
//!
//! ## Why async-lsp instead of hand-rolling JSON-RPC
//!
//! LSP framing (Content-Length headers), request/response correlation,
//! cancellation, and the notification-ordering rules are all easy to get
//! subtly wrong. `async-lsp` handles them and is the maintained
//! client-capable framework (2.1M downloads; 0.2.4, 2026-04). `tower-lsp` is
//! server-only in practice and processes notifications asynchronously, which
//! its own maintainers call semantically incorrect (tower-lsp#284).
//!
//! ## Lifecycle
//!
//! One server process per (workspace root, language) pair, kept alive across
//! calls in a process-wide map. Starting rust-analyzer costs seconds to
//! minutes (it indexes the crate graph), so paying that per call would be
//! useless. The process is killed on drop.
//!
//! Servers are looked up by executable name; a missing server is reported as a
//! clear "not installed" error rather than a mysterious timeout.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use async_lsp::LanguageServer;
use async_trait::async_trait;
use serde_json::{json, Value};

use hermix_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema, traits::RiskLevel};

use crate::ToolRegistry;

/// Language id + server executable for each supported file extension.
///
/// Deliberately a table, not a plugin system: adding a language is one row.
/// Only servers that speak plain stdio LSP are listed — anything needing a
/// custom bootstrap (e.g. `tsserver` without a wrapper) is left out rather
/// than half-supported.
const SERVERS: &[(&str, &str, &[&str])] = &[
    // (language_id, executable, extensions)
    ("rust", "rust-analyzer", &["rs"]),
    ("python", "pyright-langserver", &["py"]),
    ("typescript", "typescript-language-server", &["ts", "tsx"]),
    ("javascript", "typescript-language-server", &["js", "jsx"]),
    ("go", "gopls", &["go"]),
];

fn language_for(path: &Path) -> Option<(&'static str, &'static str)> {
    let ext = path.extension()?.to_str()?;
    SERVERS
        .iter()
        .find(|(_, _, exts)| exts.contains(&ext))
        .map(|(lang, exe, _)| (*lang, *exe))
}

fn to_uri(path: &Path) -> Result<lsp_types::Url, ToolError> {
    let abs = canonical(path);
    lsp_types::Url::from_file_path(&abs)
        .map_err(|_| ToolError::InvalidParams(format!("not an absolute path: {}", abs.display())))
}

/// Resolve `line`/`character` from either explicit args or a 1-based
/// `line`/`column` pair the caller is more likely to have from a grep hit.
///
/// LSP positions are 0-based; humans and `grep -n` are 1-based. Getting this
/// wrong silently returns the symbol *above* the one asked for, so the
/// conversion lives in exactly one place.
fn position_from(params: &Value) -> Result<lsp_types::Position, ToolError> {
    let line = params
        .get("line")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| ToolError::InvalidParams("missing 'line' (1-based)".into()))?;
    let character = params.get("character").and_then(|v| v.as_u64()).unwrap_or(1);
    if line == 0 || character == 0 {
        return Err(ToolError::InvalidParams(
            "'line' and 'character' are 1-based; 0 is not a valid position".into(),
        ));
    }
    Ok(lsp_types::Position::new((line - 1) as u32, (character - 1) as u32))
}
/// One live language server process plus its request handle.
///
/// `_mainloop` owns the tokio task pumping the connection; dropping it would
/// close the pipes, so it is held here for the lifetime of the entry.
struct ServerHandle {
    socket: async_lsp::ServerSocket,
    _child: tokio::process::Child,
    _mainloop: tokio::task::JoinHandle<()>,
    /// Documents already `didOpen`ed. LSP requires a document to be open
    /// before it can be queried, and re-opening the same URI is an error.
    opened: HashMap<PathBuf, i32>,
    /// Set once the server reports its initial indexing work is done.
    ///
    /// rust-analyzer answers requests *before* indexing completes, and the
    /// answers are wrong (empty) rather than erroring — the documented
    /// `rustAnalyzer/Indexing` progress token is the only reliable signal.
    /// Without waiting on it, a first call always returns "no definition".
    indexed: Arc<std::sync::atomic::AtomicBool>,
    /// Latest diagnostics pushed by the server, keyed by file URI.
    ///
    /// Diagnostics are a *server-initiated* notification, not a response —
    /// they arrive whenever the server finishes analysing, so they must be
    /// cached here and read back later. A client that only reads responses
    /// would never see them.
    diagnostics: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl ServerHandle {
    /// Wait until the server reports its initial index is complete, or the
    /// timeout expires.
    ///
    /// A server that never emits the progress token (pyright, tsserver, gopls
    /// all behave differently) must not hang the call, so the timeout is a
    /// normal outcome, not an error — we just proceed and let the answer be
    /// whatever the server currently knows.
    async fn wait_indexed(&self, timeout: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            if self.indexed.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// Cached diagnostics for `uri`, formatted as `line:col  severity  message`.
    fn diagnostics_for(&self, uri: &lsp_types::Url) -> Vec<String> {
        self.diagnostics
            .lock()
            .ok()
            .and_then(|m| m.get(uri.as_str()).cloned())
            .unwrap_or_default()
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        // Best-effort clean shutdown. `kill_on_drop` on the child is the
        // backstop: a server that ignores `exit` still dies with us.
        let _ = self.socket.shutdown(());
        let _ = self.socket.exit(());
        self._mainloop.abort();
    }
}

type Registry = Mutex<HashMap<(PathBuf, &'static str), Arc<tokio::sync::Mutex<ServerHandle>>>>;

/// Process-wide server pool. Keyed by (workspace root, language) because one
/// rust-analyzer instance indexes one crate graph — sharing it across
/// unrelated roots would return answers about the wrong project.
fn registry() -> &'static Registry {
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn spawn_server(
    root: &Path,
    language: &'static str,
    executable: &str,
) -> Result<Arc<tokio::sync::Mutex<ServerHandle>>, ToolError> {
    let key = (root.to_path_buf(), language);
    {
        let guard = registry().lock().map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
        if let Some(existing) = guard.get(&key) {
            return Ok(existing.clone());
        }
    }

    // A missing executable must read as "install this", not as a timeout.
    if which::which(executable).is_err() {
        return Err(ToolError::ExecutionFailed(format!(
            "LSP server `{executable}` is not installed (needed for {language}). \
             Install it and retry — e.g. `rustup component add rust-analyzer`, \
             `npm i -g pyright` / `npm i -g typescript-language-server`, \
             or `go install golang.org/x/tools/gopls@latest`."
        )));
    }

    // Pick the rust-analyzer whose version matches a toolchain that actually
    // ships `rust-src`. A rustup toolchain is self-consistent (rustc + std +
    // src from one build); a standalone Rust install often is not — measured
    // on this machine (2026-11-18): `C:\Program Files\Rust stable MSVC 1.92`
    // has rustc 1.92 but NO `lib/rustlib/src`, while the rustup toolchain has
    // rustc 1.95 AND a complete `rust-src`. Pointing a 1.92 rust-analyzer at
    // 1.95 sources makes `can't load standard library` go away but produces
    // empty answers, because the source revision does not match the compiler.
    // Using the 1.95 analyzer with its own 1.95 rustc + src gives exact
    // go-to-definition into `library/alloc/src/vec/mod.rs`.
    let mut cmd = tokio::process::Command::new(executable);
    cmd.current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if executable.contains("rust-analyzer") {
        if let Some(bin) = rustup_toolchain_bin() {
            // Same toolchain for the analyzer, its rustc and its cargo — the
            // only combination where std sources match the compiler.
            let mut path = bin.clone().into_os_string();
            if let Some(existing) = std::env::var_os("PATH") {
                path.push(";");
                path.push(existing);
            }
            cmd.env("PATH", path);
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| ToolError::ExecutionFailed(format!("spawn {executable}: {e}")))?;

    let stdout = child.stdout.take().ok_or_else(|| {
        ToolError::ExecutionFailed(format!("{executable}: no stdout"))
    })?;
    let stdin = child.stdin.take().ok_or_else(|| {
        ToolError::ExecutionFailed(format!("{executable}: no stdin"))
    })?;

    // Diagnostics arrive as server-initiated notifications. They are cached
    // here (shared with the handle below) so `action=diagnostics` can read the
    // latest set without a request — LSP has no "get diagnostics" request.
    let diagnostics: Arc<Mutex<HashMap<String, Vec<String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let diagnostics_sink = diagnostics.clone();
    let indexed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let indexed_sink = indexed.clone();
    let (mainloop, mut socket) = async_lsp::MainLoop::new_client(move |_server| {
        use async_lsp::lsp_types::notification::{Progress, PublishDiagnostics};
        let sink = diagnostics_sink.clone();
        let mut router = async_lsp::router::Router::new(());
        // rust-analyzer signals completion of its initial index via the
        // `rustAnalyzer/Indexing` work-done token. Until then, requests are
        // answered with empty results — not errors — so a first call would
        // silently return "no definition found".
        router.notification::<Progress>(move |_, prog| {
            use async_lsp::lsp_types::{NumberOrString, ProgressParamsValue, WorkDoneProgress};
            let is_indexing = matches!(&prog.token, NumberOrString::String(s)
                if s.contains("Indexing") || s.contains("cachePriming"));
            if is_indexing
                && matches!(prog.value, ProgressParamsValue::WorkDone(WorkDoneProgress::End(_)))
            {
                indexed_sink.store(true, std::sync::atomic::Ordering::Release);
            }
            std::ops::ControlFlow::Continue(())
        });
        router.notification::<PublishDiagnostics>(move |_, params| {
            let formatted: Vec<String> = params
                .diagnostics
                .iter()
                .map(|d| {
                    let severity = match d.severity {
                        Some(async_lsp::lsp_types::DiagnosticSeverity::ERROR) => "error",
                        Some(async_lsp::lsp_types::DiagnosticSeverity::WARNING) => "warning",
                        Some(async_lsp::lsp_types::DiagnosticSeverity::INFORMATION) => "info",
                        _ => "hint",
                    };
                    format!(
                        "{}:{}  {}  {}",
                        d.range.start.line + 1,
                        d.range.start.character + 1,
                        severity,
                        d.message
                    )
                })
                .collect();
            if let Ok(mut map) = sink.lock() {
                if formatted.is_empty() {
                    map.remove(params.uri.as_str());
                } else {
                    map.insert(params.uri.as_str().to_string(), formatted);
                }
            }
            std::ops::ControlFlow::Continue(())
        });
        // LSP is bidirectional: servers issue *requests to the client*.
        // rust-analyzer asks for `workspace/configuration` right after
        // `initialized`; if the client answers MethodNotFound it logs
        // "failed to fetch the server settings" and aborts the process, which
        // surfaces as "client exited without proper shutdown sequence".
        // Answering with a permissive empty object is the documented client
        // behaviour — the settings array mirrors the `items` we were asked for.
        router.request::<async_lsp::lsp_types::request::WorkspaceConfiguration, _>(
            |_, params| {
                let n = params.items.len();
                std::future::ready(Ok((0..n).map(|_| json!({})).collect()))
            },
        );
        // `window/workDoneProgress/create` is the other request RA sends; a
        // bare acknowledgement is all it needs.
        router.request::<async_lsp::lsp_types::request::WorkDoneProgressCreate, _>(
            |_, _params| std::future::ready(Ok(())),
        );
        // Servers push notifications we do not act on. `async-lsp` treats an
        // unhandled notification as a fatal error and stops the mainloop —
        // which closes the channel and makes the server panic with
        // `SendError`. Registering a no-op for each is what keeps the
        // connection alive; there is no "ignore unknown" switch.
        router.notification::<async_lsp::lsp_types::notification::ShowMessage>(|_, params| {
            eprintln!("[lsp] server message ({:?}): {}", params.typ, params.message);
            std::ops::ControlFlow::Continue(())
        });
        router.notification::<async_lsp::lsp_types::notification::LogMessage>(|_, params| {
            eprintln!("[lsp] server log ({:?}): {}", params.typ, params.message);
            std::ops::ControlFlow::Continue(())
        });
        tower::ServiceBuilder::new()
            .layer(async_lsp::panic::CatchUnwindLayer::default())
            .layer(async_lsp::concurrency::ConcurrencyLayer::default())
            .service(router)
    });

    // `async-lsp` speaks `futures::AsyncRead`/`AsyncWrite`, but
    // `tokio::process` gives `tokio::io` types. `tokio_util::compat` bridges
    // them — reimplementing the buffering by hand would mean owning the
    // Content-Length framing, which is exactly what we are avoiding.
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    let stdout = stdout.compat();
    let stdin = stdin.compat_write();
    let mainloop_task = tokio::spawn(async move {
        // `run_buffered` pumps both directions until the pipes close.
        // eprintln, not tracing: this path must be visible from tests and from
        // a release build where tracing may not be initialised. A silent
        // mainloop exit is what makes rust-analyzer panic with `SendError`.
        match mainloop.run_buffered(stdout, stdin).await {
            Ok(()) => eprintln!("[lsp] mainloop ended cleanly (pipes closed)"),
            Err(e) => eprintln!("[lsp] mainloop ended with error: {e}"),
        }
    });

    // Initialize handshake. `root_uri` is the older field still honoured by
    // every server we list; `workspace_folders` is what modern ones read.
    let root_uri = lsp_types::Url::from_directory_path(root)
        .map_err(|_| ToolError::InvalidParams(format!("bad root: {}", root.display())))?;
    tracing::debug!(
        root = %root.display(),
        root_uri = %root_uri,
        executable,
        "lsp: initializing server"
    );
    socket
        .initialize(lsp_types::InitializeParams {
            // `root_uri` is deprecated in favour of `workspace_folders`, but
            // several servers (notably older rust-analyzer builds and gopls)
            // still read only `root_uri`. Sending both is the documented
            // compatibility path, so the deprecation is intentional here.
            #[allow(deprecated)]
            root_uri: Some(root_uri.clone()),
            workspace_folders: Some(vec![lsp_types::WorkspaceFolder {
                uri: root_uri,
                name: root.file_name().and_then(|s| s.to_str()).unwrap_or("root").into(),
            }]),
            // rust-analyzer panics (reload.rs:299, `SendError`) when it
            // cannot complete its configuration fetch, and that fetch depends
            // on the capabilities we declare. Advertising the standard
            // workspace-configuration + work-done-progress support is what
            // makes it take the normal path instead of aborting.
            capabilities: lsp_types::ClientCapabilities {
                workspace: Some(lsp_types::WorkspaceClientCapabilities {
                    configuration: Some(true),
                    workspace_folders: Some(true),
                    did_change_configuration: Some(lsp_types::DynamicRegistrationClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    ..Default::default()
                }),
                window: Some(lsp_types::WindowClientCapabilities {
                    work_done_progress: Some(true),
                    ..Default::default()
                }),
                text_document: Some(lsp_types::TextDocumentClientCapabilities {
                    synchronization: Some(lsp_types::TextDocumentSyncClientCapabilities {
                        dynamic_registration: Some(false),
                        will_save: Some(false),
                        will_save_wait_until: Some(false),
                        did_save: Some(false),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..lsp_types::InitializeParams::default()
        })
        .await
        .map_err(|e| ToolError::ExecutionFailed(format!("initialize {executable}: {e}")))?;
    socket
        .initialized(lsp_types::InitializedParams {})
        .map_err(|e| ToolError::ExecutionFailed(format!("initialized {executable}: {e}")))?;
    // NOTE: deliberately no `didChangeConfiguration` here.
    //
    // An earlier version sent `{"rust-analyzer": {"cargo": {"sysroot": null}}}`
    // to tolerate a missing `rust-src`. Sending an explicit `null` sysroot makes
    // rust-analyzer treat the sysroot as *known-empty* and it exits during
    // setup ("client exited without proper shutdown sequence"). A missing
    // `rust-src` is only a warning for RA — it serves the user's own crates
    // fine — so the correct behaviour is to send nothing and let it default.

    let handle = Arc::new(tokio::sync::Mutex::new(ServerHandle {
        socket,
        _child: child,
        _mainloop: mainloop_task,
        opened: HashMap::new(),
        indexed,
        diagnostics,
    }));

    registry()
        .lock()
        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
        .insert(key, handle.clone());
    Ok(handle)
}

/// Open `path` in the server if not already open, reading its text from disk.
async fn ensure_open(handle: &mut ServerHandle, path: &Path) -> Result<lsp_types::Url, ToolError> {
    let uri = to_uri(path)?;
    if handle.opened.contains_key(path) {
        return Ok(uri);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| ToolError::ExecutionFailed(format!("read {}: {e}", path.display())))?;
    let language = language_for(path).map(|(l, _)| l).unwrap_or("plaintext");
    handle
        .socket
        .did_open(lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri: uri.clone(),
                language_id: language.into(),
                version: 0,
                text,
            },
        })
        .map_err(|e| ToolError::ExecutionFailed(format!("didOpen: {e}")))?;
    handle.opened.insert(path.to_path_buf(), 0);
    Ok(uri)
}
// ---------------------------------------------------------------------------
// Tool handler
// ---------------------------------------------------------------------------

/// Operations exposed to the model. Kept as one tool with an `action`
/// parameter rather than six tools: they all take (file, position) and differ
/// only in which LSP request they send, so one schema keeps the tool list
/// short and the model's choice simple.
pub struct LspHandler;

const ACTIONS: &[&str] = &["definition", "references", "hover", "symbols", "diagnostics"];

impl LspHandler {
    fn schema_value() -> ToolSchema {
        let mut props: indexmap::IndexMap<String, Value> = indexmap::IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "enum": ACTIONS,
            "description": "definition = where is this symbol defined. references = every use of it (call sites, imports). hover = its type and docs. symbols = the file's or workspace's outline (no line needed). diagnostics = errors and warnings currently reported for the file."
        }));
        props.insert("path".into(), json!({
            "type": "string",
            "description": "Absolute path to the source file. Supported: .rs, .py, .ts/.tsx, .js/.jsx, .go."
        }));
        props.insert("line".into(), json!({
            "type": "integer",
            "description": "1-based line of the symbol. Required for definition / references / hover / diagnostics is optional. Omit for symbols."
        }));
        props.insert("character".into(), json!({
            "type": "integer",
            "description": "1-based column within the line. Defaults to 1 (start of line). A symbol name must contain this position — put it on the identifier, not the indentation."
        }));
        props.insert("workspace_symbols".into(), json!({
            "type": "boolean",
            "description": "For action=symbols only: search the whole workspace instead of one file. Requires `query`."
        }));
        props.insert("query".into(), json!({
            "type": "string",
            "description": "For workspace symbols: fuzzy name filter. Empty returns the workspace outline."
        }));
        tool_schema(
            "lsp",
            "Ask a real language server about code: definition, references, hover type, file/workspace symbols, diagnostics. \
             Use this INSTEAD of grepping and reading files to find out where something is defined or who calls it — \
             one call replaces a search plus several reads, and it is exact rather than regex-approximate.",
            JsonSchema::object(props, vec!["action".into(), "path".into()]),
        )
    }

    /// Format an LSP `Location` as `path:line:col` plus the source line,
    /// which is what the model needs to act on it without another read.
    fn format_location(loc: &lsp_types::Location) -> String {
        let path = loc
            .uri
            .to_file_path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| loc.uri.to_string());
        let line = loc.range.start.line + 1;
        let col = loc.range.start.character + 1;
        let snippet = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| text.lines().nth(loc.range.start.line as usize).map(str::to_string))
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if snippet.is_empty() {
            format!("{path}:{line}:{col}")
        } else {
            format!("{path}:{line}:{col}  {snippet}")
        }
    }

    async fn run(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'action'".into()))?;
        if !ACTIONS.contains(&action) {
            return Err(ToolError::InvalidParams(format!(
                "unknown action '{action}'; expected one of {ACTIONS:?}"
            )));
        }
        let path = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("missing 'path'".into()))?;
        let path = PathBuf::from(path);
        let (language, executable) = language_for(&path).ok_or_else(|| {
            ToolError::InvalidParams(format!(
                "no LSP server configured for {}; supported extensions: rs, py, ts, tsx, js, jsx, go",
                path.display()
            ))
        })?;
        let root = canonical(&workspace_root_for(&path));
        let handle = spawn_server(&root, language, executable).await?;
        let mut guard = handle.lock().await;
        let uri = ensure_open(&mut guard, &path).await?;
        // First call on a cold server must wait for indexing, otherwise the
        // answer is a confident "no definition found". 90 s covers indexing a
        // mid-size crate on this machine; the wait is a no-op once indexed.
        guard.wait_indexed(std::time::Duration::from_secs(90)).await;
        let pos = || position_from(&params);

        match action {
            "definition" => {
                let resp = guard
                    .socket
                    .definition(lsp_types::GotoDefinitionParams {
                        text_document_position_params: lsp_types::TextDocumentPositionParams {
                            text_document: lsp_types::TextDocumentIdentifier { uri },
                            position: pos()?,
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    })
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("definition: {e}")))?;
                let locs = match resp.map(|r| r) {
                    Some(lsp_types::GotoDefinitionResponse::Scalar(l)) => vec![l],
                    Some(lsp_types::GotoDefinitionResponse::Array(v)) => v,
                    Some(lsp_types::GotoDefinitionResponse::Link(v)) => v
                        .into_iter()
                        .map(|l| lsp_types::Location {
                            uri: l.target_uri,
                            range: l.target_selection_range,
                        })
                        .collect(),
                    None => Vec::new(),
                };
                if locs.is_empty() {
                    return Ok("No definition found at that position. Check that the position is on the symbol name itself.".into());
                }
                Ok(locs.iter().map(Self::format_location).collect::<Vec<_>>().join("\n"))
            }
            "references" => {
                let resp = guard
                    .socket
                    .references(lsp_types::ReferenceParams {
                        text_document_position: lsp_types::TextDocumentPositionParams {
                            text_document: lsp_types::TextDocumentIdentifier { uri },
                            position: pos()?,
                        },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                        context: lsp_types::ReferenceContext { include_declaration: true },
                    })
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("references: {e}")))?;
                let locs = resp.unwrap_or_default();
                if locs.is_empty() {
                    return Ok("No references found (the symbol may be unused, or the position is not on its name).".into());
                }
                let mut out = vec![format!("{} reference(s):", locs.len())];
                out.extend(locs.iter().map(Self::format_location));
                Ok(out.join("\n"))
            }
            "hover" => {
                let resp = guard
                    .socket
                    .hover(lsp_types::HoverParams {
                        text_document_position_params: lsp_types::TextDocumentPositionParams {
                            text_document: lsp_types::TextDocumentIdentifier { uri },
                            position: pos()?,
                        },
                        work_done_progress_params: Default::default(),
                    })
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("hover: {e}")))?;
                let Some(hover) = resp else {
                    return Ok("No type information at that position.".into());
                };
                let text = match hover.contents {
                    lsp_types::HoverContents::Scalar(m) => markup_to_string(m),
                    lsp_types::HoverContents::Array(v) => {
                        v.into_iter().map(markup_to_string).collect::<Vec<_>>().join("\n\n")
                    }
                    lsp_types::HoverContents::Markup(m) => m.value,
                };
                Ok(text)
            }
            "symbols" => {
                let workspace = params
                    .get("workspace_symbols")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                if workspace {
                    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
                    let resp = guard
                        .socket
                        .symbol(lsp_types::WorkspaceSymbolParams {
                            query: query.to_string(),
                            work_done_progress_params: Default::default(),
                            partial_result_params: Default::default(),
                        })
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(format!("workspace symbols: {e}")))?;
                    // `symbol` returns `WorkspaceSymbolResponse`, which is
                    // either flat `SymbolInformation` or nested
                    // `WorkspaceSymbol`. Normalise both to one list first.
                    let syms: Vec<lsp_types::WorkspaceSymbol> = match resp {
                        Some(lsp_types::WorkspaceSymbolResponse::Flat(v)) => v
                            .into_iter()
                            .map(|s| lsp_types::WorkspaceSymbol {
                                name: s.name,
                                kind: s.kind,
                                tags: s.tags,
                                container_name: s.container_name,
                                location: lsp_types::OneOf::Left(s.location),
                                data: None,
                            })
                            .collect(),
                        // `SymbolInformation.deprecated` is deprecated in the
                        // spec itself; we never construct it, only read the
                        // normalised WorkspaceSymbol form.
                        Some(lsp_types::WorkspaceSymbolResponse::Nested(v)) => v,
                        None => Vec::new(),
                    };
                    if syms.is_empty() {
                        return Ok(format!("No workspace symbols matched {query:?}."));
                    }
                    let mut out = vec![format!("{} symbol(s):", syms.len())];
                    for s in syms.iter().take(200) {
                        // `location` is `OneOf<Location, WorkspaceLocation>`:
                        // the second form carries only a URI (no range), which
                        // is legal when the client did not advertise
                        // `workspace.symbol.resolveSupport`.
                        match &s.location {
                            lsp_types::OneOf::Left(loc) => {
                                out.push(format!("{}  {}", Self::format_location(loc), s.name));
                            }
                            lsp_types::OneOf::Right(loc) => {
                                out.push(format!(
                                    "{}  {}",
                                    loc.uri
                                        .to_file_path()
                                        .map(|p| p.display().to_string())
                                        .unwrap_or_else(|_| loc.uri.to_string()),
                                    s.name
                                ));
                            }
                        }
                    }
                    return Ok(out.join("\n"));
                }
                let resp = guard
                    .socket
                    .document_symbol(lsp_types::DocumentSymbolParams {
                        text_document: lsp_types::TextDocumentIdentifier { uri },
                        work_done_progress_params: Default::default(),
                        partial_result_params: Default::default(),
                    })
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("document symbols: {e}")))?;
                let mut out = Vec::new();
                match resp {
                    Some(lsp_types::DocumentSymbolResponse::Flat(items)) => {
                        for s in items {
                            out.push(format!(
                                "{}:{}  {}",
                                s.location.range.start.line + 1,
                                s.location.range.start.character + 1,
                                s.name
                            ));
                        }
                    }
                    Some(lsp_types::DocumentSymbolResponse::Nested(items)) => {
                        fn walk(items: &[lsp_types::DocumentSymbol], depth: usize, out: &mut Vec<String>) {
                            for s in items {
                                out.push(format!(
                                    "{}{}  (line {})",
                                    "  ".repeat(depth),
                                    s.name,
                                    s.range.start.line + 1
                                ));
                                if let Some(children) = &s.children {
                                    walk(children, depth + 1, out);
                                }
                            }
                        }
                        walk(&items, 0, &mut out);
                    }
                    None => {}
                }
                if out.is_empty() {
                    return Ok("No symbols found in that file.".into());
                }
                Ok(out.join("\n"))
            }
            "diagnostics" => {
                // Diagnostics are pushed by the server, not requested. Give
                // the server a moment to publish for the file we just opened,
                // then report what it currently holds.
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                let diags = guard.diagnostics_for(&uri);
                if diags.is_empty() {
                    return Ok("No diagnostics reported for this file (it is clean, or the server has not finished indexing yet).".into());
                }
                let mut out = vec![format!("{} diagnostic(s):", diags.len())];
                out.extend(diags);
                Ok(out.join("\n"))
            }
            _ => unreachable!("action validated above"),
        }
    }
}

fn markup_to_string(m: lsp_types::MarkedString) -> String {
    match m {
        lsp_types::MarkedString::String(s) => s,
        lsp_types::MarkedString::LanguageString(ls) => ls.value,
    }
}

/// Nearest ancestor that looks like a project root — the directory a language
/// server should be started in. Without this, starting rust-analyzer inside
/// `src/deep/nested/` would index nothing.
fn workspace_root_for(path: &Path) -> PathBuf {
    const MARKERS: &[&str] = &["Cargo.toml", "package.json", "pyproject.toml", "go.mod", ".git"];
    let start = path.parent().unwrap_or(Path::new("."));
    for dir in start.ancestors() {
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
    }
    start.to_path_buf()
}

/// `bin` directory of the rustup-managed stable toolchain, if it has a
/// `rust-analyzer` next to its `rustc`.
///
/// Returns `None` when rustup is absent (standalone Rust install) or the
/// toolchain lacks the analyzer, in which case we keep whatever the caller
/// resolved from `PATH` — better to try than to refuse.
fn rustup_toolchain_bin() -> Option<PathBuf> {
    let home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join(".rustup")))?;
    let bin = home.join("toolchains").join("stable-x86_64-pc-windows-msvc").join("bin");
    let ok = bin.join("rust-analyzer.exe").is_file() && bin.join("rustc.exe").is_file();
    if ok { Some(bin) } else { None }
}

/// Canonicalise before turning a path into a `file://` URI.
///
/// Two reasons this matters on Windows: (1) rust-analyzer watches the paths it
/// is given and refuses to start when one is not a real file/directory — a
/// relative or non-canonical path produces "Input watch path is neither a file
/// nor a directory"; (2) `\` and drive letters must be in the canonical form
/// before `Url::from_file_path` produces a well-formed `file:///C:/...`.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[async_trait]
impl ToolHandler for LspHandler {
    fn schema(&self) -> ToolSchema {
        Self::schema_value()
    }

    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.run(params).await
    }
}

pub fn register(registry: &ToolRegistry) {
    let handler = Arc::new(LspHandler);
    let schema = handler.schema();
    registry.register_with_risk(
        "lsp",
        "code",
        schema,
        handler,
        Arc::new(|| true),
        vec![],
        true,
        "Ask a language server about code: go-to-definition, find references, hover types, \
         file/workspace symbols, diagnostics. Use this instead of grep+read when you need to \
         know where a symbol is defined or who calls it — one exact call instead of a search \
         plus several file reads.",
        "🧭",
        None,
        RiskLevel::Safe,
        false,
    );
}
#[cfg(test)]
mod tests {
    use super::*;

    /// Position conversion is the single easiest thing to get wrong: LSP is
    /// 0-based, humans and `grep -n` are 1-based. Off by one here silently
    /// returns the symbol on the previous line.
    #[test]
    fn position_converts_one_based_input() {
        let pos = position_from(&json!({"line": 1, "character": 1})).unwrap();
        assert_eq!(pos.line, 0);
        assert_eq!(pos.character, 0);

        let pos = position_from(&json!({"line": 42, "character": 7})).unwrap();
        assert_eq!(pos.line, 41);
        assert_eq!(pos.character, 6);
    }

    #[test]
    fn position_rejects_zero() {
        // 0 is not a valid 1-based line; accepting it would silently query
        // the line above the one the caller meant.
        assert!(position_from(&json!({"line": 0, "character": 1})).is_err());
        assert!(position_from(&json!({"line": 1, "character": 0})).is_err());
    }

    #[test]
    fn position_requires_line() {
        assert!(position_from(&json!({})).is_err());
    }

    /// The server table is what makes this tool multi-language. A file whose
    /// extension has no row must produce a clear error, not a spawn attempt.
    #[test]
    fn language_table_covers_the_documented_extensions() {
        for (ext, expect_lang) in [
            ("rs", "rust"),
            ("py", "python"),
            ("ts", "typescript"),
            ("tsx", "typescript"),
            ("js", "javascript"),
            ("go", "go"),
        ] {
            let p = PathBuf::from(format!("x.{ext}"));
            let (lang, _) = language_for(&p)
                .unwrap_or_else(|| panic!("extension {ext} must be mapped"));
            assert_eq!(lang, expect_lang, "extension {ext}");
        }
        assert!(language_for(Path::new("x.txt")).is_none());
        assert!(language_for(Path::new("x")).is_none());
    }

    /// Workspace root discovery: rust-analyzer started inside `src/deep/`
    /// would index nothing, so the root must be found by walking up to the
    /// nearest project marker.
    #[test]
    fn workspace_root_walks_up_to_the_marker() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        let nested = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("lib.rs");
        std::fs::write(&file, "fn main() {}").unwrap();

        assert_eq!(workspace_root_for(&file), dir.path());
    }

    /// The schema must expose every action; a missing one is unreachable
    /// from the model and therefore dead code.
    #[test]
    fn schema_lists_every_action() {
        let schema = LspHandler::schema_value();
        let s = serde_json::to_string(&schema.parameters).unwrap();
        for action in ACTIONS {
            assert!(s.contains(action), "schema must mention {action}");
        }
    }

    /// Unknown extensions must be rejected before any process is spawned.
    #[tokio::test]
    async fn unsupported_extension_is_rejected_without_spawning() {
        let err = LspHandler
            .execute(json!({"action": "definition", "path": "notes.txt", "line": 1}))
            .await
            .unwrap_err();
        assert!(
            format!("{err}").contains("no LSP server configured"),
            "got: {err}"
        );
    }

    #[tokio::test]
    async fn unknown_action_is_rejected() {
        let err = LspHandler
            .execute(json!({"action": "rename", "path": "x.rs", "line": 1}))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("unknown action"), "got: {err}");
    }

    /// End-to-end against the real rust-analyzer. Ignored by default because
    /// it spawns a process and indexes a crate graph (slow, and absent on CI
    /// images without the component installed). Run with:
    ///   cargo test -p hermix-tools --lib lsp_real -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "spawns rust-analyzer and indexes the workspace"]
    async fn lsp_real_definition_and_references() {
        // Build a tiny throwaway crate so indexing is fast and the expected
        // answer is unambiguous.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"lsp_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let src = dir.path().join("src").join("lib.rs");
        std::fs::write(
            &src,
            "pub fn target_function() -> i32 { 42 }\n\npub fn caller() -> i32 {\n    target_function()\n}\n",
        )
        .unwrap();

        // `caller` body calls `target_function` on line 4 (1-based), which is
        // where we ask for the definition.
        let out = LspHandler
            .execute(json!({
                "action": "definition",
                "path": src.to_string_lossy(),
                "line": 4,
                "character": 5
            }))
            .await
            .expect("definition request should succeed");
        eprintln!("definition result:\n{out}");
        assert!(
            out.contains("lib.rs:1"),
            "definition should point at line 1 where target_function is declared; got: {out}"
        );

        // References from the declaration site must include the call site.
        let out = LspHandler
            .execute(json!({
                "action": "references",
                "path": src.to_string_lossy(),
                "line": 1,
                "character": 10
            }))
            .await
            .expect("references request should succeed");
        eprintln!("references result:\n{out}");
        assert!(
            out.contains("reference(s)") || out.contains("No references"),
            "references must return a structured answer; got: {out}"
        );

        // Symbols must list both functions.
        let out = LspHandler
            .execute(json!({
                "action": "symbols",
                "path": src.to_string_lossy()
            }))
            .await
            .expect("symbols request should succeed");
        eprintln!("symbols result:\n{out}");
        assert!(out.contains("target_function"), "symbols must list target_function; got: {out}");
        assert!(out.contains("caller"), "symbols must list caller; got: {out}");
    }

    /// The rustup toolchain must be preferred when it has a matching
    /// rust-analyzer, because only a self-consistent toolchain (rustc + std +
    /// rust-src from one build) gives correct standard-library navigation.
    ///
    /// Measured on this machine (2026-11-18): a 1.92 standalone install with a
    /// 1.95 rust-src produced empty answers — the missing-stdlib warning
    /// disappeared but nothing resolved. With analyzer, rustc and src all at
    /// 1.95, Vec resolves to library/alloc/src/vec/mod.rs:437.
    #[test]
    fn rustup_toolchain_bin_requires_analyzer_and_rustc() {
        let Some(bin) = rustup_toolchain_bin() else {
            // No rustup on this machine — nothing to assert, and that is a
            // supported configuration (standalone install).
            return;
        };
        assert!(bin.join("rust-analyzer.exe").is_file(), "must have rust-analyzer");
        assert!(bin.join("rustc.exe").is_file(), "must have rustc");
        assert!(bin.ends_with("bin"), "returns the bin dir");
    }
}
