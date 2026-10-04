//! Dashboard management API endpoints.
//!
//! These endpoints mirror the Python `hermix_cli/web_server.py` API surface,
//! providing the `/api/*` routes consumed by the React web dashboard.

pub mod agents;
pub mod custom_apps;
pub mod custom_providers;
pub mod analytics;
pub mod cua;
pub mod automation;
pub mod config;
pub mod env;
pub mod workflows;
pub mod mcp;
pub mod mcp_catalog;
pub mod mcp_scan;
pub mod persona;
pub mod plugins_inspect;
pub mod polish;
pub mod skills;
pub mod stubs;
pub mod transcribe;
pub mod types;

// Re-export shared types used by multiple modules
pub use types::*;
use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::HttpServerState;

/// Build the dashboard API router.
/// All routes are prefixed with `/api/` and share the same `HttpServerState`.
pub fn router() -> Router<HttpServerState> {
    Router::new()
        //Sessions — T2-A : moved to /v1/sessions in lib.rs.
        // The dashboard router no longer mounts any session routes; the
        // SPA uses `/v1/sessions/*` exclusively now.
        // Config
        .route(
            "/api/config",
            get(config::get_config).put(config::update_config),
        )
        // Auto-correct `config.model` so it points to a provider the user
        // actually has a key for. Called by the desktop Settings UI after a
        // key save, and by the server at startup if `config.yaml` is
        // missing. Idempotent — if `config.model` already points to a
        // configured provider the call is a no-op.
        .route("/api/config/auto-correct", post(config::auto_correct_config))
        // 安全中心：自动备份目录统计。
        .route("/api/backup/status", get(config::get_backup_status))
        // Model info
        .route("/api/model/info", get(config::get_model_info))
        // Model selector list (durable, backend-persisted) — fixes the
        // switcher "forgetting" configured models when localStorage is wiped.
        .route(
            "/api/model/list",
            get(config::get_model_list).put(config::update_model_list),
        )
        // Builtin provider metadata (id + env var) — single source of truth
        // in hermix-config; the frontend groups builtin/custom from here.
        .route("/api/providers/meta", get(config::get_provider_meta))
        // Authoritative context-window resolution (value + source tier), so
        // the settings UI stops mirroring the static catalog.
        .route(
            "/api/models/context-windows",
            post(config::resolve_context_windows),
        )
        // Env
        .route(
            "/api/env",
            get(env::get_env_vars)
                .put(env::set_env_var)
                .delete(env::delete_env_var),
        )
        // Reveal a single env var's value (POST so the key doesn't end up in URL/access logs).
        //之前忘了注册这个 route → SettingsView mount 时
        // `POST /api/env/reveal` 直接 404 → 全部 provider fetch 静默失败
        // → localStorage 的 model_config.enabled 永远是空 → ChatView 弹窗
        // 只剩后端当前在跑的默认模型（fallback 合成的那 1 个）。
        .route("/api/env/reveal", post(env::reveal_env_var))
        // Skills & Tools
        .route("/api/skills", get(skills::list_skills))
        .route("/api/skills/toggle", put(skills::toggle_skill))
        .route("/api/skills/{name}", get(skills::get_skill))
        .route("/api/tools/toolsets", get(skills::list_toolsets))
        .route(
            "/api/tools/toolsets/enabled",
            put(skills::set_enabled_toolsets),
        )
        // Analytics
        .route("/api/analytics/usage", get(analytics::get_usage))
        //diagnostic view of the stats DB (existence / row
        // counts / sample rows / applied migrations) so a user can
        // pinpoint where the pipeline is broken without poking at
        // SQLite by hand.
        .route("/api/analytics/debug", get(analytics::get_debug))
        // Dashboard plugins & themes (local bundles under `dashboard-plugins/` + API)
        .route(
            "/api/dashboard/plugins",
            get(stubs::get_plugins).put(stubs::set_plugins),
        )
        .route("/api/dashboard/plugins/rescan", post(stubs::rescan_plugins))
        .route(
            "/api/dashboard/plugin-manifests",
            get(stubs::get_plugin_manifests),
        )
        // MCP server CRUD (config layer; actual spawn handled by hermes-mcp)
        .route(
            "/api/mcp/servers",
            get(mcp::list_servers).post(mcp::create_server),
        )
        .route("/api/mcp/servers/reset", post(mcp::reset_to_official))
        // MCP 社区目录（官方 registry）+ 一键添加
        .route("/api/mcp/catalog", get(mcp_catalog::get_catalog))
        // 批次 M2：多源手动切换（源清单/切换/连通性探测）
        .route(
            "/api/mcp/catalog/sources",
            get(mcp_catalog::get_sources).put(mcp_catalog::set_source),
        )
        .route("/api/mcp/catalog/probe", get(mcp_catalog::probe))
        .route(
            "/api/mcp/catalog/install",
            post(mcp_catalog::install),
        )
        .route(
            "/api/mcp/servers/{id}",
            put(mcp::update_server).delete(mcp::delete_server),
        )
        // Automation tasks: scheduled agent prompts (CRUD + manual trigger).
        .route(
            "/api/automation/tasks",
            get(automation::list_tasks).post(automation::create_task),
        )
        .route(
            "/api/automation/tasks/{id}",
            put(automation::update_task).delete(automation::delete_task),
        )
        .route(
            "/api/automation/tasks/{id}/trigger",
            post(automation::trigger_task),
        )
        .route("/api/automation/reset", post(automation::reset_to_official))
        // Workflows: 对话里跑通后由 `workflow` 工具保存的再跑流程。
        // 运行走 agent_service.send_message()（铁律 DD），会话 id =
        // workflow:{id}，历史在对话列表可见。
        .route(
            "/api/workflows",
            get(workflows::list_workflows).post(workflows::upsert_workflow),
        )
        .route(
            "/api/workflows/{id}",
            delete(workflows::remove_workflow).post(workflows::run_workflow),
        )
        // Plugin inspection: which official-manifest plugins are wired
        // into the in-process ToolRegistry. Drives the "已挂接 N 工具"
        // badges in the desktop PluginsView.
        .route(
            "/api/dashboard/plugin-inspect",
            get(plugins_inspect::inspect_all_plugins),
        )
        .route(
            "/api/dashboard/plugin-inspect/{id}",
            get(plugins_inspect::inspect_plugin),
        )
        // Custom LLM providers
        .route("/api/custom-providers", get(custom_providers::list_custom_providers).post(custom_providers::create_custom_provider))
        .route("/api/custom-providers/{id}", delete(custom_providers::delete_custom_provider))
        .route("/api/custom-providers/{id}/fetch-models", post(custom_providers::fetch_models))
        .route("/api/custom-providers/{id}/test-connection", post(custom_providers::test_connection))
        .route("/api/custom-providers/{id}/model-contexts", put(custom_providers::update_model_contexts))
        // 未落盘配置的连通性测试（添加模型弹窗"填完就测"）
        .route("/api/providers/test", post(custom_providers::test_inline))
        // Persona management: SOUL / system prompt / personalities / memory.
        // See `dashboard/persona.rs` for the file layout and the agent-loop
        // code that consumes these files.
        .route("/api/persona/soul", get(persona::get_soul).put(persona::put_soul))
        .route(
            "/api/persona/memory",
            get(persona::get_memory).put(persona::put_memory),
        )
        .route(
            "/api/persona/user",
            get(persona::get_user_profile).put(persona::put_user_profile),
        )
        .route(
            "/api/persona/personalities",
            get(persona::list_personalities),
        )
        .route(
            "/api/persona/personalities/{name}",
            get(persona::get_personality)
                .put(persona::put_personality)
                .delete(persona::delete_personality),
        )
        .route(
            "/api/persona/overrides",
            get(persona::get_overrides).put(persona::put_overrides),
        )
        // §2 Polish — doc rewrite endpoints used by the desktop
        // "润色" / "改写" affordances. `polish::post_polish_chat`
        // rebuilds a natural-language message, `polish::post_polish_automation`
        // rewrites a draft into a structured automation PROMPT.
        // Each handler binds to a single template constant from
        // `polish.rs`; the front-end sends `{draft, ctx}` and gets
        // back `{ok, text, error?}` so it can keep its existing
        // whitespace-only fallback if the backend is unreachable.
        .route("/api/polish/chat", post(polish::post_polish_chat))
        .route(
            "/api/polish/automation",
            post(polish::post_polish_automation),
        )
        // Custom agent (persona) persistence — read/write JSON files
        // under `$HERMIX_HOME/agents/*.json`. The front-end `AgentsView`
        // calls these endpoints instead of (or alongside) localStorage
        // so agents survive across browser caches.
        .route(
            "/api/agents/custom",
            get(agents::list_agents).post(agents::save_agent),
        )
        .route(
            "/api/agents/custom/{id}",
            delete(agents::delete_agent),
        )
        // CUA trace panel backend — Stage 4.1. Reads the
        // JSONL action log that `hermix-tools/tools/cua.rs`
        // appends to `<HERMIX_HOME>/cua-trace.jsonl` and returns
        // rows matching the requested session. The front-end trace
        // panel (Stage 4.2) polls this every few seconds and
        // renders an action timeline. Until the React component
        // lands, the endpoint is reachable from `curl` for
        // debugging.
        .route("/api/cua/trace/{session_id}", get(cua::get_cua_trace))
        // Custom apps — user-facing desktop applications
        .route(
            "/api/custom-apps",
            get(custom_apps::list_apps).post(custom_apps::create_app),
        )
        .route(
            "/api/custom-apps/{id}",
            get(custom_apps::get_app)
                .put(custom_apps::update_app)
                .delete(custom_apps::delete_app),
        )
        .route("/api/custom-apps/{id}/package", post(custom_apps::package_app))
        .route("/api/custom-apps/{id}/run", post(custom_apps::run_app))
        .route("/api/custom-apps/{id}/icon", get(custom_apps::get_app_icon))
}
