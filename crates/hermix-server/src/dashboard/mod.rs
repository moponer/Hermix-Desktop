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
pub mod workspace_templates;
pub mod consciousness;
pub mod env;
pub mod logs;
pub mod mcp;
pub mod migration;
pub mod persona;
pub mod plugins_inspect;
pub mod polish;
pub mod researcher;
pub mod skills;
pub mod status;
pub mod stubs;
pub mod transcribe;
pub mod groupchat_api;
pub mod groupchat_dispatcher;
pub mod groupchat_sqlite;
pub mod types;
pub mod workspaces;

// Re-export shared types used by multiple modules
pub use types::*;
use axum::routing::{delete, get, post, put, patch};
use axum::Router;

use crate::HttpServerState;

/// Build the dashboard API router.
/// All routes are prefixed with `/api/` and share the same `HttpServerState`.
pub fn router() -> Router<HttpServerState> {
    Router::new()
        // Status
        .route("/api/status", get(status::get_status))
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
        // Logs
        .route("/api/logs", get(logs::get_logs))
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
        .route("/api/dashboard/themes", get(stubs::get_themes))
        .route("/api/dashboard/theme", put(stubs::set_theme))
        // MCP server CRUD (config layer; actual spawn handled by hermes-mcp)
        .route(
            "/api/mcp/servers",
            get(mcp::list_servers).post(mcp::create_server),
        )
        .route("/api/mcp/servers/reset", post(mcp::reset_to_official))
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
        //Migration .
        .route("/api/migration/probe", get(migration::probe_migration))
        .route("/api/migration/apply", post(migration::apply_migration))
        .route("/api/migration/skip", post(migration::skip_migration))
        .route("/api/migration/reset", post(migration::reset_migration))
        // Workspaces — schema-driven workbenches (registry + definition).
        .route(
            "/api/workspaces",
            get(workspaces::list_workspaces).post(workspaces::create_workspace),
        )
        .route(
            "/api/workspaces/{id}",
            get(workspaces::get_workspace)
                .put(workspaces::rename_workspace)
                .delete(workspaces::delete_workspace),
        )
        .route(
            "/api/workspaces/{id}/schema",
            put(workspaces::update_workspace_schema),
        )
        // 布局专用端点：拖拽保存只动 layout 层，避免整 schema 快照回写
        // 覆盖 LLM 并发修改（丢失更新）
        .route(
            "/api/workspaces/{id}/layout",
            put(workspaces::update_workspace_layout),
        )
        // M5 协作分发：.hwx 导出 / 导入（JSON bundle，防脏包校验）
        .route(
            "/api/workspaces/{id}/export",
            get(workspaces::export_workspace),
        )
        .route(
            "/api/workspaces/import",
            post(workspaces::import_workspace),
        )
        // M6 模板：预置台账模板列表 + 一键建工作台
        .route(
            "/api/workspace-templates",
            get(workspace_templates::list_templates),
        )
        .route(
            "/api/workspace-templates/{id}/create",
            post(workspace_templates::create_from_template),
        )
        // 数据端点：打通 LocalSqliteStore（表名/列名过 schema 白名单）
        .route(
            "/api/workspaces/{id}/tables/{table}/query",
            post(workspaces::query_table),
        )
        .route(
            "/api/workspaces/{id}/tables/{table}/insert",
            post(workspaces::insert_rows),
        )
        .route(
            "/api/workspaces/{id}/tables/{table}/update",
            post(workspaces::update_row),
        )
        .route(
            "/api/workspaces/{id}/tables/{table}/delete",
            post(workspaces::delete_row),
        )
        .route(
            "/api/workspaces/{id}/tables/{table}/count",
            post(workspaces::count_rows),
        )
        .route("/ws/consciousness", get(consciousness::ws_consciousness))
        // GroupChat rooms
        .route(
            "/api/groupchat/rooms",
            get(groupchat_api::list_rooms).post(groupchat_api::create_room),
        )
        .route(
            "/api/groupchat/rooms/{room_id}",
            get(groupchat_api::get_room).delete(groupchat_api::delete_room),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/agents",
            put(groupchat_api::update_agents),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/agents/{agent_id}",
            post(groupchat_api::invite_agent)
                .delete(groupchat_api::kick_agent)
                .patch(groupchat_api::update_member_model),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/agents/{agent_id}/display-name",
            patch(groupchat_api::update_member_display_name),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/members",
            get(groupchat_api::list_room_members),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/ranks",
            get(groupchat_api::list_room_ranks),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/leader",
            put(groupchat_api::set_leader),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/settings",
            get(groupchat_api::get_settings).put(groupchat_api::update_settings),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/message",
            post(groupchat_api::send_message),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/messages",
            get(groupchat_api::get_messages),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/approve",
            post(groupchat_api::approve),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/reject",
            post(groupchat_api::reject),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/rules",
            put(groupchat_api::update_rules),
        )
        // P2 — Announcement: publish (POST) replaces, DELETE clears.
        // Same path; different methods. POST also accepts empty body
        // to clear (sugar — DELETE is the canonical clear verb).
        .route(
            "/api/groupchat/rooms/{room_id}/announcement",
            post(groupchat_api::publish_announcement)
                .delete(groupchat_api::clear_announcement),
        )
        // P4 — room description (free-form text shown in chat header).
        // Same shape as announcement: PATCH publishes (or clears when the
        // body is empty), DELETE clears unconditionally.
        .route(
            "/api/groupchat/rooms/{room_id}/description",
            patch(groupchat_api::update_description)
                .delete(groupchat_api::clear_description),
        )
        // Chat mode toggle: "task_collab" vs "free_chat".
        .route(
            "/api/groupchat/rooms/{room_id}/chat-mode",
            patch(groupchat_api::update_chat_mode),
        )
        // P2 — Tasks: history list + active single-get.
        .route(
            "/api/groupchat/rooms/{room_id}/tasks",
            get(groupchat_api::list_room_tasks),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/tasks/active",
            get(groupchat_api::get_active_task),
        )
        // P2 — Subtask update: status flip + result text. PATCH is
        // idempotent — sending the same payload twice yields the
        // same row state.
        .route(
            "/api/groupchat/subtasks/{subtask_id}",
            patch(groupchat_api::update_subtask),
        )
        // P1.4 — vote +20/-20
        .route(
            "/api/groupchat/rooms/{room_id}/vote",
            post(groupchat_api::vote),
        )
        // P4 — dispute + overturn
        .route(
            "/api/groupchat/rooms/{room_id}/dispute",
            post(groupchat_api::dispute_violation),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/overturn",
            post(groupchat_api::overturn_violation),
        )
        // P2.2 — discussion status query + cancel
        .route(
            "/api/groupchat/rooms/{room_id}/discussion",
            get(groupchat_api::get_discussion_status),
        )
        .route(
            "/api/groupchat/rooms/{room_id}/discussion/cancel",
            post(groupchat_api::cancel_discussion),
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
