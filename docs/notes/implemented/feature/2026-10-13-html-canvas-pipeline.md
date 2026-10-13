# Decision Note: 文档画布管线——HTML 流式画布为富文档形态，导出走旁路

Status: implemented — 画布链路已落地（marker 分流 / 流式画布 / 自动开面板），导出旁路与 skill 指引待后续提交

## Decision

文档编制采用 **"HTML 流式画布 + 导出旁路"** 架构：LLM 在对话流中输出
`<hermix-canvas page="slide16x9|a4">` marker 包裹的结构化 HTML 块，右侧
面板的文档画布随流实时"长"出页面；PDF/docx/xlsx 导出是画布之上的旁路
动作，不在生成链路上。

已交付的部分（2026-10-13，commits 7cde58d / 28e85e8 / 2911882）：

- **分流协议**（`packages/preview/src/lib/docCanvasStore.ts`）：
  `feed()` 返回"本次应进气泡的文本"，不变量 = tail 里的字符尚未进过
  气泡也尚未上屏，每个字符恰好落在气泡或画布之一；块级缓冲只在完整
  块边界（slide → `</slide>`，a4 → `</section>`）上屏；marker 与
  close tag 均可在任意字符处切断（跨 chunk 拼合，逐字符切分单测验证）。
- **流式画布**（`packages/preview/src/viewers/DocCanvasView.tsx`）：
  iframe srcDoc 常驻渲染脚本，块经 postMessage 直达 DOM 追加，**delta
  不经 React**；slide16x9（1280×720 横版幻灯片）/ a4（794×1123 竖版
  文档）双几何由 marker 声明，渲染层不猜；iframe onLoad 重放
  `store.getBlocks()` 补"文档解析慢于流"的竞态；编辑态块排队、退出
  回传全量 HTML（导出旁路的消费点）。
- **自动开面板**（`App.tsx` + `PreviewBar.tsx`）：订阅 store 元信息，
  geometry 从 null 变有值（marker 首次命中）即 rAF 延帧打开单例画布
  tab 并激活右栏，打开动作不打断对话气泡的追加动画。

**流畅性铁律**（主人 2026-10-13 拍板的硬约束，验收即标准）：任何卡顿
都破坏用起来的感觉。工程化为主线程零 long task（>50ms 打回）、滚动零
重排、渲染纯追加。落点：delta 不经 React；元信息微任务合并发射（一帧
多块只触发一次壳层渲染）；旧块 DOM 永不改写。

**页面几何拍板**：PPT/幻灯片默认横版 16:9，不存在竖版 A4 中间态；
几何由 skill 层按指令类型声明（PPT → slide16x9 横向翻页；报告 → a4
纵向滚动），渲染层不得自行猜测或让用户"切换"。

PDF 导出两档：Phase 1 用 `window.print()` + 打印 CSS（零依赖，今天
可用）；Phase 2 在 src-tauri 加 ~100 行 Rust 命令绕过 wry 直接调
webview2-com 的 `ICoreWebView2_16::PrintToPdf`（绑定已确认存在于
webview2-com 0.39，`callback.rs:278-282`；wry 0.57 自身只包了
`window.print()`，`webview2/mod.rs:1796-1798`），静默出 PDF 到指定路径。

docx 导出两档：默认前端 `docx` npm 库（"干净不花哨"：结构、标题层级、
表格全对，无精致排版）；用户明确要正式 Word 文档时才走重转换
（pandoc / LibreOffice headless），慢但保真，不进默认链路。

## Alternatives considered

- **自建 Office 生成（officecli 路线复辟 / rust_xlsxwriter + docx-rs）**
  —否决。主人已实测删除过 officecli（"自建的非常垃圾，完全达不到
  需求"）；根因不是"用了库"，而是缺少质量闭环：LLM 控制低层文档
  结构 = 垃圾产物。唯一例外是 xlsx 数据表（结构化数据非排版容器，
  前端 ExcelJS 已在预览链路，导出是同库反向用）——保留在旁路方案里。
- **移植 Anthropic 官方 skills（docx-js/pptxgenjs/openpyxl + LibreOffice
  渲染回看）** —推迟。质量闭环设计优秀（渲染→看图→回炉），但要拖
  LibreOffice + Node 脚本全家桶进默认链路，与"轻量快速"诉求冲突；
  其核心思想（生成后必须回看）由 impeccable detect 门禁 + 画布所见
  即产物部分继承。
- **COM 直驱真 Office（offipy / ai-office-mcp 路线）** —否决为默认
  链路。保真度最高但重（会话管理、Office 进程生命周期），且把 Office
  变成硬依赖；hermix 是便携零安装产品，不成立。未来若用户要"可编辑
  pptx"再评估。
- **新增 Rust 事件类型（StreamEvent::DocDelta）后端分流** —否决。
  语义干净但要动 transport 枚举 + ws 分发 + 前端 parser 三层；前端
  marker 分流零协议改动即达成同等效果（侵入面：1 个 hook 分支 +
  1 个组件）。
- **纯工具文件方案（LLM 只 write_file html，画布读文件）** —否决。
  没有流式感（等文件写完才能看），且首屏延迟与"卡顿破坏感觉"直接
  冲突。

## Consequences

- 画布内容由 LLM 遵守 marker 协议保证——skill 指引（待提交）是协议
  生效的前提；LLM 不守协议时画布不开启，降级为今天的代码块渲染，零
  损失但不享受新体验。
- pptx 导出**明确不承诺**。行业事实：高保真 pptx 程序化生成没有好的
  开源方案（GenOffice 实测亦然——转换层扎实但"一句话生成"风格漂移）。
  幻灯片交付形态 = 画布 HTML / 打印 PDF；要可编辑 pptx 是未来独立
  评估项（GenOffice 转换层或 COM）。
- 画布是易逝视图：一份回复至多一个 canvas（协议硬规则），新画布开启
  旧份被覆盖；持久产物靠 write_file 落盘（skill 指引会要求成稿同时
  落盘）。编辑回传是全量 HTML 而非增量 diff——画布与文件内容的一致
  性由后续的导出旁路负责，当前编辑结果不落盘。
- tsc --noEmit 因执行限制未在本次提交链中跑通（vitest 全量 308/308
  通过）；补跑是后续提交的前置项。
- 导出旁路（PDF/docx/xlsx 三键 + skill 指引 + impeccable 门禁接入
  画布完成事件）尚未实现，是下一个提交批次的内容。
