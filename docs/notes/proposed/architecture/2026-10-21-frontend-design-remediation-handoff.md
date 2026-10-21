# Decision Note: 前端设计整改交接（审计 → 8 个可并行批次）

Status: implemented — 批次已全部执行完毕：P0 六项真缺陷修复、P1 四个批次（弹窗/表单/视觉/文案）落地、两条新门禁接入 CI 并转阻断；执行结果见 [2026-10-21-frontend-design-audit.md](2026-10-21-frontend-design-audit.md) 的「执行记录」

## Decision

审计报告只给结论，不给执行顺序。本篇把结论转成**可分发给多个 agent 的批次**，
并用**文件所有权表**消除并行冲突：同一文件在整个整改期只属于一个批次，
需要动别人文件的发现一律写进该批次的「移交项」，由主控 agent 统一转派。

**不取代** [2026-10-21-frontend-design-audit.md](2026-10-21-frontend-design-audit.md)（下称「审计」）。

### 文件所有权表（唯一真相源，冲突以此表为准）

| 批次 | 独占文件 |
|---|---|
| **1** 暗色代码面 + MarkdownBody 令牌化 | `apps/desktop-tauri/src/lib/codeHighlight.ts`、`packages/preview/src/lib/codeHighlight.ts`、`apps/desktop-tauri/src/components/CodeView.tsx`、`apps/desktop-tauri/src/components/MarkdownBody.tsx` |
| **2** 主题对比度 + 对比度门禁 | `apps/desktop-tauri/src/styles/tokens.css`、`apps/desktop-tauri/src/styles/themes/dark.css`、`apps/desktop-tauri/src/styles/themes/ink-green.css`、`scripts/check-theme-tokens.mjs` |
| **3** 消息流缺陷 | `apps/desktop-tauri/src/components/MessageList.tsx`、`ToolCallBubble.tsx`、`StreamingBubble.tsx`、`TextBubble.tsx`、`MessageActions.tsx`、`MessageCopyAction.tsx`、`apps/desktop-tauri/src/lib/markdownFence.ts` |
| **4** 弹窗收编 | `apps/desktop-tauri/src/components/ui/Modal.tsx`、`settings/ModelAddDialog.tsx`、`config/SkillCard.tsx`、`AutomationView.tsx`、`config/McpCatalogGrid.tsx`、`CustomAppsView.tsx`、`SessionPickerDialog.tsx`、`EditProfileDialog.tsx`、`agents/TestDialog.tsx`、`custom-apps/AppDetailModal.tsx` |
| **5** 表单与控件层 | `apps/desktop-tauri/src/components/ui/Input.tsx`、`ui/Switch.tsx`、`ui/Button.tsx`、`settings/AdvancedSettingsTab.tsx`、`settings/VoiceTab.tsx`、`settings/SecurityTab.tsx`、`settings/BackendSettingsTab.tsx`、`settings/SettingsShell.tsx`、`settings/ImChannelsCard.tsx`、`config/ConnectorsTab.tsx`、`agents/AgentForm.tsx` |
| **6** 视觉细节 | `apps/desktop-tauri/src/components/ui/EmptyState.tsx`、`styles/components-prose.css`、`ToggleSwitch.tsx`、`VaultPanel.tsx`、`ToastContainer.tsx`、`agents/AIGenerateWizard.tsx`、`custom-apps/iconResolver.tsx`、`Sidebar.tsx`、`Composer.tsx` |
| **7** 新增 CI 门禁 | `scripts/check-design-guard.mjs`、`scripts/check-design-primitives.mjs`（新建） |
| **8** 文案一致性 | `apps/desktop-tauri/src/components/strings.ts`（新建）、`PreviewPanel.tsx`、`ModelListTab.tsx`、`ToolsetsTab.tsx`、`SkillsTab.tsx`、`ErrorBoundary.tsx`、`ScreenshotEditor.tsx`、`ScreenshotOverlay.tsx`、`FileSearchWindow.tsx`、`settings/DailyMemoryPanel.tsx`、`settings/PersonasPanel.tsx`、`settings/MemoryTab.tsx`、`settings/MemoryTextPanel.tsx`、`settings/MemoryGraph.tsx`、`settings/DiagnosticsTab.tsx`、`settings/ArchiveTab.tsx`、`settings/DevicesTab.tsx`、`settings/RuntimesTab.tsx`、`settings/ChoiceCard.tsx`、`AnalyticsView.tsx`、`agents/AgentCard.tsx`、`agents/BuiltInExpertsTab.tsx` |

**不在表内的文件**：任何批次都不得改动。需要动 → 写进该批次的「移交项」。

### 波次与并行度

| 波 | 批次 | 可并行 | 内容 | 验收门槛 |
|---|---|---|---|---|
| **W1** | 1 / 2 / 3 | ✅ 3 个并行 | P0 真缺陷（主题不可见、暗色代码错色、memo 比较器、流式契约） | 编译过 + 三道门禁绿 + 实机切主题验代码块 |
| **W2** | 4 / 5 / 6 | ✅ 3 个并行 | P1 一致性收敛（弹窗、表单、视觉） | 编译过 + 门禁绿 + `impeccable detect` 0 |
| **W3** | 7 / 8 | ✅ 2 个并行 | P2 门禁 + 文案 | 门禁全绿 + 新门禁能在故意破坏时报错 |

W2 必须在 W1 之后：批次 2 会改 `tokens.css` 的语义色，批次 5/6 会消费这些令牌。
W3 必须在 W2 之后：批次 7 的门禁规则以收敛后的现状为基线，否则会立刻红。

### 每个批次的硬性纪律（8 份提示词共用）

1. **只改所有权表内的文件**。其余一律进「移交项」。
2. **不新增依赖**（含 npm 与 cargo）。
3. **禁**：字面色值（`#hex` / `rgb(`）、`dark:` 变体、`window.confirm`、`加载中…` 字面量。
4. **改 UI 必跑**：`npx -y impeccable@4.1.0 detect <改动路径>`，退出码 0 才算干净。
   **注意**：detect 对解析不了的文件会静默返回 0 —— 先跑 `tsc --noEmit` 再信它的绿灯。
5. **类型检查必须直调二进制**：`cd apps/desktop-tauri && ./node_modules/.bin/tsc --noEmit`。
   `pnpm tsc --noEmit` 不会真正执行类型检查（只跑 pnpm install 流程就返回 exit 0，是假信号）。
6. **三道门禁**（W1 后每批必跑）：
   ```bash
   node scripts/check-theme-tokens.mjs
   node scripts/check-preview-layer.mjs
   (cd apps/desktop-tauri && node ../../scripts/check-design-guard.mjs)   # 必须从 apps/desktop-tauri 跑，脚本按 cwd 拼 src/
   ```
   注意 `check-design-guard.mjs` 用 `process.cwd() + "src"`，在仓库根跑会 `ENOENT` 崩掉。
7. **Windows / MSVC only**；不引入跨平台产物。
8. **交付格式**（四段，缺一不可）：
   - 改了什么：逐条 `file.tsx:LINE — 改前 → 改后`
   - 验证证据：上面 4 条命令的**真实输出**（不是「应该通过」）
   - 移交项：审计里落在你文件之外的、你判断需要改的条目
   - 遗留风险：没做完的、需实机确认的

### 与审计的对应

批次 → 审计条目：
1 → 问题 26/34/35/37/38 + 硬编码字号表（MarkdownBody 5 处）；2 → 问题 18/19/20 + §8 对比度门禁；
3 → 问题 4/27/28/29/30/31/32/33 + P0-3；4 → 问题 42–47 + §3.2 重复清单第 1 行；
5 → 问题 12/13/24/48–52/53 + §3.3 焦点态；6 → 问题 3/7–11/28/39/40 + EmptyState/描边；
7 → §8 全部 7 条门禁；8 → 问题 12–15 + §4.2。

## Alternatives considered

- **单个 agent 串行做完全部**：否决——63 条发现跨 40+ 文件，串行需要数天且中途无可见产出。
  分批后 W1 一天内可交付 3 个独立可见的修复。
- **按「文件类型」分批（所有 tsx 一个 agent）**：否决——那会退化成串行。
  按**功能域**分才能真正并行。
- **不做文件所有权表，靠 agent 自觉**：否决——并行 agent 改同一文件必然产生 git 冲突与覆盖。
  所有权表是本篇的核心产物。
- **把所有权表写进每份提示词而不是本篇**：否决——8 份提示词各抄一份会漂移。
  提示词里写「先读本篇的 §文件所有权表」。

## Consequences

- **W2 的三个批次工作量不均**：批次 4 要改 10 个弹窗实现，批次 6 要改 9 个文件但每个改动小。
  若 W2 进度不齐，以批次 4 为瓶颈。
- **批次 2 会改 `tokens.css`**，而批次 1/3 可能想加令牌 —— 批次 1/3 的新令牌需求一律进「移交项」。
- **批次 7 的门禁一旦全开，CI 会变红** —— 必须先让 W2 收敛完成再开阻断。
  建议顺序：先只开 `contrast` 规则 → 修完 → 再开语义色类名 → 再开图标/控件高度。
- **批次 5 改 `ui/Button.tsx` 的焦点环 offset** 会影响全 app 按钮的观感，需实机走查确认。
- 审计中标为「未核验」的三类结论（图标视觉重量、表单控件实际渲染高度、文案语流长度感）
  **在 P1 阶段仍需实机复核** —— 批次 4/5/6 的执行者应把实机观察写进交付说明。
- 整改完成后应把审计文档的状态从「发现」更新为「已处理」或归档，
  避免下一轮重复审计同一批问题。
