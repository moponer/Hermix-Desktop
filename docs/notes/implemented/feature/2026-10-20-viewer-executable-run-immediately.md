# Decision Note: 可执行文件在预览卡上给「立即运行」（三道信任边界）

Status: implemented — 不可预览的可执行文件（exe / msi / com / scr / pif）在元信息卡上说清「二进制可执行文件不可预览，点击「立即运行」执行」，并给一个可点的运行入口；判定权在 Rust，执行前必弹原生确认框。

关联：[hermix-viewer 双模式化与全能力重构](../feature/2026-10-19-hermix-viewer-overhaul.md)、[viewer 开放能力 —— 自实现规范兼容的 MCP 服务](../feature/2026-10-20-viewer-open-capabilities-mcp.md)

## Decision

**1. 扩展名白名单是信任边界，判定权只在 Rust 一侧**（`apps/viewer/src-tauri/src/commands.rs::EXECUTABLE_EXT` = `["exe","msi","com","scr","pif"]`）。`viewer_run_executable` 只放行这些扩展名，不能被用来启动任意文件。前端**不自己猜扩展名**——历史上共享预览层与后端的扩展名表分叉过 57 个扩展名，两边各一份表必然重蹈覆辙。

**2. 执行前必弹原生确认框**（`tauri-plugin-dialog` 的 `OkCancelCustom("运行","取消")`），框里显示文件名 + 完整路径 + 风险提示。预览一个陌生 exe 就把它跑起来是最短的误触路径，所以确认必须由用户亲手点，不能静默执行。用户在框里取消**不是错误**（命令返回 `false`）。

**3. 不可预览就不读字节**：可执行扩展名在 `viewer_read_file` 里**提前返回**，连文件内容都不读（大 exe 有几百 MB，读一遍再丢掉是白费 IO 与内存），也不吃大小上限——超大的 exe 同样不可预览，报「文件过大」只会让用户更困惑。

**4. 启动方式走系统 shell，不自己创建子进程**（`commands.rs::shell_open` → `ShellExecuteExW(verb="open")`）。

**这条是被 360 安全卫士教会的**：2026-10-20 实测，第一版用
`Command::new(path).spawn()`（= `CreateProcessW`）启动 notepad.exe，360 的
「恶意行为监控」把 `hermix-viewer.exe` 判成 **「ADVtrojan/GenInjector.E
（木马病毒）」并直接杀掉进程**。程序本身干净，但用户看到的是「我的软件报毒
了」——对一个要分发到用户机器上的便携应用，这是不可接受的。

「程序自己创建子进程」正是行为监控的经典误报点：Explorer 不会用
CreateProcess 启动一个 exe，它交给 shell；所以「让 shell 打开这个文件」既
是**语义正确**（等价于用户在资源管理器里双击），也是**行为同构**（子进程由
explorer.exe 创建，不由 viewer 创建）。同款修正也应用到
`viewer_open_with_system`（原实现 spawn `explorer.exe`，同样是自己创建
子进程）。

**5. 按钮用实心 accent 底，不画成红色。** 真正的安全闸门是那个确认框，不是把按钮画得吓人——红色按钮会把「正常功能」误读成「危险操作」，而用户点下去还会再遇到一次确认，两处都在喊「危险」等于两处都不传达信息。

**6. 措辞分两种**：可执行文件说「二进制可执行文件不可预览，点击「立即运行」执行」，其余类型沿用「不支持的类型（ext）：原因」——可执行文件那句再加「不支持的类型（exe）：」前缀会把重点冲淡。

**7. `.bat` / `.cmd` / `.ps1` 刻意不入白名单**：它们需要 shell 语义（相对路径、`%VAR%`、管道），用 `Command::new` 直接跑会失败或行为诡异；这类文件走「用系统程序打开」。`.dll` / `.sys` 是库不是程序，也不在白名单。

## Alternatives considered

1. **直接双击 exe 就运行，不确认** ——拒绝：这是最短的误触路径。预览是低风险动作，运行未知二进制是高风险动作，两者之间必须有一个人工闸门。
2. **确认框改成 viewer 自己的 HTML 对话框** ——拒绝：原生对话框不依赖 WebView，且视觉上属于系统层，用户更清楚它是「系统在问你」。
3. **在确认框里显示文件哈希 / 签名状态** ——拒绝（当前不必要）：签名校验是另一件事（且需要 `WinVerifyTrust`）。先保证「跑之前必须知道跑的是什么」。
4. **把按钮画成红色警告色** ——拒绝：见 Decision 第 4 条。
5. **允许运行任意扩展名（只是问一次）** ——拒绝：白名单 + 确认框两道闸门都要有；只留确认框的话，一次误点就够。
6. **把「立即运行」也暴露给 MCP** ——拒绝：MCP 模式没有窗口，弹不了原生确认框。见 [开放能力笔记](../feature/2026-10-20-viewer-open-capabilities-mcp.md)。
7. **可执行文件也走「用系统程序打开」兜底** ——拒绝：双击 exe 本来就是运行，用户要的就是这一步；给一个能真正运行的入口比让人自己去资源管理器双击更省事。
8. **用 `Command::new(path).spawn()` 启动 exe** ——**已实施并被实测否决**：360 安全卫士的行为监控把 viewer 判成「ADVtrojan/GenInjector.E（木马病毒）」并杀掉进程（2026-10-20 实测）。改为 `ShellExecuteExW(verb="open")` 交给系统 shell，与资源管理器双击同构。
9. **干脆去掉「立即运行」，只留「用系统程序打开」** ——备选但未采纳：用户明确要的是一个能点的运行入口（原先的诉求就是「提示信息里加一个立即运行」）；而且「用系统程序打开」走的是同一个 shell 路径，两者在行为监控眼里没有差别，去掉按钮并不能降低风险，只损失功能。
10. **给应用做代码签名** ——拒绝（超出当前能力）：签名证书是发行链路的事，不是这个特性的决定；且未签名只是让启发式更容易命中，真正被命中的是「自己创建子进程」这个行为，已从根上改掉。

## Consequences

- **判定权单点**：`EXECUTABLE_EXT` 是 Rust 侧常量；共享预览层（`packages/preview`）只接收后端传来的 `executable` 布尔标志（可选字段），不自己判断。
- **可执行文件的预览内容为空是预期行为**：后端返回 `FileContent::Unsupported { executable: true }`，前端据此渲染运行入口。
- **运行的工作目录 = 文件所在目录**：程序若读同目录的配置文件，从 viewer 的工作目录启动会找不到（双击 exe 时 cwd 就是它所在目录）。`ShellExecuteExW` 用 `lpDirectory` 表达这一点。
- **`viewer_run_executable` 不受文件大小上限约束**（与其它类型不同），但仍受白名单与确认框约束。
- **共享包只加可选字段**（`ReadTextResult.executable?` / `LoadOutcome.executable?` / `PreviewState.executable?` / `FileSource.runExecutable?`），desktop 端零影响。
- **被行为监控拦过的 exe 需要用户手动加信任**：360 已经把 `hermix-viewer.exe` 记过一次风险；即使行为改成走 shell，用户机器上仍可能需要点一次「信任」才不再弹窗。这是「不自己创建子进程」能降低的风险，但消不掉启发式本身。

## Reintroduction conditions

- 若用户明确要求「记住我的选择，不再询问」，则引入持久化白名单（路径 + sha256），并把确认框降级为「已加入信任列表」的提示——**前提是白名单必须用户手动逐条添加**，不能由一次点击自动入列。
- 若需要显示签名与哈希信息，则在确认框里补 `WinVerifyTrust` 与文件哈希，并考虑把「未知签名」标成比「无签名」更重的视觉权重。
