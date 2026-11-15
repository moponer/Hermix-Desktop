# Decision Note: 内置免费大模型渠道 —— 端点情报复用，不移植 dsh 插件

Status: proposed — 可行性评估完成；结论：免费模型走原生内置 provider 路线（小型改动、无新依赖），不为此引入 Node 插件宿主；前置条件是合规拍板端点白名单

关联笔记：[DSH 插件兼容层](../architecture/2026-10-19-dsh-plugin-compatibility.md)（同日提出）。两篇不互相取代：兼容层笔记的目标是 dsh 插件**生态**（工具/命令/模型插件的通用运行时），OFM 只是它的 L1 验收插件；本篇只回答「**免费模型能力如何进 hermix**」这一个问题，并论证这一个问题不需要插件运行时。若本篇被采纳，兼容层笔记的 L1 验收目标应另选（不再以 OFM 为首枚），其生态价值不变。

## 背景

用户诉求：评估集成 `Ebony-Vinyl/dsh-our-free-model`（下称 OFM），复用其免费大模型连接能力，为 hermix 内置若干免费模型。要求覆盖：集成难度、模型信息自动更新、可配置开关、风险。

OFM 调研事实（2026-10-19，依据其 GitHub README 与 [兼容层笔记的拆解](../architecture/2026-10-19-dsh-plugin-compatibility.md)）：

- 形态：DeepSeek Harness 宿主的第三方**插件 + 本地网关**，明文 JavaScript、零依赖、无构建。不是 SDK/库，宿主半身 2366 行 + 浏览器半身 6579 行 + `src/` 约 40 模块。
- 能力分层：① 免费车道——OpenCode 免密 Zen 网关（匿名 `Bearer public`）；② Kilo 公共网关免费模型池（上游自述 prompts may be logged）；③ EAC 渠道——凭据 AES-256-GCM 密封、密钥分片按宿主即时派生，**刻意规避检测**；④ 13 个账号渠道（各厂商免费积分整合）；⑤ 75 家 provider 注册表（`src/providers/registry.json`，源自 free-claude-code / 9router）。
- 数据来源：模型清单**实时拉上游接口**、不存静态快照（`GET .../inference/v1/models`、`GET api.kilo.ai/api/gateway/models`）。
- MIT，26.2k star，活跃；README 自述与上游厂商无隶属，并警告 npm 存在同名单名篡改包。

hermix 侧可复用资产（已核实）：

- `GenericProvider`（`crates/hermix-agent/src/provider.rs:54`）：OpenAI 兼容、任意 base_url + key + model；`build_runtime_provider`（`runtime_provider.rs:221`）对未知 provider 名兜底走它——**接一个免费网关不需要写新 provider 代码**。
- 自定义 provider 全套 REST API（`crates/hermix-server/src/dashboard/custom_providers.rs`）：`fetch_models`（:322）已实现「GET `{base_url}/models` → 解析 → 模型 CSV + context_window 回写 config.yaml」，`create` 后 `set_config` **热推送**运行中的 agent。
- 单模型开关已存在：`model_selector.enabled`（`PUT /api/model/list`，`dashboard/config.rs:467`，持久化 + 热推送），前端 ModelSelector/ModelListTab 即每模型勾选 UI。
- 内置厂商唯一真理源：`BUILTIN_PROVIDER_ENV_VARS`（`hermix-config/src/loader.rs:716`），`/api/providers/meta` 自动下发前端分组。
- 免 key 无先例：`resolve_runtime_api_key`（`runtime_provider.rs:54`）与 `fetch_models` 都强制非空 key。
- server 侧无定时任务先例；桌面端有现成的后台周期检查模式（`apps/desktop-tauri/src-tauri/src/updater.rs:266`）。

## Decision

**不移植 OFM 代码，只复用其端点情报**：把免费模型做成 hermix 原生的「内置免费渠道」provider，分三层落地——

1. **接入层（小型改动）**：注册一个内置免费 provider（或预置 custom-provider 条目），base_url 指向匿名 OpenAI 兼容免费网关；放宽两处免 key 校验（`resolve_runtime_api_key` + `fetch_models`：api_key 为空/哨兵值则跳过 Bearer 头）。首期只接**一个**明确公开、允许匿名的网关（候选：OpenCode Zen，`Bearer public`），实测可用后再扩。
2. **自动更新层（中型改动，可后置）**：
   - 数据来源：网关自身的 `/models` 接口（OpenAI 格式，现有 `fetch_models` 解析器直接可用）。**不**抓取 OFM 仓库的 registry.json——链路脆弱且扩大供应链面。
   - 触发：应用启动时 + 设置页手动「刷新」（雏形已有）为 MVP；可选挂 Tauri 侧 updater 式调度器。不给 server 引入 cron 框架。
   - 兜底三级降级：上游失败 → 沿用上次成功快照（fetch 结果已持久化在 config.yaml）→ 编译期 seed 清单。刷新结果**只增不删**（删除项需显式确认），防止上游一次抖动清空用户已启用模型。
3. **开关层（大部分现成）**：全局开关（启/停免费渠道 + 是否参与自动刷新）+ 单模型开关（直接复用 `model_selector.enabled`）；持久化走 `save_config_yaml` 原子写 + `set_config` 热推送，无需重启。**默认关闭**（或仅预启用 1–2 个实测最稳的模型）。

**明确不做**：移植/内嵌 OFM 的 JS 代码（含打包 Node sidecar）；EAC 渠道及任何密钥混淆、规避检测机制；账号渠道（各厂商积分整合，合规风险同等且面更大）。

**前置条件（未拍板不动工）**：
1. 端点白名单：哪些免费端点允许随产品内置（匿名公开网关优先；疑似厂商内部端点的第三方套用一律不进白名单）——负责人/法务拍板。
2. UI 免责：标注「社区免费源，随时可能失效；对话数据将流经第三方且可能被上游记录」；敏感会话不默认路由到免费源。
3. 免费源整体标记「尽力而为」：不作为默认模型的唯一候选；context_window 用保守实测值。

工作量：MVP（接入层 + seed + 开关 + 免 key 放宽）为小型改动，几个文件、无新增依赖；自动更新层为中型。

## Alternatives considered

- **A. Node 插件宿主跑真 OFM 插件**（[DSH 插件兼容层](../architecture/2026-10-19-dsh-plugin-compatibility.md) Phase 0/1）：生态价值最大，但对「内置免费模型」这一个目标是杀鸡用牛刀——需新增 `hermix-plugin-host` crate、捆 Node 22（50–60MB）、JSON-RPC 双向映射、pnpm 依赖解析，MVP 2–3 周 vs 原生路线几天；且 OFM 的设置能力在浏览器半身（L4），兼容层明确不做，「能跑但不能配」。插件运行时是长期维护负担（兼容层笔记自己承认）。**对本目标否决**；兼容层若立项，其价值在工具/命令生态，不在免费模型。
- **B. 把 OFM 的 JS 裁剪后内嵌为专用 sidecar**：只保留免费车道逻辑。裁剪后仍需 Node 运行时，且 OFM 自带自升级/签名校验与其宿主耦合，裁剪版失去上游更新能力，变成我们维护的 fork。**否决**。
- **C. 纯静态内置清单，不做自动刷新**：最省，但免费源变动频繁（这正是要自动更新的原因），静态清单会静默腐烂，用户拿到的是一堆失效端点。**否决**（seed 清单仅作兜底，不是唯一来源）。
- **D. 不内置，只在文档里教用户把免费网关配成 custom-provider**：零代码，但每次升级/换机要手工重配，且与「内置若干免费模型」的诉求直接相悖；可作为 MVP 之前的过渡验证步骤（用现有 custom-provider UI 手工接一次 Zen 网关实测可用性）。**不作为交付形态**。
- **E. 全量同步 OFM 的 registry.json（75 家 provider）**：覆盖面大，但数据源本身是社区项目的社区项目（free-claude-code / 9router），无 SLA 无校验；上游 npm 篡改前科提示数据同样可能被投毒。**否决**，只取实测过的端点。

## Consequences

- 得到：免费模型以原生 provider 形态进选择器，复用既有开关/持久化/热推送/审批全链路；无 Node 运行时、无新依赖、无插件维护面。
- 放弃：OFM 的其余渠道（EAC、13 个账号渠道、75 家聚合注册表）——本方案只覆盖匿名免费车道；也放弃「装任意 dsh 插件」的生态叙事（那归兼容层笔记管）。
- 承担：免费端点随时失效的售后成本——用户会把免费源故障当成产品故障报障，UI 免责与失败明示是必须项，不是可选项。
- 承担：策展维护——seed 清单与端点白名单需要定期人工复核（上游无契约）。

## Reintroduction conditions

本提案若被否决（含合规否决），出现以下任一证据时应重新提起：

1. 出现**明确允许第三方接入**（公开 ToS / 官方免费层）的模型网关，合规疑虑消失；
2. 兼容层笔记落地 Node 插件宿主后，OFM 免费车道成为零成本副产品；
3. 用户侧出现高频自配免费网关的实证（custom-provider 使用统计），说明值得产品化。

## Open questions

1. 首个白名单端点是否为 OpenCode Zen？需实测：匿名额度、限流阈值、工具调用可靠性、真实 context 长度。
2. 默认开关：默认关闭，还是首启引导页提供「一键开启免费模型（含免责确认）」？
3. 免费源请求是否走既有 `cost_guard` / `rate_limit` 计量体系（建议走，便于看板与限额）。
4. context_window 兜底值取多少（建议保守 32k，实测后再调，避免压缩误判）。
