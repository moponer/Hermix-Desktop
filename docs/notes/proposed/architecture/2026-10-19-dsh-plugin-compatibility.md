# Decision Note: DeepSeek Harness 插件兼容层

Status: proposed — 调研完成；建议只做「纯宿主插件」的有界兼容，放弃「无缝安装全部 dsh 插件」

调研对象：`deepseek-harness` 0.2.1-rc.2（本地源码 `<refs>/ref-dsh`）、
真实三方插件 `Ebony-Vinyl/dsh-our-free-model` v2.5.3（本地副本 `<refs>/dsh-ofm`）。

## Decision

1. **不承诺「所有 dsh 插件无缝可用」**。dsh 插件不是声明式配置，而是直接运行在宿主进程内、
   依赖宿主服务语义的 ESM 代码；且带浏览器半身的插件绑定 dsh 自己的 Web 外壳。
   「全部」在物理上不成立，能成立的是**分类兼容**。
2. **建 Node 插件宿主（独立 sidecar），用真 `@deepseek-ai/cordis` 当容器，不自研 DI 内核**。
   dsh 的 `ctx.inject / effect / get / on / fiber` 语义由 cordis 提供（MIT，npm `@deepseek-ai/cordis@4.0.5`，
   解包约 239KB），我们只实现 **ctx 门面（服务层）**与 **Rust↔Node 的 JSON-RPC 桥**。
3. **兼容分四层，按价值排序落地**：
   - **L1 模型/供应商插件**（`ctx.llm.*`）：源码零改动 → hermix 模型选择器可用。首枚验收插件
     `dsh-our-free-model`。
   - **L1b 工具/命令插件**（`ctx.tools.register`、`ctx.commands`）：源码零改动 → 工具进 hermix
     `ToolRegistry`，命令进 slash 面板。
   - **L2 带 HTTP 路由的插件**（`ctx.webServer`）：hermix 反向代理 `/plugins/{id}/*` 到插件的回环端口；
     插件自带设置页仍不可见（无 client 半身）。
   - **L3 深水区**（`ctx.sessions / agents / sandbox / shell / terminals / subprocess` 的完整语义、
     dsh 自带 bundle/HMR/自升级）：**打桩降级，不保证可用**。
   - **L4 浏览器半身**（`window.__ModuleLoader__` + `ctx.slots` + dsh React 外壳 + `remote` 绑定）：
     **不做**。要支持只能把 dsh 的 Web UI 整页嵌进 WebView，属另一个产品决策。
4. **包格式复用 dsh**：安装产物 = 同一个 npm tarball / 目录，识别 `package.json` 的
   `dsh.bundle.patch`、`dsh.client`、`main`/`exports`；**不发明 hermix 专属插件包**。
   安装来源支持 dsh 自己的四种 spec：registry / 本地路径 / tarball / git。
5. **宿主隔离**：插件跑在独立 node 进程，不链接进 Rust 进程；工具注册与模型接入按 hermix 审批模型
   走（插件 = 任意代码执行，等同于 terminal 权限）。

## 调研事实

### 1. dsh 插件系统解剖（安装 → 装配 → 运行 → UI）

**包契约**（`<refs>/dsh-ofm\package.json`）：

```json
{
  "type": "module",
  "main": "./index.js",
  "exports": { ".": "./index.js", "./client": "./client.js" },
  "dsh": {
    "bundle": { "patch": "./cordis.patch.yml" },
    "client": { "platform": "web", "immediately": true, "inject": [] }
  },
  "peerDependencies": {
    "@deepseek-ai/cordis": "^4.0.2",
    "@deepseek-ai/dsh-llm": "^0.1.2-rc.1 || ^0.1.7-rc.2 || ^0.2.0-rc.1",
    "@deepseek-ai/dsh-commands": "...",
    "@deepseek-ai/dsh-credentials": "...",
    "@deepseek-ai/schemastery": "^3.18.4"
  }
}
```

**装配链**（`packages/boot/app-boot/README.md:48-69`）：profile 位于 `$DSH_HOME/profiles/<name>`，
由 `package.json.dependencies` + `dsh.profile.bundles` + `cordis.patch.yml` 组成；
每个 bundle 通过 `dsh.bundle.patch` 贡献一层 patch；boot 时按
环境层 → bundle 层（按声明顺序）→ profile 用户 patch → home 级 patch 合并成一张 Cordis 配置树，
再由 Loader 逐个 import + 激活。`!!js` 表达式在 boot 时求值。

**插件本体契约**：ESM 模块导出 `name`、`inject`（声明依赖的服务名）、`apply(ctx, config)`。
`ctx` 是唯一能力入口：服务查找（`get`/`inject`/`provide`）、生命周期（`effect`/`fiber`）、
事件（`on`/`emit`/`waterfall`）、日志，以及各服务（`llm`、`tools`、`commands`、`sessions`、
`agents`、`fs`、`subprocess`、`shell`、`sandboxPolicy`、`credentials`、`settings`、`webServer`、
`skills`、`jobs`、`subagents`、`sessionProjections`、`systemPrompt`）。

**版本准入**（同上 README:52）：优先 `engines.dsh`，否则逐个校验 `peerDependencies` 里
`@deepseek-ai/dsh*` 的 range；profile 的 `compatibility.json` 可按精确版本豁免。

**安装器**（`packages/boot/plugin-manager/src/operations.ts`）：调 **pnpm** 装依赖、写 profile
`package.json` + `dsh.profile.bundles`，再发布 runtime resolution；也支持 `path`/`tarball`/`git`
（`src/install-spec.ts`）。即安装环节硬依赖 Node + 包管理器。

**两层半身**：宿主半身 = `main`；浏览器半身 = `dsh.client` + `exports["./client"]`，
由 dsh Web 的 ModuleLoader 加载，拿到 `React/console/styles/host`，通过 `ctx.slots.inject(...)`
往 `settings.section`、`sidebar` 等槽位挂 React 组件（`<refs>/dsh-ofm\client.js:31,6521`）。

### 2. 真实插件 `dsh-our-free-model` 拆解

- 规模：`index.js` 2366 行（宿主半身）+ `client.js` 6579 行（浏览器半身）+ `src/` 约 40 个模块。
- 安装：`dsh plugin --profile web add <绝对路径>`；README 明令不要走 npm（同名包被第三方改过），
  只认 Release 包/源码目录；自带 Ed25519 签名的应用内升级与热重载。
- 宿主半身依赖面（全仓库静态统计）：`ctx.logger` 191、`ctx.credentials` 171、`ctx.effect` 23、
  `ctx.llm` 19、`ctx.get` 17、`ctx.inject` 7、`ctx.fiber` 7、`ctx.emit` 4、`ctx.locale` 6。
  **外部 import 只有 `@deepseek-ai/dsh-llm`（惰性、失败降级为字面量 UA）**，其余全是相对路径 + `node:*`。
- 注册方式：`export const inject = ['llm']`（`index.js:107`），
  `ctx.llm.registerAdapter([route...], adapter)`（:423）、可选
  `ctx.llm.registerConfigurableProviders`、`registerModelDiscovery`；
  `ctx.inject(['credentials','commands','llm'])`（:1017）、`ctx.inject(['webServer'])`（:1394）。
- 适配器契约（`packages/llm/llm/src/index.ts:208` 抽象类、`:401 registerAdapter`）：
  `providerInfo / providerRetryPolicy / listModels / resolveModel / prepareCall / stream`，
  流式输出统一为 `StreamChunk`（`types.ts:470`）：
  `block-start | text-delta | reasoning-delta | tool-call-delta | block-end | usage | finish`。
- 结论：**这是「纯 ctx 消费者」型插件**，没有碰 sessions/agents/sandbox。它的模型能力可以整体
  搬进 hermix，只缺设置页（client 半身）。

### 3. 生态规模与类别（页面口径，未逐仓核验）

- GitHub topic `dsh-plugin` 报 21,030 个仓库（含大量「蹭标签」的成熟项目与教程），
  社区口径「插件仓库 1.4K–2K」。头部仓库集中在：编码/逆向 agent、记忆层（EverOS/Memos/OpenViking）、
  UI/桌面外壳（两款 dsh-desktop、侧边栏）、集成（PicGo/NocoBase）。**模型 provider 类很少**。
- 含义：生态里占比最大的不是「接模型」，而是「加工具 / 加记忆 / 改宿主 UI」。
  L1b（工具）比 L1（模型）价值更大，但 L3（记忆/会话/沙箱）恰好最难兼容。

### 4. hermix 现状与落点

- LLM provider 是**静态 match**：`crates/hermix-agent/src/agent_builder.rs:434`（"openai"/"anthropic"/…），
  运行时另有一份 `runtime_provider.rs:242`；`LlmProviderConfig` 已预留 `command`/`args` 字段，
  `local_agent_service.rs:339` 有 `ProviderFactory` 注入点（仅测试使用）。
- 工具注册是**运行时可加**的：`ToolRegistry::register`（`crates/hermix-tools/src/registry.rs:332`），
  MCP 已用 `hermix-mcp` 的 stdio 外部进程把 `server__tool` 动态注册进来——**这是既有先例**。
- 已存在但未接线的 `crates/hermix-agent/src/plugins.rs`：`Plugin` trait + `PluginManifest` +
  `HookType` 十个钩子 + 权限枚举；`agent_loop/config.rs:367` 恒为 `plugin_manager: None`，
  **没有任何磁盘加载器**。即「hermix 已有插件词汇，没有插件运行时」。
- 前端：无 router（`app/routes.tsx` 的 `renderRoutes(page)`），设置页按 category 分发
  （`SettingsShell.tsx`，已有 `plugins`/`skills`），`config/PluginsTab.tsx` 已能列 discovered manifests。
  **没有 slot 机制**，与 dsh client 半身不兼容。

## 兼容矩阵

| 层 | 插件类型 | 需要实现的 ctx 门面 | 源码改动 | 结论 |
|---|---|---|---|---|
| L0 | 安装/启停/卸载 | 包解析、依赖解析、落盘、manifest 读取 | 无 | 可 100% 复用 dsh 包格式 |
| L1 | 模型/供应商（OFM、custom provider） | `llm.registerAdapter/registerConfigurableProviders/registerModelDiscovery`、`credentials`、`logger`、`effect/inject/get/on/emit/fiber` | 无 | **可行**，首枚验收目标 |
| L1b | 工具/命令（defineTool、slash command） | `tools.register/restrict/guard`、`commands` | 无 | **可行**，需把 hermix 工具 schema 双向映射 |
| L2 | 带 webServer 路由的插件 | `webServer` 反向代理 | 无 | 可用但无 UI；配置需手改 JSON |
| L3 | 记忆/会话/多 agent/沙箱/终端类 | `sessions/agents/sandboxPolicy/shell/terminals/subprocess` 完整语义 | 无（但需大量桥接） | **降级/不保证**，逐个适配 |
| L4 | 带 client 半身的 UI 插件 | dsh ModuleLoader + slots + dsh React 外壳 + remote | 无 | **不支持**（需整页嵌 dsh Web） |
| L5 | dsh 自身 bundle/HMR/自升级 | dsh loader 语义 | 无 | **不支持**（用 hermix 的机制替代） |

## Alternatives considered

- **A. 直接嵌入整个 dsh 运行时**（Node + dsh web + 我们只做壳）：
  兼容性最高，但等于把 agent loop 换成 dsh，与 2026-10-17「不换底座，只参考」的决策冲突；
  且体积/启动/双份会话状态不可接受。**否决**。
- **B. 只把插件当 MCP server 包装**：dsh 插件不是 MCP server，没有协议入口；
  要包装仍需先跑起宿主，反而多一层。**否决**（但不排除插件内部本身用 MCP）。
- **C. 自造 hermix 插件 API，不做 dsh 兼容**：最省事、最干净，但拿不到 2K 生态，
  与本次诉求相反。**不作为主路线**；只保留「hermix 原生插件包 = dsh 包格式 + 扩展字段」的兼容写法。
- **D. 把插件编译进 Rust / 用 QuickJS 内嵌执行**：Node API（`node:fs/http/crypto/net`）与 ESM
  解析器缺失，绝大多数插件直接崩；QuickJS 还要重做 Node 兼容层。**否决**。
- **E. 重写 dsh 的 cordis 容器**：cordis 已 MIT 且体积小，重写只会引入语义漂移。**否决**。

## 实施路径

**Phase 0（spike，3–5 天，先证伪）**
1. 新增 `crates/hermix-plugin-host`：spawn `node host.mjs`，stdio 按行 JSON-RPC（复用 hermix 现有 stdio 风格）。
2. `host.mjs` 依赖真 cordis + 我们的 ctx 门面：`logger/llm/credentials/commands/settings(最小)/effect/inject/get/on/emit`。
3. 把 `dsh-our-free-model` 的 tarball 解到 `hermix_home/plugins/dsh/`，`ctx.plugin(import(entry))`。
4. Rust 侧：
   - `providers.list` → 模型选择器出现 OFM 的两个分组；
   - `chat.stream` → 把 hermix 消息映射成 dsh `GenerateOptions`（`RequestMessage/ContentBlock/ToolSchema`），
     把 `StreamChunk` 映射回 hermix `StreamChunk`（文本/思考/工具调用/usage/finish/abort）；
   - 工具调用透传 + 取消。
5. 验收：hermix 里选 OFM 的模型，跑一次带工具调用的流式对话成功；再装第二个「只注册工具」的插件，
   其工具能被 agent 调用。

**Phase 1（MVP，2–3 周）**：安装/卸载/启停、来源四形态、依赖解析（先捆 pnpm 单文件 CLI，
避免要求用户装 pnpm）、插件列表页（复用 `PluginsTab`）、日志、崩溃隔离与自动重启、版本准入
（记录 `HERMIX_DSH_COMPAT = 0.2.x`，peer range 只警告不阻断）。

**Phase 2（1–2 周）**：`ctx.tools.register` 全链路 + 审批/风险分级；`ctx.commands` 进 slash；
`webServer` 反代；通用配置表单（schemastery → JSON Schema → 表单），补齐无 UI 插件的配置能力。

**Phase 3（可选，需单独决策）**：dsh Web 面板（WebView）以承载 L4；插件市场索引。

## 风险（对抗性评审）

- **安全**：插件是任意 Node 代码，可读文件、发网络请求、起子进程。进程隔离只防崩不防恶意。
  必须：安装前展示来源与权限、显式审批、默认禁用自动更新、日志留痕；不引入插件市场一键装。
- **供应链**：OFM README 自己就记录了 npm 同名包被篡改。安装源必须支持「本地目录/tarball +
  哈希展示」，并在 UI 里标明来源。
- **协议漂移**：dsh 0.2.x 仍是 rc，`StreamChunk`/`LlmAdapter` 会变。门面要薄，映射集中在一个文件，
  加契约测试（用 OFM 当 golden fixture）。
- **Node 体积**：需捆 Node 22.19+（约 50–60MB）。项目已捆 189MB 的 office 引擎，可接受；
  仍应优先探测系统 Node ≥22，缺失再提示下载。
- **伪兼容**：L3 打桩会让插件「装上但半死」。UI 必须显示兼容等级与缺失服务，不允许静默降级。
- **生态噪音**：topic 下大量仓库不是真插件（教程、桌面壳、蹭标签）。不做市场前不要承诺「全部可装」。
- **维护面**：这是本项目第一次引入 JS 运行时与包管理器，长期成本高于同等 Rust 功能。

## Consequences

- 得到：一批「模型/工具/命令」类 dsh 插件可不改源码装进 hermix；与 dsh 生态同包格式，
  未来作者可一份包同时上架两边。
- 放弃：client 半身 UI、dsh 深水区服务语义、dsh 的 HMR/自升级；「全部插件无缝」的表述。
- 新增常驻面：Node 运行时 + 包管理器 + 插件进程管理 + JSON-RPC 协议 + 两个方向的消息映射，
  属长期维护负担，必须靠契约测试锁住。

## Open questions

1. Node 运行时是「随包分发」还是「首次使用下载」？（建议随包，Windows-only 场景简单）
2. L1 是否只支持「无 client 半身」的插件？带 UI 的（OFM）是否接受「能跑但不能配」？
3. 插件工具是否默认进主 agent 工具面，还是默认禁用、按会话开启？
4. 是否要把现有 `crates/hermix-agent/src/plugins.rs`（未接线的 Rust 插件词汇）并入本方案，
   避免第五套扩展机制？
