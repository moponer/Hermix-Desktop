# Decision Note: 动态工作流引擎（脚本运行时）

Status: proposed — 已定方向：工作流 = 可执行脚本 + 沙箱运行时。本篇给出运行时选型、脚本语言、沙箱权限模型与里程碑；待评审后开工 M1。

## 背景事实（读源码所得）

ZCode 的「工作流」是 agent 内核级能力：工作流 = **TypeScript 脚本**，跑在随包分发的 V8/Node 运行时里（其安装目录的 `v8_context_snapshot.bin` 为佐证）。工具族 `create-workflow / save-workflow / list-saved-workflows / get-workflow-run / resume-workflow-run / resolve-workflow-question` + `apps/zcode-cli/.../dynamic-workflow-*`（run 派发/生命周期/日志/重放/产物/血缘）。脚本语言提供**确定性控制流**（分支/循环/并行/等人工答复），agent 是脚本内被调度的组件。

Hermix 现状（2026-10-04）：工作流 = 保存的 prompt + `{{参数}}` 占位符 + 再跑一次（`360b7028`），无脚本层——不是工作流引擎。

## Decision

**方向已定：脚本运行时。** 不再以"无运行时阶段协议"作为主路径（那条只作为 Alternatives 里的退路记录）。

### 运行时选型

按"嵌入成本 / TS 支持 / 攻击面 / 构建时长"四维评估（Hermix 是 Rust + Tauri 桌面应用，构建在 Windows MSVC 上进行，V8 级依赖的编译时长是硬约束）：

| 候选 | 体积 | TS | 攻击面 | 构建时长 | 结论 |
|---|---|---|---|---|---|
| **boa**（纯 Rust JS 引擎） | 小（~几 MB） | 否（JS + esbuild 预编译） | 极小（无内建 net/fs） | 中 | **选它** |
| rquickjs（QuickJS 绑定） | 极小（~1 MB） | 否 | 极小 | 小 | 备选（JS 语义兼容性略好于 boa 的部分边角） |
| deno_core | 大（数十 MB） | 原生 | 大（Deno API 带 net/fs） | 极长（V8） | 否决：体积 + 构建时长 + 攻击面三重不划算 |
| 随包 node/v8（ZCode 路线） | +40 MB 级 | 原生 | 中（需进程隔离） | 长 | 否决：桌面端要背一个 Node 运行时与升级链 |

**选定 boa**：纯 Rust 依赖、无内建 IO（沙箱天然从"没有"开始加，而不是从"都有"开始删）、构建时长可控。TS 支持走 `esbuild` 预编译（保存工作流时编译，或前端编辑器保存前编译），运行时只认 JavaScript。

### 脚本语言与 API 面

- 语言：JavaScript（TS 子集经 esbuild 降级后运行）；类型声明随仓库分发。
- 运行时 API（最小集，逐里程碑加）：`hermes.ask(prompt)` 发起一轮对话、`hermes.tool(name, args)` 白名单工具调用、`hermes.state` 工作流内状态（可序列化）、`hermes.askUser(question)` 挂起等人工答复、`hermes.log(event)` 写 run 日志。
- 明确**不提供**：任意 fs（文件访问一律经 `hermes.tool("read_file"/"write_file", …)` 走现有 ToolRegistry 与审批引擎）、任意网络（http 工具同样走工具白名单）、require/import 任意模块（仅白名单内建模块）。脚本拿不到宿主内存与文件系统句柄。

### 权限与沙箱

- 工具白名单：工作流声明可调用的工具子集，运行时校验；越权调用直接抛错并记入 run 日志。
- 目录范围：文件类工具受现有 `permission_profile` / approval 引擎约束，脚本不能绕过审批（对齐"铁律 DD"：自动化与工作流与对话同路径、同审批）。
- 资源上限：单次运行墙钟超时 + 步数上限 + 内存上限，超限记 `failed` 并保留日志。
- 逃生口为零：runtime 层不暴露 IO，工具调用是唯一出口，而工具层已有审批与签名校验。

### 运行记录与重放

- 一次运行 = 一条 `workflow_runs` 记录（状态机同自动化：`claimed → running → succeeded/failed/stopped`）+ 脚本产物/日志行。
- 日志/血缘：阶段与每次 `hermes.tool` 调用进 run 日志；`hermes.state` 快照支持断点续跑。
- 结果默认落在 `workflow:{id}` 会话（会话即日志载体），与定时任务共用会话持久化。

### 里程碑

- **M1 骨架**：boa 依赖 + 最小 runtime 封装（执行一个 hello 脚本、超时/步数/内存上限、拿不到 IO 的自检测试）+ `workflow_runs` 表。
- **M2 工具桥**：`hermes.tool` 白名单校验 → 现有 ToolRegistry 派发（含审批）；`hermes.ask` 接现有 agent loop（与 send_message 同路径）。
- **M3 状态与重放**：`hermes.state` 快照 / 断点续跑 / run 日志与血缘查询。
- **M4 前端**：工作流脚本编辑器（TS 语法高亮 + esbuild 预编译错误定位）、参数声明与填参再跑、run 日志/重放入口（并入已落地的工作流 tab）。
- **M5 人工回路**：`hermes.askUser` 挂起 → 复用现有 clarify 交互。

## Alternatives considered

- **无运行时阶段协议**（prompt 里写阶段清单、agent 自行推进） — 记录在案但**已被否决**：阶段推进由 LLM 判定，不具备脚本级确定性，且与"工作流"的语义名不副实。
- **移植 ZCode 的 TS 引擎 / 随包 node** — 否决：随包 V8 的分发/升级链与构建时长在桌面端不可接受。
- **deno_core 求 TS 原生** — 否决：攻击面（自带 net/fs）与构建时长否决。
- **让模型写 Rust 插件** — 否决：需要编译链，交互式场景不可用。

## Consequences

- **新信任面**：脚本沙箱是本项目新增的最大攻击面——M1 的验收标准包含"脚本无法触达 IO/网络/未授权工具"的自检测试。
- 新增依赖（boa，可能含 esbuild）；构建时长上升，M1 首次编译偏久。
- 脚本版本与兼容：工作流保存时固化编译产物哈希，日志记录脚本版本，保证重放可复现。
- 与既有工作流（保存的 prompt + 填参重跑）并存期：旧条目按 prompt 型运行，新条目按脚本型运行，按条目类型分派。
