//! Dashboard management API endpoints.
//!
//! These endpoints mirror the Python `hermix_cli/web_server.py` API surface,
//! providing the `/api/*` routes consumed by the React web dashboard.

pub mod agents;
pub mod custom_apps;
pub mod custom_providers;
pub mod analytics;
pub mod automation;
pub mod config;
pub mod env;
pub mod vault_api;
pub mod workflows;
pub mod mcp;
pub mod mcp_catalog;
pub mod mcp_scan;
pub mod memory_inspect;
pub mod persona;
pub mod plugins_inspect;
pub mod polish;
pub mod skills;
pub mod storage_admin;
pub mod stubs;
pub mod transcribe;
pub mod types;
pub mod voice_stt;

// Re-export shared types used by multiple modules
pub use types::*;
use axum::routing::{delete, get, post, put};
use axum::Router;

use crate::HttpServerState;
use hermix_core::origin::Capability;

/// §5.5 第 5 层（端点级）：敏感/写类端点统一要求显式能力位。
///
/// `request_guard` 只保证「已认证且非未配对」，能力位判定此前靠每个 handler
/// 自己调——127 条路由里只有 3 处调了，于是默认 `scopes=["chat"]` 的配对设备
/// 能直接读 env/vault 明文、改工具集、拉起自定义应用。这里用一层中间件把
/// 「哪些路径属于哪个能力位」变成路由表上的声明，handler 不用再各自记得。
async fn require_capability(
    axum::extract::State(cap): axum::extract::State<Capability>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let origin = req.extensions().get::<hermix_core::Origin>().cloned();
    let scopes = req
        .extensions()
        .get::<Option<std::collections::HashSet<Capability>>>()
        .cloned()
        .flatten();
    if let Some(origin) = origin {
        if let Err((status, message)) =
            crate::routes::check_capability(cap, &origin, scopes.as_ref())
        {
            return (status, message).into_response();
        }
    }
    next.run(req).await
}

/// 给一组路由挂上能力位门槛（`Capability` 作为中间件 state）。
fn guarded(cap: Capability, routes: Router<HttpServerState>) -> Router<HttpServerState> {
    routes.layer(axum::middleware::from_fn_with_state(
        cap,
        require_capability,
    ))
}

/// 仅本机（`Origin::Local`）可用的端点：凭据明文、配置写、应用执行。
///
/// 选 `ConfigMutation` / `WorkbenchAccess` 而不是新枚举：两者在
/// `Origin::may` 的 PairedDevice 分支都是 `Denied`，语义正好是「本机专属」；
/// 桌面端（Local）在矩阵里全 `Allowed`，行为零变化。
fn local_only_routes() -> Router<HttpServerState> {
    guarded(
        Capability::ConfigMutation,
        Router::new()
            // 单条环境变量明文（此前漏注册过一次，导致模型列表静默为空）
            .route("/api/env/reveal", post(env::reveal_env_var))
            // 凭据保险库：列表永不含密码，但写入/明文/删除都只该由本机发起
            .route(
                "/api/vault/entries",
                get(vault_api::list_entries).post(vault_api::create_entry),
            )
            .route(
                "/api/vault/entries/{id}",
                put(vault_api::update_entry).delete(vault_api::delete_entry),
            )
            .route(
                "/api/vault/entries/{id}/reveal",
                post(vault_api::reveal_entry),
            )
            // 技能/工具集开关会改变模型可见能力面
            .route("/api/skills/toggle", put(skills::toggle_skill))
            .route(
                "/api/tools/toolsets/enabled",
                put(skills::set_enabled_toolsets),
            )
            // MCP server 写：新增的 server 会被 spawn 成本地进程
            .route("/api/mcp/servers", post(mcp::create_server))
            .route("/api/mcp/servers/reset", post(mcp::reset_to_official))
            .route("/api/mcp/catalog/install", post(mcp_catalog::install))
            .route("/api/mcp/servers/{id}", put(mcp::update_server).delete(mcp::delete_server))
            // 自定义 agent 落盘（决定后续每一轮的系统提示与工具集）
            .route("/api/agents/custom", post(agents::save_agent))
            .route("/api/agents/custom/{id}", delete(agents::delete_agent))
            // 存储维护：体积事实 + 人工触发 VACUUM（唯一会缩文件的操作）
            .route("/api/storage/stats", get(storage_admin::stats))
            .route("/api/storage/vacuum", post(storage_admin::vacuum))
            // 诊断视图会回传内部表名与样本行（session_id 等），只给本机。
            .route("/api/analytics/debug", get(analytics::get_debug))
            // 自定义 provider 的写面：会带用户密钥发起出站请求。
            .route(
                "/api/custom-providers/{id}",
                delete(custom_providers::delete_custom_provider),
            )
            .route(
                "/api/custom-providers/{id}/fetch-models",
                post(custom_providers::fetch_models),
            )
            .route(
                "/api/custom-providers/{id}/test-connection",
                post(custom_providers::test_connection),
            )
            .route(
                "/api/custom-providers/{id}/model-contexts",
                put(custom_providers::update_model_contexts),
            )
            .route("/api/providers/test", post(custom_providers::test_inline))
            // 插件设置写入 / 重新扫描
            .route("/api/dashboard/plugins", put(stubs::set_plugins))
            .route("/api/dashboard/plugins/rescan", post(stubs::rescan_plugins))
            // config 写面：auto-correct 改 config.yaml；model/list 是持久化清单
            .route("/api/config/auto-correct", post(config::auto_correct_config))
            .route("/api/model/list", put(config::update_model_list))
            // 模型探测（带用户密钥出站）
            .route(
                "/api/models/context-windows",
                post(config::resolve_context_windows),
            )
            .route(
                "/api/models/refresh-context",
                post(config::refresh_model_context),
            )
            // 免费渠道：总开关、死模型上报、1-token 真实探测
            .route("/api/free-channel", put(config::put_free_channel))
            .route("/api/free-channel/dead-model", post(config::report_dead_model))
            .route("/api/free-channel/probe-model", post(config::probe_free_model))
            // 手机端配置写面（桌面 UI 写入，经 WS 密封帧下发给手机）
            .route("/api/voice/stt", put(voice_stt::put_voice_stt))
            .route("/api/mobile/model-push", put(config::put_mobile_model_push))
            // 自定义 provider 创建
            .route(
                "/api/custom-providers",
                post(custom_providers::create_custom_provider),
            )
            // 自动化的增/改/删/重置
            .route("/api/automation/tasks", post(automation::create_task))
            .route(
                "/api/automation/tasks/{id}",
                put(automation::update_task).delete(automation::delete_task),
            )
            .route("/api/automation/reset", post(automation::reset_to_official))
            // 工作流 upsert / 删除
            .route("/api/workflows", post(workflows::upsert_workflow))
            .route("/api/workflows/{id}", delete(workflows::remove_workflow))
            // Persona 写入：SOUL.md 决定 agent 的人格与边界，记忆/user/人格
            // 文件会进入每一轮的系统提示 —— 都只该由本机改。
            .route("/api/persona/soul", put(persona::put_soul))
            .route("/api/persona/memory", put(persona::put_memory))
            .route("/api/persona/user", put(persona::put_user_profile))
            .route("/api/persona/overrides", put(persona::put_overrides))
            .route(
                "/api/persona/personalities/{name}",
                put(persona::put_personality).delete(persona::delete_personality),
            ),
    )
    .merge(guarded(
        Capability::WorkbenchAccess,
        Router::new()
            // 打包 = 写磁盘 + 起进程；运行 = 直接执行用户 JS
            .route("/api/custom-apps/{id}/package", post(custom_apps::package_app))
            .route("/api/custom-apps/{id}/run", post(custom_apps::run_app)),
    ))
    .merge(guarded(
        // 手动触发自动化 = 让 agent 立刻跑一轮（会被计入 `dangeroustool`
        // 能力位：配对设备配了才放行，矩阵对未配者仍是 Denied）。
        Capability::DangerousTool,
        Router::new()
            .route(
                "/api/automation/tasks/{id}/trigger",
                post(automation::trigger_task),
            )
            // 运行工作流同样是一次完整 agent 回合。
            .route("/api/workflows/{id}", post(workflows::run_workflow)),
    ))
}

/// Build the dashboard API router.
/// All routes are prefixed with `/api/` and share the same `HttpServerState`.
pub fn router() -> Router<HttpServerState> {
    Router::new()
        .merge(local_only_routes())
        //Sessions — T2-A : moved to /v1/sessions in lib.rs.
        // The dashboard router no longer mounts any session routes; the
        // SPA uses `/v1/sessions/*` exclusively now.
        // Config
        .route(
            "/api/config",
            get(config::get_config).put(config::update_config),
        )
        // Auto-correct `config.model`（写 config.yaml）→ `local_only_routes()`。
        // 安全中心：自动备份目录统计。
        .route("/api/backup/status", get(config::get_backup_status))
        // Model info
        .route("/api/model/info", get(config::get_model_info))
        // Model selector list (durable, backend-persisted) — fixes the
        // switcher "forgetting" configured models when localStorage is wiped.
        .route("/api/model/list", get(config::get_model_list))
        // 手机端语音识别配置（桌面配置 → WS 密封帧下发手机，见 voice_stt.rs）
        .route("/api/voice/stt", get(voice_stt::get_voice_stt))
        // 手机端「模型配置推送」开关（**默认关闭**）：打开才把模型清单与
        // provider 凭据经密封帧推给手机（见 config::MobileModelPushConfig）。
        .route("/api/mobile/model-push", get(config::get_mobile_model_push))
        // Builtin provider metadata (id + env var) — single source of truth
        // in hermix-config; the frontend groups builtin/custom from here.
        .route("/api/providers/meta", get(config::get_provider_meta))
        // Authoritative context-window resolution (value + source tier), so
        // the settings UI stops mirroring the static catalog.
        // context-windows / refresh-context（出站请求）→ `local_only_routes()`。
        // 选中模型时拉取该模型的 provider 级真实窗口（endpoints），
        // 让卡片上的上下文从"聚合值"校准到"我们实际连的那家"。

        // Env
        .route(
            "/api/env",
            get(env::get_env_vars)
                .put(env::set_env_var)
                .delete(env::delete_env_var),
        )
        // Reveal / 写入类端点已移入 `local_only_routes()`（需能力位）。
        // Skills & Tools（只读面）
        .route("/api/skills", get(skills::list_skills))
        .route("/api/skills/{name}", get(skills::get_skill))
        .route("/api/tools/toolsets", get(skills::list_toolsets))
        // Vault 路由已移入 `local_only_routes()`（凭据明文只给本机）。
        // Analytics
        .route("/api/analytics/usage", get(analytics::get_usage))
        // `/api/analytics/debug` 已移入 `local_only_routes()`（回传内部样本行）。
        // Dashboard plugins（只读面；PUT 在 `local_only_routes()`）
        .route("/api/dashboard/plugins", get(stubs::get_plugins))
        // rescan（会写入种子文件）→ `local_only_routes()`。
        .route("/api/dashboard/plugin-manifests", get(stubs::get_plugin_manifests))
        // 移动端活动探测：桌面端拿它参与「保持唤醒」判定
        .route("/api/dashboard/mobile-activity", get(stubs::get_mobile_activity))
        // 桌面注意力上报：审批超时按「可送达时间」计费要靠它
        .route("/api/dashboard/presence", post(stubs::post_presence))
        // MCP 写面（create/update/delete/reset/install）在 `local_only_routes()`：
        // 新增 server 会被 spawn 成本地进程，属于配置写。
        // 目录是编译期常量，没有"源"可切，也没有可探测的网络端点。
        .route("/api/mcp/servers", get(mcp::list_servers))
        .route("/api/mcp/catalog", get(mcp_catalog::get_catalog))
        // Automation tasks: scheduled agent prompts (CRUD + manual trigger).
        .route("/api/automation/tasks", get(automation::list_tasks))
        // 自动化的创建/改/删/重置 → `local_only_routes()`。
        // `/api/automation/tasks/{id}/trigger` 已移入 `local_only_routes()`
        // 的 DangerousTool 分组。
        // 活跃 run 计数：桌面保活信号。后台任务没有 UI 正在看的会话，
        // 活动状态只能从 run 表读。
        .route(
            "/api/automation/active-count",
            get(automation::active_count),
        )
        // reset：把自动化任务重置回官方种子。桌面端暂无入口（2026-10-18
        // 审查确认）——同 mcp/servers/reset，保留待入口或排障。

        // Workflows: 对话里跑通后由 `workflow` 工具保存的再跑流程。
        // 运行走 agent_service.send_message()（铁律 DD），会话 id =
        // workflow:{id}，历史在对话列表可见。
        .route("/api/workflows", get(workflows::list_workflows))
        // workflows 的 upsert / 删除（ConfigMutation）与运行（DangerousTool）
        // → `local_only_routes()` 分组。
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
        // 自定义 provider 的**写面**已移入 `local_only_routes()`（ConfigMutation）。
        // 列表与创建合并注册在同一条路径上（axum 不允许同 path 分两次注册），
        // 故 GET/POST 保留在这里；创建本身也受 `/api/config` 的写权限约束。
        .route("/api/custom-providers", get(custom_providers::list_custom_providers))
        // 免费渠道死模型摘除（前端收到上游 404 / 401 时上报）
        // dead-model / probe-model / put → `local_only_routes()`。
        // 免费模型行内测试：1-token 真实推理探测（GET /models 测不出单模型死活）

        // 社区免费模型总开关：开=拉取社区清单，关=不拉取并移除已拉取的条目
        .route("/api/free-channel", get(config::get_free_channel))
        // Persona management: SOUL / system prompt / personalities / memory.
        // See `dashboard/persona.rs` for the file layout and the agent-loop
        // code that consumes these files.
        // 写面（PUT/DELETE）已移入 `local_only_routes()`——persona 文件会进入
        // 每一轮系统提示，只该由本机改；这里只保留读面，否则同 path 双重
        // 注册 PUT 会让 axum 启动即 panic（overlapping method route）。
        .route("/api/persona/soul", get(persona::get_soul))
        .route("/api/persona/memory", get(persona::get_memory))
        .route("/api/persona/user", get(persona::get_user_profile))
        .route(
            "/api/persona/personalities",
            get(persona::list_personalities),
        )
        .route(
            "/api/persona/personalities/{name}",
            get(persona::get_personality),
        )
        .route("/api/persona/overrides", get(persona::get_overrides))
        // Memory management ("记忆管理" settings page): graph view +
        // item detail / soft-delete / pin over memory.db, daily
        // memory and profile md entries. See `memory_inspect.rs`.
        .route(
            "/api/memory/graph",
            get(memory_inspect::get_memory_graph),
        )
        .route(
            "/api/memory/item",
            get(memory_inspect::get_memory_item).delete(memory_inspect::delete_memory_item),
        )
        .route("/api/memory/search", get(memory_inspect::search_memory))
        .route("/api/memory/daily", get(memory_inspect::get_daily_memory))
        .route("/api/memory/pin", post(memory_inspect::pin_memory))
        // §2 Polish — doc rewrite endpoints used by the desktop
        // "改写" affordance (automation PROMPT) and "增强" (prompt
        // enhancement). Each handler binds to a single template
        // constant from `polish.rs`; the front-end sends
        // `{draft, ctx}` / `{draft, recentTurns, hasAttachments}` and
        // gets back `{ok, text, error?}`.
        .route(
            "/api/polish/automation",
            post(polish::post_polish_automation),
        )
        // 增强提示词：把一句模糊需求扩写成六段结构的提示词。
        // 与「改写」端点的区别是**扩写**而非同义替换，场景规格表
        // 内置在 ENHANCE_PROMPT 里，前端不传场景。
        .route("/api/polish/enhance", post(polish::post_polish_enhance))
        // Custom agent (persona) persistence — read/write JSON files
        // under `$HERMIX_HOME/agents/*.json`. The front-end `AgentsView`
        // calls these endpoints instead of (or alongside) localStorage
        // so agents survive across browser caches.
        .route("/api/agents/custom", get(agents::list_agents))
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
        // package / run 在 `local_only_routes()`（起进程 + 执行用户 JS）
        .route("/api/custom-apps/{id}/icon", get(custom_apps::get_app_icon))
        // 日志查看端点（/api/logs/{files,read,disk_usage}）2026-10-18 移除：
        // 桌面端日志查看走 Tauri command（list_backend_log_files /
        // read_backend_log_tail / open_log_directory，desktopBridge.ts），
        // HTTP 面零消费，是同一能力的两份实现。
}
