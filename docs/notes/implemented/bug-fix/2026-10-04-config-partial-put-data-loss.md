# Decision Note: `PUT /api/config` 半截 payload 覆盖 config.yaml 导致模型配置全丢

Status: implemented — 后端改合并语义 + 前端改读改写，两处均已落地并有回归测试；已丢失的数据需用户在 UI 重建（无备份可恢复）

## 事故

2026-10-03 11:13:02，用户在桌面端保存「高级设置」，`model` / `llm_providers` / `model_selector` / `custom_providers` 四段配置被整段抹掉。日志证据（`~/.hermix/logs/hermes-server.2026-10-03`，UTC+8）：

```
11:13:02  PUT /api/config → AgentService::set_config: model=None providers=0
11:37:26  stdio: shutdown frame received
11:37:27  startup: auto-corrected config.model before=None after=minimax:MiniMax-M3 reason=unset
```

11:13:02 的写入把 config.yaml 换成了只剩 `compaction` + `advanced` 两节的文件；11:37:27 应用重启时 `load_config` 用 serde 默认值补全、`.env` 重新注入了 deepseek/minimax 的 key，`auto_correct_model` 补回 `model` 并把这份"全默认"配置落盘。**聊天始终能用**（`model` + key 活着），但 `GET /api/model/list` 兜底只认 `llm_providers[*].model`，而两家的 `model` 字段都没了 → 返回空列表 → 模型配置页显示"还没有配置任何模型"。

两个肇祸点：

- `hermix-server/src/dashboard/config.rs` — `serde_yaml::to_string(&body.config)` 把请求体**原样**写成整份 config.yaml，不与现有配置做任何 merge。
- `apps/desktop-tauri/src/components/settings/AdvancedSettingsTab.tsx` — 唯一发半截配置的调用方，`handleSave` 只 PUT `{compaction, advanced}`。`HermesConfig` 是 `{[key: string]: unknown}` 索引签名，加上的 `as unknown as api.HermesConfig` cast 让 TS 也拦不住。同文件的 `SecurityTab` 本来就是正确的读改写写法。

## Decision

**`PUT /api/config` 改为合并语义，不再整份覆盖。** 读现有 config.yaml → 把请求体里出现的顶层键合并进去 → 原子写 → 把**合并后的全量**推给活 agent service。合并语义取 JSON Merge Patch：出现的键覆盖，显式 `null` 删除，缺失的键不动。

同时 `AdvancedSettingsTab.handleSave` 改为先 `api.getConfig()` 拿全量再 spread 覆盖自己那两节，并去掉 `as unknown as` cast。

两道防线是有意的：后端那条防住"任何调用方"（含新写的、脚本的、手 curl 的），前端那条让调用方的意图在代码里就是对的。

## Alternatives considered

| 方案 | 为何否决 |
|---|---|
| 只改前端，保留整份覆盖 | 后端仍是"整份覆盖"陷阱，下一个发半截 payload 的调用方（或脚本 / curl）会再次清空。事故复发的成本是一次静默数据丢失，防线不能只建在调用方自觉上。 |
| 后端反序列化成 `GatewayConfig` 再逐字段合并 | 要手写每个字段的合并规则，漏一个就是一次新的静默丢失；且会丢掉 YAML 里 `GatewayConfig` 不认识的键。JSON Value 层的键合并没有这个维护面。 |
| 递归深合并嵌套对象 | 会让"清空某段"无法表达——`deepseek: {}` 和"没提到 deepseek"在深合并下不可区分，而清空 `llm_providers` / `custom_providers` 是合法操作。顶层粒度是"能表达删除"与"规则最少"的交点。 |
| 加校验：拒绝不带 `model` 的 payload | 只挡这一种误用形态。事故的根因是"半截 payload 被当成整份"，不是"某个字段缺失"。 |
| 只加 config.yaml 快照 / 自动备份 | 事后可恢复，但配置仍然是错的。实测 `~/.hermix` 下不存在任何 config.yaml 副本，`security.auto_backup` 并不覆盖它；WebView2 localStorage 里也没有 `model_config` 镜像可捞。 |

## Consequences

- **合并是顶层粒度。** 想改嵌套对象的某个子键，调用方必须整段发送该嵌套对象（`SecurityTab` 已是这个写法）。这是有意接受的代价：换来"缺失即不动、null 即删除"的清晰语义。
- **推给 agent service 的必须是合并后的全量。** 修复前推的是 `body.config`，会出现"磁盘对了但内存还是截断版"的次生 bug——`GET /api/config` 和 `GET /api/model/list` 都优先读 `current_config_value()`，会继续报空。
- **对依赖整份覆盖的调用方是破坏性变更。** 全仓 `updateConfig` 调用点只有 `SecurityTab`（本就发全量）与 `AdvancedSettingsTab`（本次修正）两处，均已核对。
- **新增两个私有函数** `read_config_json` / `merge_config_patch`，加 3 个单测（`dashboard::config::config_patch_tests`）：半截 patch 保住模型配置、覆盖与 null 删除语义、缺失/损坏文件降级为 `{}`。
- **已丢失的配置无法自动恢复。** 无磁盘备份、localStorage 镜像也已被清空；`model_selector` 的原启用清单和 5 个自定义供应商的模型名不可考。凭据仍完好（`.env` 里 5 个 `CUSTOM_*_API_KEY/_BASE_URL` + `MINIMAX_API_KEY` + `DEEPSEEK_API_KEY` 都在），且「添加模型」弹窗能自行拉取端点 `/models`，因此可在 UI 重建。
- **未验证项：** 事故当天的 config.yaml 原内容无从核对，"只丢了上述四段"是基于 `load_config` 默认值补全行为与落盘结果的推断。
