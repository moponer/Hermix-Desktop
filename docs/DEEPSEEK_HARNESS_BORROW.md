# deepseek-harness 可借鉴点调研

调研对象：<https://github.com/deepseek-ai/deepseek-harness>（MIT，TS/Cordis，默认分支 `master`，2026-09 活跃）
调研日期：2026-09-26

## 0. 一句话结论

它跟我们的底座方向**高度同源**（Cordis 可逆效果 + 能力接缝 + append-only session log，这三样 `docs/PLUGIN_KERNEL_DESIGN.md` 已经抄过一轮），所以"抄架构"的边际收益已经吃完了。
真正值得抄的是 **四件具体机制**，其中 `spill` 与我们的"零上下文膨胀"铁律直接对口、成本最低；`document` 能治掉 Office 预览对外部二进制的依赖。
至于文件预览组件本身——**它没有现成的可抄组件**，别抱期待。

## 1. 它是什么

- 内核：Cordis 插件系统，"Everything is a Plugin"——model / tools / skills / session / sandbox / storage / loop / schedule / UI 全是插件，靠配置层叠（profile → bundle → `cordis.patch.yml` → `--patch` overlay）组装，不 patch 内核源码。
- 不变量：**进模型的一切必须能从 session log 重建**。append-only 事件流是唯一真相源，`deriveMessages()` 从中投影模型历史；resume / fork / replay / telemetry / UI 全部同源。
- 33 个能力接缝（seam），每个接缝三角色齐全：**Service Definition（接口） / Provider（实现） / Consumer（通常是模型侧工具）**。
- Waterfall 事件：请求/工具执行前多个插件可观察、改写或拒绝，每个 listener 必须调 `next()` 委托。
- 四种运行模式：Standard / PTC(Code Mode) / Minimal / Creator。

## 2. 对照表

| # | harness 机制 | 我们现状 | 抄不抄 | 工作量 |
|---|---|---|---|---|
| 1 | `spill/`：`maxInlineTokens` 阈值，超长工具结果落盘 + 返回 preview + locator + 取回指引；存储失败则保留原结果 | **没有**。工具结果整体进上下文，超长就硬塞 | **抄**（最高优先级） | 小 |
| 2 | `document/`：Host 侧 Office → PDF 统一转换，前端只处理一种格式 | Office 预览走外部 `officecli` 二进制，缺失即报"渲染引擎未找到"；PPT/Word 走 iframe srcDoc | **抄思路**：统一转中间格式 | 中 |
| 3 | `ptc-runtime/`（Code Mode）：工具经 SDK 暴露给模型写 TS 程序，5 轮往返压成 1 次调用 | 无。工具逐个 function call | **抄**（省 token 大头） | 大 |
| 4 | `guard/`：重复调用提醒（advisory）+ `tools/execute`  deadline 强制执行器 | 只有 `convergence_detector.rs` 的 advisory warning，无 deadline 强杀 | **抄 deadline 部分** | 小 |
| 5 | `interaction/`：审批/交互接缝 + permission preset + `ask_user` 工具 | 审批在 `hermix-tools::approval`，未接缝化 | 部分抄（接缝化） | 中 |
| 6 | `session-query/`：bounded reads / lineage / SQLite FTS 检索会话 | 会话检索基本靠全量拉 | 抄 bounded read | 中 |
| 7 | `deliverables/`：显式文件交付 + 记录工作区变更 | 产物面板有，但无"本轮改了哪些文件"的语义记录 | 抄 | 小 |
| 8 | `attachment/`：内容寻址存储 + 稳定附件身份 | 图片走 data URL，8 MiB 上限，无去重 | 抄（顺带解决大图） | 小 |
| 9 | `sandbox/`：Landlock / Seatbelt / Windows restricted-token | 我们有 Job Object + `TerminalBackend`（Docker/WSL2），本机受限令牌未见 | 对照补 | 中 |
| 10 | 33 seam 清单 | `hermix-core::seams` 只有 6 个（Fs/Shell/ExecWorld/Sandbox/Transport/ToolHandler） | 抄清单做对照扩充 | 中 |

**明确不抄**：整套 Cordis/TS 运行时（`hermix-context` 已提供等价的可逆注册 + 层级作用域）；profile/bundle 配置层叠（`hermix-context::boot` 已有）；双语 `.i18n.yaml` 文档体系。

## 3. 文件浏览组件专项

结论先说：**harness 没有可抄的文件预览组件**。它的 `packages/web` + `client` 是 CLI/Web harness 的界面壳，不是文档预览器。我们能抄的只有一条后端管线：

- `document/`：Office → PDF 统一转换在 Host 侧完成，前端只渲染 PDF。

对照我们 `PreviewPanel.tsx` 的现状短板（按影响排序）：

1. **Office 依赖外部二进制**——`officecli` 不在就整体失败，这是最脆的一环。
2. **PDF 用 `<embed>`**——依赖 WebView2 内建 PDF 插件，工具栏/搜索/分页全不可控，视觉跟应用割裂。
3. **文本预览上限 2 MiB**，大文件直接报"文件过大"。
4. `SpreadsheetView` 无虚拟化，大 xlsx 渲染卡（`CodeView` 已虚拟化行号，表格没有）。
5. PPT 只能整页 HTML iframe，无分页导航/演讲者视图。
6. 图片走 data URL（8 MiB 上限），无缩放/EXIF 方向处理。

**建议路线**（不是抄 PDF，而是统一转"文本/HTML 中间格式"）：
- docx / pptx / xlsx → 用已规划的 `markitdown-rs`（我们自己的项目，4 平台二进制已发布）转成 Markdown/HTML，前端复用现有 `MarkdownPreview` + `CodeView` 渲染。对 agent 场景比 PDF 更合适：文本可进上下文、可搜索、可高亮、可复制。
- PDF 保留 `<embed>` 但加降级：内建插件不可用时提示"用默认程序打开"，而不是白屏。
- 表格加虚拟滚动，与 `CodeView` 同一套做法。

## 4. 落地顺序建议

1. **spill 策略**（小成本，直接服务零膨胀铁律）
2. **markitdown-rs 接管 Office 预览**（治掉外部依赖 + 打通 agent 读文档）
3. **tools/execute deadline 强杀**（小成本，防卡死）
4. **附件内容寻址 + 大图降级**
5. **PTC / Code Mode**（大工程，放到前四项之后单独立项）

## 4.1 落地进度

| # | 机制 | 状态 | 落点 |
|---|---|---|---|
| 1 | spill | ✅ 已落地 | `crates/hermix-agent/src/spill.rs`（阈值 20K 字符，按 `tool_call_id` 命名，写盘失败退化为截断）；`budget::enforce_budget_with_spill` 在截断前先落盘；`run_loop.rs` 惰性构造 sink |
| 4 | deadline 强杀 | ✅ 已落地 | `crates/hermix-core/src/win_job.rs`（Job Object + `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`）；`LocalShell::exec` 超时分支调 `terminate()` 杀整棵树。此前超时只 drop future，`cmd /C` 派生的孙进程变孤儿 |
| 2 | document 统一中间格式 | ✅ 部分落地 | 新增 `document_to_markdown` 命令（`src-tauri/src/markitdown.rs`），Office/PDF/HTML/EPUB/CSV… 一律转 Markdown；预览面板工具栏可切「文本视图」，且 officecli 缺失时自动降级，不再白屏 |
| 8 | 附件内容寻址 | ⏸ 未动 | 前端目前没有消费 `ToolAttachment`，先补齐消费链路再谈寻址 |
| 5 | PTC / Code Mode | ⏸ 未动 | 需 `Task` 7 项里的其它机制先收敛 |

## 5. 参考链接

- 架构：<https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md>
- 能力接缝：<https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/capability-seams.md>
- Cordis 论文：arXiv:2608.25512
