//! Plugin inspection endpoint.
//!
//! Maps a plugin id from the seeded manifest catalog to the actual tools
//! that are registered in the `ToolRegistry`. A "ready" plugin (e.g.
//! `image_gen`, `web`, `video_gen`) is one whose toolset exists in the
//! registry; a "seeded" plugin (e.g. `kanban`, `model_providers`) is one
//! we have the manifest for but no Rust implementation yet.

use axum::extract::{Path as AxumPath, State};
use axum::Json;
use serde::Serialize;

use super::stubs::resolve_toolset_for;
use crate::HttpServerState;

#[derive(Debug, Serialize)]
pub struct PluginInspect {
    pub id: String,
    pub availability: &'static str,
    pub toolset: Option<String>,
    pub tools: Vec<String>,
    pub total: usize,
}

impl PluginInspect {
    fn unavailable(id: String) -> Self {
        Self {
            id,
            availability: "seeded",
            toolset: None,
            tools: vec![],
            total: 0,
        }
    }
}

/// GET /api/dashboard/plugin-inspect/{id} — return the toolset and
/// tool names registered for a given plugin id. Returns 200 with an
/// empty list (not 404) when the plugin id is unknown — the front-end
/// can use this to fade the card regardless.
pub async fn inspect_plugin(
    State(state): State<HttpServerState>,
    AxumPath(id): AxumPath<String>,
) -> Json<PluginInspect> {
    let toolset_id = resolve_toolset_for(&id);
    let tools = state.tool_registry.list_tools();
    let mut matched: Vec<String> = tools
        .iter()
        .filter(|t| t.toolset == toolset_id)
        .map(|t| t.name.clone())
        .collect();
    matched.sort();
    let total = matched.len();
    if total > 0 {
        Json(PluginInspect {
            id: id.clone(),
            availability: "ready",
            toolset: Some(toolset_id),
            tools: matched,
            total,
        })
    } else {
        Json(PluginInspect::unavailable(id))
    }
}

/// GET /api/dashboard/plugin-inspect — bulk variant. Returns a map of
/// {plugin_id: PluginInspect}. Used by the front-end to refresh the
/// "已挂接" badges in one round-trip after the registry changes.
pub async fn inspect_all_plugins(
    State(state): State<HttpServerState>,
) -> Json<std::collections::HashMap<String, PluginInspect>> {
    let tools = state.tool_registry.list_tools();
    let mut by_id: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for t in &tools {
        by_id.entry(t.toolset.clone()).or_default().push(t.name.clone());
    }
    let mut out: std::collections::HashMap<String, PluginInspect> =
        std::collections::HashMap::new();
    // Surface every known plugin id. The empty ones get `availability: "seeded"`.
    for key in by_id.keys() {
        let mut names = by_id.get(key).cloned().unwrap_or_default();
        names.sort();
        let total = names.len();
        out.insert(
            key.clone(),
            PluginInspect {
                id: key.clone(),
                availability: "ready",
                toolset: Some(key.clone()),
                tools: names,
                total,
            },
        );
    }
    Json(out)
}
