# Decision Note: 查看器的 Word / Excel 编辑能力 —— 打开即可就地编辑，暂不内嵌 Craft 办公套件

Status: implemented — 查看器重量模式下 Word / Excel 打开即默认可编辑；开源办公套件（WordCraft / GridCraft / Univer）完成调研，本轮不内嵌。

## Decision

查看器的 Word（.docx）与 Excel（.xlsx/.xlsm）**在重量模式下打开就是可编辑的文档/表格本体**，
不是「先预览、再点一个编辑按钮进入另一种形态」。纯净（轻量）模式仍是只读预览。

两条实现路线各自选了"保真优先"的做法：

- **Word**：用 `docx-preview` 还原版面（与共享预览层同一套渲染选项），每个段落变成
  可编辑块；用户改完后，Rust 侧 `docx_edit.rs` 按**段落索引**把改动回写进 OOXML
  （`word/document.xml`），**只改用户动过的那几段**，其余字节原样搬运。
  跨段落的样式、页眉页脚、图片、编号、纸张设置全部不丢。
  实现依赖 `zip` + `quick-xml`——两者已在工作区 Cargo.lock 里（hermix-tools 的
  docx-rs / rust_xlsxwriter 传递依赖），加进 viewer **不新增任何编译单元**。
- **Excel**：显示走共享层的 `parseXlsx`（ExcelJS 解析，底色/字色/边框/对齐/合并区/
  列宽/行高/公式结果全部还原，与桌面端预览同一条代码路径）；保存时**重新载入原文件**，
  只把用户真正动过的格子写回去再导出——没动过的公式、样式、其它工作表原样保留
  （不是"读成 JSON 再生成"那种全本重建）。

产物一律是新文件：Rust 侧 `safe_output_path` 守卫（与源文件相同、含大小写变体与
硬链接，一律拒绝），默认落 `<原名>.edited.<扩展名>`。

**本轮不内嵌开源办公套件**。调研结论记录在下面，路线留作后续里程碑：
现在只做「检测到本机装了 WordCraft / GridCraft 就一键交给它打开」的入口。

## Alternatives considered

- **Univer（dream-num/univer，Apache-2.0，v1.0.0 于 2026-09-24 发布）** —— 否决。
  开源核心覆盖表格与文档编辑器，但 **xlsx / docx 的导入导出属于 Univer Pro**
  （`@univerjs-pro/sheets-exchange-client` + license key，Pro 面在无 license 时
  水印或限制操作）。也就是说开源版拿不到「打开真实 .xlsx / .docx 并写回」这条
  最基础的能力，与本项目的诉求正好错位。

- **WordCraft / GridCraft（storytold，MIT OR Apache-2.0，纯 Rust）内嵌 WASM 版** ——
  **暂缓，但这是最对味的一条路**。硬事实：两者都是 clean-room 重写，
  WordCraft 主打 .docx 原生读写，GridCraft 把 .xlsx 当**原生格式**
  （样式/主题/公式/条件格式/透视表/图表全部 round-trip）；无遥测、无订阅、
  无 license server，正合「安全隐私、速度性能一流」。实测代价：
  GridCraft 网页版解压 18.27 MB、WordCraft 40.39 MB（两者合计 +58.7 MB，
  而查看器现发布 exe 26.9 MB）；若自编还要 vendor 13.7 MB / 35.7 万行第三方
  Rust 源码进仓，外加 trunk + wasm32 构建链（WordCraft 还要把工具链升到
  Rust 1.95 并联网拉 craft-libs 的 git 依赖）。
  **可行的省力做法**（下一轮候选）：直接内嵌官方**预编译**产物，不自编、
  不 vendor 源码，把产物放 `public/office/<app>/` 由 Vite 原样拷进 dist，
  用相对路径 iframe 引入 → 同源成立 → GridCraft 现成的 `?host=parent`
  postMessage 桥（`gridcraft:open` 收字节 / `gridcraft:save` 交回字节）直接可用，
  零构建链。**WordCraft 的网页版没有这个桥**（`apps/wordcraft-web/src/web/`
  下只有 `font_store.rs`），要内嵌就得给它补一个，等于维护 fork。
  另有一条硬约束：两个网页版都要求 WebGPU 或 WebGL2，Tauri 的 WebView2
  默认不开 WebGPU，实际会退回 WebGL2，渲染路径与性能需实测。

- **docx-preview → HTML → docx 全篇重建** —— 否决。整篇重新生成会丢样式、
  图片、页眉页脚、编号与纸张设置；「只改用户动过的段落、其余字节原样搬运」
  保真度完全不同。

- **服务器型 OnlyOffice Docs / Collabora Online** —— 否决（与本地优先冲突）。
  文档要送上服务端，与「安全隐私一流、本地优先」的红线冲突，且引入常驻服务。

- **HermesOffice（Apache-2.0）** —— 参考其路线而非直接采用：它对 docx 走的也是
  字节级段落改写，与本方案同源；它的表格用 Univer（受 Pro 限制）。

## Consequences

- Word 侧是**段落级**编辑：被改写段落里的**段内局部混排**（"一句话中间加粗"）
  会抹平成该段首个文本 run 的统一格式；段与段之间互不影响。
  段落**新增/删除**暂不支持（回退 Enter 会被拦下并提示）——要支持得改写
  `document.xml` 的结构，超出当前能力边界。
- Excel 侧没有公式引擎：公式格显示的是**上次计算的结果**，编辑后按新值写成
  公式或常量，但不重算依赖链；透视表、图表、宏不支持。
- 这条路线**不引入任何第三方产物**：查看器包体积与构建链不变，零新增编译单元。
- 与 Craft 系列的互操作留了口子：`viewer_open_with` 的 `wordcraft` 分支 +
  `viewer_wordcraft_path` 探测命令——装了就在文档工具条上给一键入口，
  用真正的 Office 引擎编辑当前文档（修订、目录、透视表、图表全都有）。
- 代价是「真正的 Office 能力」暂时要靠外部应用兜底；内嵌路线（上面第 2 条
  的省力做法）是下一轮的候选，需要单独评估体积与 alpha 质量。
