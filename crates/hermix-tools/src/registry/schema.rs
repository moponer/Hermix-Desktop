//! Registry protocol / schema generation: tool definitions for the LLM,
//! catalog, markdown reference, and JSON result/error formatting.
use std::sync::atomic::Ordering;

use hermix_core::{ToolCatalogEntry, ToolSchema};

use super::ToolRegistry;
use super::entry::{ToolEntry, ToolEntryInfo};

// ---------------------------------------------------------------------------
// LLM-facing schema / catalog generation
// ---------------------------------------------------------------------------

impl ToolRegistry {
    /// Get tool definitions for all tools whose `check_fn` returns true
    /// **and** whose `is_enabled` flag is set.
    /// Both gates are required: a tool that is capability-OK (env
    /// vars present) but user-disabled will be hidden from the LLM;
    /// a tool that is capability-broken (missing env) will also be
    /// hidden even if user-enabled. This is used to build the tool
    /// list sent to the LLM.
    ///
    /// **Sorted by name.** The backing store is a `HashMap`, so the raw
    /// iteration order differs between processes. This list reaches the
    /// LLM verbatim (as the `tools` request parameter), and that
    /// parameter sits inside the provider's cached prefix — an unstable
    /// order invalidates the prefix on every restart. Sorting makes the
    /// order a pure function of the tool *set*.
    pub fn get_definitions(&self) -> Vec<ToolSchema> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        let mut out: Vec<ToolSchema> = inner
            .tools
            .values()
            .filter(|entry| {
                (entry.check_fn)() && entry.is_enabled.load(Ordering::Acquire)
            })
            .map(|entry| entry.schema.clone())
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Generate a markdown reference file listing all registered tools.
    /// The LLM can `read_file` it on demand instead of bloating the prompt.
    pub fn write_tools_reference(&self, path: &std::path::Path) -> Result<(), String> {
        let inner = self.inner.read().map_err(|e| format!("lock: {e}"))?;
        let mut md = String::from("# Tool Reference\n\n");
        md.push_str("Read this file when you need a tool's parameters.\n\n");

        let mut tools: Vec<&ToolEntry> = inner.tools.values().filter(|e| e.is_enabled.load(Ordering::Acquire)).collect();
        tools.sort_by(|a, b| a.schema.name.cmp(&b.schema.name));

        for entry in &tools {
            let s = &entry.schema;
            md.push_str(&format!("## {}\n", s.name));
            md.push_str(&format!("- Toolset: {} | Risk: {:?}\n", entry.toolset, entry.risk_level));
            let desc = s.description.trim();
            if !desc.is_empty() {
                md.push_str(&format!("- {}\n", desc));
            }
            if let Some(props) = &s.parameters.properties {
                let required = s.parameters.required.as_deref().unwrap_or_default();
                for (pname, pinfo) in props {
                    let ptype = pinfo.get("type").and_then(|v| v.as_str()).unwrap_or("any");
                    let pdesc = pinfo.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    let req = if required.contains(pname) { " *" } else { "" };
                    if !pdesc.is_empty() {
                        md.push_str(&format!("  - `{}`: `{}`{} — {}\n", pname, ptype, req, pdesc));
                    } else {
                        md.push_str(&format!("  - `{}`: `{}`{}\n", pname, ptype, req));
                    }
                }
            }
            md.push('\n');
        }
        std::fs::write(path, &md).map_err(|e| format!("write failed: {e}"))?;
        Ok(())
    }

    /// Get a reference to a tool entry by name.
    /// Note: This returns a cloned `Arc<ToolEntry>` because we cannot return
    /// a reference to data behind a Mutex lock. Returns `None` if not found.
    pub fn get_tool(&self, name: &str) -> Option<ToolEntryInfo> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        inner.tools.get(name).map(|e| ToolEntryInfo {
            name: e.name.clone(),
            toolset: e.toolset.clone(),
            description: e.description.clone(),
            emoji: e.emoji.clone(),
            is_async: e.is_async,
            is_enabled: e.is_enabled.load(Ordering::Acquire),
            env_deps: e.env_deps.clone(),
            max_result_size_chars: e.max_result_size_chars,
        })
    }

    /// List all registered tool entries.
    ///
    /// Sorted by name — same determinism reason as [`Self::get_definitions`].
    pub fn list_tools(&self) -> Vec<ToolEntryInfo> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        let mut out: Vec<ToolEntryInfo> = inner
            .tools
            .values()
            .map(|e| ToolEntryInfo {
                name: e.name.clone(),
                toolset: e.toolset.clone(),
                description: e.description.clone(),
                emoji: e.emoji.clone(),
                is_async: e.is_async,
                is_enabled: e.is_enabled.load(Ordering::Acquire),
                env_deps: e.env_deps.clone(),
                max_result_size_chars: e.max_result_size_chars,
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// List distinct toolset names.
    pub fn list_toolsets(&self) -> Vec<String> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        let mut sets: Vec<String> = inner.tools.values().map(|e| e.toolset.clone()).collect();
        sets.sort();
        sets.dedup();
        sets
    }

    /// Names of the tools tagged with `toolset` (empty for an unknown
    /// group). Expert hiring uses this to widen the per-turn allow-list:
    /// an expert brings its toolbox without being able to revoke the
    /// floor or anything the user enabled.
    pub fn toolset_members(&self, toolset: &str) -> Vec<String> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        let mut names: Vec<String> = inner
            .tools
            .values()
            .filter(|e| e.toolset == toolset)
            .map(|e| e.name.clone())
            .collect();
        names.sort();
        names
    }

    /// Catalog — lightweight tool directory for agent startup.
    /// Returns only enabled+available tools, ~50 bytes per entry.
    /// Sorted by name — same determinism reason as [`Self::get_definitions`].
    pub fn catalog(&self) -> Vec<ToolCatalogEntry> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        let mut out: Vec<ToolCatalogEntry> = inner.tools.values()
            .filter(|e| e.is_enabled.load(Ordering::Acquire) && (e.check_fn)())
            .map(|e| hermix_core::ToolCatalogEntry {
                name: e.name.clone(),
                one_line: truncate_one_line(&e.description),
                risk_level: format!("{:?}", e.risk_level).to_lowercase(),
                tags: vec![e.toolset.clone()],
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Look up the full schema for a single tool, so the agent can
    /// call `get_tool_details("name")` after browsing the catalog.
    pub fn get_tool_schema(&self, name: &str) -> Option<ToolSchema> {
        let inner = self.inner.read().expect("registry rwlock not poisoned");
        inner.tools.get(name)
            .filter(|e| e.is_enabled.load(Ordering::Acquire) && (e.check_fn)())
            .map(|e| e.schema.clone())
    }

    /// Format an error as JSON: `{"error": "msg"}`.
    pub fn tool_error(msg: &str) -> String {
        serde_json::json!({ "error": msg }).to_string()
    }

    /// Format a tool result. If the result looks like JSON, pass it through;
    /// otherwise wrap it as `{"result": "..."}`.
    pub fn tool_result(data: &str) -> String {
        if data.starts_with('{') || data.starts_with('[') {
            // Assume already JSON-ish, pass through
            data.to_string()
        } else {
            serde_json::json!({ "result": data }).to_string()
        }
    }
}

/// Trim a description to ~50 chars for the one-line catalog entry.
fn truncate_one_line(desc: &str) -> String {
    let first_line = desc.lines().next().unwrap_or(desc);
    if first_line.len() <= 50 {
        first_line.to_string()
    } else {
        format!("{}…", &first_line[..first_line.floor_char_boundary(47)])
    }
}
