# 模型/供应商配置界面重设计方案

> 状态：**设计稿，待审定后另起批次实施**（不直接动代码）。
> 关联批次：`07b42ebb`（双权威统一）+ `098f3d3`（safeId 校验 + setConfig 反模式）已消除"同一份数据 3 处各认一个权威"的 bug；本方案在此之上做**交互简化**，属产品设计层，非 bug 修复。
> 参考成熟方案：pi-mono（badlogic）的 provider 单一实体架构 + deepseek-harness 的配置文件驱动。

## 1. 现状痛点（审计实证）

### 1.1 五处配置面分散

| 配置面 | 文件 | 职责 | 痛点 |
|---|---|---|---|
| ModelTab | `components/settings/ModelSettingsTab.tsx` L86-550 | 默认模型选择器 + 已启用列表 + 内置供应商面板（18 家） | 1200 行单文件，3 个 SECTION 一屏塞满 |
| CustomProvidersTab | 同文件 L1122-1200 | 自定义 OpenAI 兼容供应商 CRUD | 与 ModelTab 并列 sub-tab，供应商配置逻辑各走一套 |
| 图像 sub-tab | `SystemConfigPanel.tsx` | 图片生成开关 + 图片模型名 | 纯 localStorage，不写后端 |
| ModelSelector | `components/ModelSelector.tsx` | 聊天窗口模型选择器 | 独立 reconcile 逻辑，消费 enabled 列表 |
| useCurrentModel | `hooks/useCurrentModel.ts` | 当前模型状态 hook | 又一份独立读取路径 |

**"加一个新供应商并设默认"当前步数**：切到「自定义模型」sub-tab → 展开 CustomProviderSection → 填名称/API URL/Key/模型清单（4 个输入框）→ 点「保存并获取模型」→ 切回「模型配置」sub-tab → 在「已启用模型」里找到新模型 → 点「设为默认」。**跨 2 个 sub-tab、6+ 次点击、3 个组件协作**（CustomProviderSection → ModelTab state → ModelSelector 事件扇出）。

### 1.2 内置 vs 自定义两套路径

| 配置项 | 内置 provider（ModelTab） | 自定义 provider（CustomProvidersTab） |
|---|---|---|
| API Key 填写 | `providerKeyInput` state（内存）→ `setEnvVar` 写 `.env` | 表单 `apiKey` 字段 → `createCustomProvider` 写后端 |
| 获取模型 | `fetchModelsForProvider`（前端直连厂商 `/v1/models` 探测） | `handleFetchModels`（后端 `fetch_models` 端点） |
| 启用模型 | `toggleModel`（自动合并探测结果） | 手动逗号分隔输入 |
| 设默认 | `setDefault`（两级选择器） | `setDefault`（独立 handler） |
| context window | `useContextWindows(enabledFids)` | `useContextWindows(providerModelIds)` + 手填 `contextWindowInput` |

两套路径在 5 个配置面上各渲染一遍，用户心智负担：「内置的怎么配？自定义的怎么配？为什么不一样？」

### 1.3 信息密度

ModelTab 单屏 3 SECTION：默认模型（两级下拉）+ 已启用模型（可无限长列表）+ 供应商（18 家折叠面板堆叠）。用户配一个默认模型要滚动过整个已启用列表 + 18 家供应商。

## 2. 目标架构（对照 pi-mono 单一实体）

### 2.1 核心原则

**Provider 统一实体，前端不再区分"内置/自定义"两套 UI。**

pi-mono 证据（`packages/ai/src/models.ts:1021`）：
> "Built-in provider factories and models.json custom providers both go through this [createProvider]."

即：内置 provider 和自定义 provider 都是同一个 `Provider { id, name, baseUrl, auth, models, api }` 实体，唯一的差异是**来源**（内置 = 代码内置 catalog + env var auth；自定义 = models.json + 用户填的 baseUrl + apiKey）。UI 层只需一套供应商管理面板，按"来源"渲染差异（内置显示"已配置/未配置"状态 + API Key 输入；自定义显示完整表单）。

### 2.2 目标交互（简化后）

**单面板「供应商与模型」**，替代现在的 ModelTab + CustomProvidersTab 两个 sub-tab：

```
┌─ 供应商 ──────────────────────────────────────────────┐
│ [已配置] OpenAI    GPT-4o (默认)   ⚙️  [已配置] Anthropic  │
│ [已配置] DeepSeek  ...            [未配置] Google  [+ 配置]│
│ [自定义]  我的OpenAI兼容供应商  gpt-3.5-turbo  [编辑] [删除]│
├─ 模型选择 ──────────────────────────────────────────────┤
│ 默认模型: [供应商 ▾] [模型 ▾]  上下文: [128K ▾]         │
├─ 已启用模型 ────────────────────────────────────────────┤
│ ☑ OpenAI/GPT-4o (默认)  ☑ DeepSeek/V3  ☐ Google/Gemini  │
│ (折叠：N 个未启用模型)                                   │
└──────────────────────────────────────────────────────────┘
```

**"加新供应商并设默认"简化后步数**：点「+ 配置」→ 选供应商类型（内置/自定义）→ 填 1-4 个字段（内置只需 API Key；自定义填名称+URL+Key）→ 自动获取模型并启用可对话模型 → 自动设默认。**1 个面板、2-4 步、无 sub-tab 切换**。

### 2.3 数据流统一

```
后端 config.yaml (model_selector + custom_providers)  ← 单一持久权威
    ↑↓ writeModelConfigAndNotify (一次写 localStorage + 推后端 + 事件扇出)
localStorage (跨 tab 瞬态镜像, 离线兜底)
    ↑
5 处消费面 (ModelTab/CustomProvidersTab/图像/ModelSelector/useCurrentModel)
    全部读同一份镜像, 不再各认权威
```

## 3. 文件改动面

| 文件 | 改动 | 规模 |
|---|---|---|
| `components/settings/ModelSettingsTab.tsx` | ModelTab + CustomProvidersTab 合并为单一「供应商与模型」面板；CustomProviderSection 改造为统一供应商卡片（内置/自定义同一组件，按来源渲染差异） | 1200 行 → 预计 800 行 |
| `components/SystemConfigPanel.tsx` | 删「模型配置」「自定义模型」两个 sub-tab，合并为单一「供应商与模型」sub-tab；图像 sub-tab 保留 | -50 行 |
| `components/ModelSelector.tsx` | 不变（已消费统一镜像） | 0 |
| `hooks/useCurrentModel.ts` | 不变 | 0 |
| `api/rest.ts` | `getSystemStatus` 已死删（`9a066d6`）；`createCustomProvider`/`deleteCustomProvider`/`fetchCustomProviderModels` 等保留（后端路由保留） | 0 |
| 后端 | **不动**（`custom_providers.rs` 6 端点 + `/api/providers/meta` + `/api/model/list` 能力面已够） | 0 |

**总改动面：1 个前端文件（合并 + 改造）+ 1 个 sub-tab 调整。** 纯前端，后端零改动。

## 4. 风险与回归面（对抗性审查）

| 风险 | 场景 | 缓解 |
|---|---|---|
| 供应商面板渲染 18+ 家卡顿 | 大目录 provider 列表一屏渲染 | 折叠面板 + 虚拟滚动（仅展开的渲染明细） |
| 内置/自定义同组件后字段差异漏渲染 | 内置 provider 无 baseUrl/models 字段，自定义有 | 组件按 `provider.source` 分支渲染，单测覆盖两种来源 |
| 默认模型自动设错 | 获取模型后 `pickDefaultModel` 选错（embedding/rerank） | 已有 `isChatModel` 过滤 + 后端权威 `providerDefaults`，保留 |
| localStorage 镜像与后端漂移 | 离线时改 enabled，上线后与后端 config.yaml 冲突 | 已有双权威统一规则（`07b42ebb`：后端可达→本地∩后端；离线→本地兜底），不变 |
| 图像 sub-tab 孤立 | 图像模型不走供应商面板 | 保留独立 sub-tab（图片生成是独立能力，不强塞进供应商面板） |
| 回归面：现有用户 enabled 列表 | 合并后 enabled 列表语义不变（仍读同一镜像） | 写路径未变（writeModelConfigAndNotify），数据兼容 |

## 5. 分阶段实施计划

| 阶段 | 内容 | 验证 |
|---|---|---|
| **P1** | 合并 ModelTab + CustomProvidersTab 为单一 sub-tab（SystemConfigPanel 调整）；CustomProviderSection 抽取为统一供应商卡片组件（内置/自定义同组件按来源渲染） | tsc 全绿 + 手动：加自定义供应商 2-4 步内完成 |
| **P2** | 信息密度优化：已启用模型列表折叠未启用项 + 供应商面板虚拟滚动 | 手动：大目录（>50 模型）无卡顿 |
| **P3** | 默认模型自动化：获取模型后自动启用可对话模型 + 自动设默认（现有 `pickDefaultModel` 逻辑前移） | 手动：首次配置 ≤ 2 步 |
| **P4** | 真机验收 P5 全量（供应商配置全流程） | 真机窗口 |

**P1 是核心**（消除"两套路径"痛点），P2/P3 是体验优化，P4 验收。每阶段独立 commit + 双绿验证。

## 6. 审定清单

- [ ] 原则：Provider 统一实体、前端不区分内置/自定义两套 UI（对照 pi-mono）
- [ ] 交互：单面板「供应商与模型」替代两个 sub-tab
- [ ] 范围：纯前端改动，后端零动
- [ ] 分阶段：P1 合并 → P2 密度 → P3 自动化 → P4 真机
- [ ] 图像 sub-tab 保留独立（不并入供应商面板）

审定后另起批次实施 P1。
