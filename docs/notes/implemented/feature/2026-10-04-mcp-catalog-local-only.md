# Decision Note: MCP 目录改纯本地内置（取消远程拉取，32 → 21 条）

Status: implemented — 远程取数链路与三个源已删除；目录收窄并补齐准入不变式；env 输入 UI 补上，21 条里 13 条需凭据的条目从"看得见装不上"变为可装

## 触发

上一份 `rejected/feature/2026-10-04-self-hosted-extension-marketplace.md` 否决了自建市场服务后，决策落到「纯本地，内置就是内置」。本篇记录据此对 MCP 目录做的收窄。

## Decision

**目录改为编译期打包的常量，取消一切远程取数；并把 32 条精选收窄到 21 条。**

### 一、取消远程拉取

删除的东西比留下的多，这是本次的主体：

| 删除 | 位置 |
|---|---|
| `Official` / `SelfHosted` 两个数据源（只留内置） | `CatalogSource` 枚举 |
| `fetch_remote` + 游标分页（`limit=100` × 5 页） | 同上 |
| 磁盘缓存 `mcp_catalog.json` + 24h TTL + `?refresh=1` | `read_cache` / `write_cache` / `CACHE_TTL_SECS` |
| 远端归一化层 `Raw*` / `normalize` / `is_latest` / `env_placeholders` | 同上 |
| 源偏好持久化 `mcp_registry.json` | `RegistryPrefs` 系列 |
| 三个端点：`/api/mcp/catalog/sources`（GET+PUT）、`/probe` | `dashboard/mod.rs` 路由 |
| 数据源选择条组件（源下拉 + 自建 URL 输入 + 测试连接） | `CatalogSourceBar.tsx` 整体删除 |

`get_catalog` 从 90 行三层兜底收敛为返回 `builtin_items()`。目录因此从「随网络变化的列表」变成**确定性函数**——同一版本永远给出同一份列表，`from_cache` / `stale_reason` 两个字段随之失去意义，从响应里删掉。

**顺带解开的死结**：`install` 端点原先从**磁盘缓存**查条目（`read_cache`），于是「必须先打开过一次目录页」才装得上；而内置源恰恰不写缓存，在该源下点安装必然 412。改为直读 `builtin_items()` 后这个 412 消失。

### 二、目录 32 → 21

删掉的 11 条分三类，前两类是硬判断：

| 类别 | 条数 | 条目 |
|---|---|---|
| 与内建工具完全重复 | 6 | filesystem ↔ `file`(9 工具)、memory ↔ `memory`+`daily_memory`、sequentialthinking、fetch ↔ `web_extract`、**markitdown ↔ 内建 `markitdown`**、commands ↔ `terminal` |
| 浏览器重复 | 2 | puppeteer、browserbase ↔ `browser`(8)+`cua`(16) |
| 同类重复 | 2 | 第二个 notion；full-access 版 postgres（与留用的 read-only 版**权限模型相反**） |
| 测试服务器 | 1 | `server-everything`（"Reference test server exercising every MCP feature"）居然在精选里 |

保留的 21 条：13 条需用户填凭据（github/gitlab/slack/airtable/notion/brave/exa/firecrawl/supabase/neon/upstash/redis + 需 URL 的 qdrant），8 条零凭据（youtube-transcript、read-only postgres、k8s、ssh、time、sqlite 等）。

**新增两条准入硬约束**（写进 JSON 的 `comment` 并由测试钉死）：
1. 不得与内建工具重复——重复的 MCP server 只会给 LLM 增加同义工具、拉高 token 成本、并在选工具时制造歧义；
2. 同一能力只留一个。

### 三、补 env 输入 UI

`McpCatalogGrid` 原先硬编码 `api.installMcpFromCatalog(item.id, {}, acknowledge)`，而 21 条里 13 条 `required_env` 非空——**它们一直是"看得见装不上"**，点安装必然 400。新增凭据对话框（密码框、逐项、非空才放行），值只发往本机后端写入 `mcp_servers.json`。

428 重发（`mcp_scan` 判 warn 时）必须原样带上 env——重发是一次全新请求，不带就又缺 env 了，故新增 `lastEnv` 状态承接。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 保留 `Official` 源但改默认值为内置 | 保留了一套"联网失败要回退、缓存要刷新、切源要重拉"的机制，而主路径已经不需要它。留着就是两套并行的真相。 |
| 目录清空、全部交给用户手动填 | 内建 79 个工具 + 34 个 toolset 之外再要求用户手写 MCP 配置，与"内置就是内置"的意图相反。 |
| 只收窄目录，不动远程链路 | 官方源仍会在目录里注入 400+ 条与内建重复的条目（`server-filesystem` 等），收窄成果被在线版稀释——L2 合并时精选 id 命中才不被覆盖，等于把策展成果交给字母序。 |
| 保留 `Manual` 条目但实现 pypi 安装 | 目录里 4 条 manual 全是 pypi 系。实现它需要引入 `uvx`/pip 运行时与另一套依赖解析，且这些条目多数已被内建 `read_pdf`/`file_converter` 覆盖。收益低，供应链面大。 |
| 逐条评估后再决定收窄 | 已经做了——收窄依据是逐条比对内建 toolset 清单（`file` 9 / `browser` 8 / `cua` 16 / `web` 2 / `memory` 2 / `document` / `pdf` / `office` 等 34 组），不是按知名度删。 |
| install 改走本地打包二进制 | **本轮不做**，见「遗留工作」。 |

## Consequences

- **可用率从 31% 提到接近 100%**：此前 32 条里只有 10 条能真装上（B 桶 14 条缺 env 输入框、C 桶 8 条 501）。现在 21 条全部可走通安装路径。
- **412 死结消失**：install 不再依赖"先打开过一次目录页"。
- **失去远程目录的广度**：从 ~500 条收窄到 21 条，且**新增条目或升版本必须跟应用发版走**。这是纯本地方案的固有成本，明确接受。
- **失去自建子注册中心能力**：该能力此前是官方支持的（`self-hosted` 源 + `registry_custom_url` + `HERMIX_MCP_REGISTRY_URL`）。企业内网用户若需要自建目录，现在没有出口。
- **目录更新节奏变慢**：新 server 上架要等发版。此前随官方 registry 走，理论上当天可见。
- **两条策展约束由测试钉死**：`builtin_catalog_does_not_duplicate_builtin_tools` 与 `builtin_catalog_has_no_same_capability_duplicates` 会在有人"看着眼熟又加回来"时立刻失败。原有 7 个针对远端归一化的测试随 `Raw*` 一并删除，替换为 6 个内置目录不变式测试 + 保留的 4 个中文对照表测试。
- **未做 auto-seed**：MCP 是外部服务的接入层，零凭据的条目要么已被内建覆盖、要么需要本就有集群/主机——没有真正"开箱可用且不重复"的候选。硬播种只会把重复项塞进默认上下文。
- **顺带修掉一处全仓编译中断**：`hermix-tools/src/tools/workflow.rs` 在 `360b7028` 提交时带了 E0308 + E0499 两个错误，main 当时编译不过。本篇不含该修复，单独成 commit。

## 遗留工作

**install 走本地打包二进制（替代 `npx -y`）本轮未做。** 原因不是工作量小，而是技术上不成立到可以顺手做的程度：目录里 17 条 npm 系 server 全是 **Node 程序**，"打包成二进制"要么给每个 server 附带一份 Node 运行时，要么逐个用 pkg/deno 之类转成独立可执行文件——这是每个 server 一项工作，且与 `mcp_scan` 现有的"会在本机执行第三方代码"告警语义直接冲突。在这件事落地前，`npx -y <pkg>@<ver>` 保持不变（它是版本钉死、无全局安装、不污染用户环境的正确做法）。

其余已知遗留：Custom Apps 的形态契约冲突（`hx-packager` 产出 `dist/*.exe` vs scanner 只认源码）、Skills 的 hub 客户端已实现但从未接线且架构文档判 TRIM、按扩展维度的使用数据仍然缺失（`hermix-telemetry` 只有进程内聚合计数，`record_tool_call` 连工具名都不收）。
