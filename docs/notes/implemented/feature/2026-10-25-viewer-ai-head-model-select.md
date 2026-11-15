# Decision Note: AI 栏标题栏直选模型

Status: implemented — AI 助手标题栏右侧放模型下拉（inherit 模式列出主程序启用的模型直接切换）；原头部下方的只读后端行仅在未就绪时保留

## Decision

AI 栏头部原本只显示一条**只读**后端信息（「主程序配置 · kilo-auto/free」），
想换模型只能绕 设置 → AI 助手；而设置对话框里的模型清单（Rust 侧
`viewer_ai_models`：主程序 `model_selector.enabled` + 各供应商兜底 +
免费渠道 seed）与写回机制（`settings.ai.selected_model`）都是现成的。

把选择器搬进标题栏：

- `ai-head` 右侧加紧凑 `<select>`（`.ai-model-select`，22px 高）：
  - **inherit 模式**：「跟随主程序默认」+ 启用过的模型清单，选中即写
    `selectedModel` 并刷新生效状态（`saveSettings` → `loadAiStatus`），
    全程不出 AI 栏；
  - **custom 模式**：置灰显示生效模型——端点/模型是设置里手填的，
    没有清单可选，悬停提示去哪里改；
  - **未就绪**：下拉染危险色，头部下方保留一行红字说明原因（就绪时
    该行删除——模型信息由下拉自己承担，头部少一行）。
- 选项文字带「（默认）/（不可用）」标记；生效详情悬停可见
  （`生效：{backend.model}`）。

## Alternatives considered

- **头部只显示 + 点击跳设置**：多一跳；清单和写回都是现成 API，
  直选成本低。
- **下拉里也放 custom 模式**：custom 的模型是自由文本不是枚举，
  塞进下拉要造一个「编辑项」混合控件；置灰 + 提示更诚实。
- **删除未就绪红条**：原因文字（缺 base_url / key）比下拉选项长，
  留一行专门说明。

## Consequences

- inherit 模式换模型零跳转；custom 模式头部可见当前模型名；
- `pickModel` 走 `saveSettings` 全量写回（与设置对话框同一通道），
  存失败不阻塞、本次会话按旧值走；
- AI 栏头部在未就绪时比原来多一次红条渲染（就绪时反而少一行）。

验证：viewer `tsc --noEmit` 0 错；`vite build` 通过；`impeccable
detect`（AiSidebar.tsx）exit 0；`check-preview-layer` OK；debug 实例
实测——头部下拉出现且列出主程序模型清单，选中 deepseek-v4 后状态
即时变为「未就绪：deepseek 没有 base_url」（该供应商确无端点，反馈
链路正确），切回「跟随主程序默认」红条消失恢复原状。
