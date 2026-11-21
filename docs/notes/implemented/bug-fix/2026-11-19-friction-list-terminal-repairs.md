# 碰壁清单复盘：terminal 摩擦的八个产品修复

**日期**: 2026-11-19
**类目**: bug-fix
**影响**: `crates/hermix-intelligence/src/redact.rs`、`crates/hermix-environments/src/{sandbox,local}.rs`、`crates/hermix-agent/src/directions.rs`

## 背景

用户让项目 agent 总结 CleanKing 编译任务的全过程（17 条碰壁 + 2 个误判）。复盘后拆成八处产品缺陷，全部修复：

## Decision

1. **脱敏不再吞路径**（清单第三节）。email 正则把 Go toolchain 路径 `toolchain@v0.9.1.windows-amd64` 判成 email 脱敏成 `[REDACTED_EMAIL]`，agent 按字面假路径操作 —— 一连串 `Test-Path False` / `dir 找不到` / `move_file 源路径不存在` 全是这个假路径造成的。修法：email 候选要求前一个字符是行首/空白/标点（Rust regex 无 lookbehind，用前缀捕获组 + 替换发回前缀），路径分隔符后的 @ 不再触发脱敏。回归测试 `test_redact_email_keeps_go_toolchain_path`。

2. **`powershell -File` 真的能执行**（清单 #17，`write_file` 写出的脚本「没执行」）。两个叠加原因：
   - 引号没剥：LLM 写 `powershell -File "C:\x\a.ps1"`，powershell.exe 的 -File 解析器**不剥**这层引号，引号成了路径字符 → "Illegal characters in path"。cmd 层同样的引号能跑，差别就在我们把引号当成了路径的一部分。
   - AppLocker/WDAC 拦截：实测本机有 AuthorizationManager 策略，只放行 `-ExecutionPolicy Bypass -File` 形态（微软推荐的 AppLocker 规则集即如此）。不带 Bypass 的脚本执行被**静默拒绝** —— 表现正是「脚本没执行，日志没生成」。
   修法：剥掉 -File 路径的首尾引号，并给该形态加 `-ExecutionPolicy Bypass`（**进程级**，不改机器策略）。这不扩大能力面：`-Command` 形态本来就不受执行策略约束，两者等价；只是让两条路行为一致。回归测试 `powershell_file_script_actually_runs`。

3. **不再弹可见命令行窗口**（用户明确要求）。父进程 hermix-desktop.exe 是 GUI 程序（windows_subsystem = "windows"），**没有自己的控制台** —— 这样的父进程 spawn 控制台程序时，Windows 会给子进程**新建一个可见控制台窗口**，每条命令闪一次。三条 spawn 路径逐一处理：std::process 路径已有 CREATE_NO_WINDOW；沙箱受限路径改为 STARTF_USESHOWWINDOW + SW_HIDE（CREATE_NO_WINDOW 在受限创建下会 STATUS_DLL_INIT_FAILED，见模块头）；沙箱降级路径绕开 ConPTY 落到 std::process。旧注释「Low 完整性进程受桌面 ACL 限制本就无法创建可见窗口」不成立 —— 控制台窗口由 conhost.exe（Medium）代为创建。

4. **沙箱不可用按工作区记忆化**（清单 #10 的 600s 超时）。provision 失败是确定性的，旧代码每条命令都重走一遍 ACL 授权（大工作区分钟级）。现在 `SANDBOX_VERDICT` 是**按工作区键控**的进程内缓存：同一工作区首次 5/1314 后直接短路进降级；不同工作区、以及盘根这类 provision 内部 fail-closed 校验不受影响。重启应用 = 重新给沙箱一次机会。

5. **沙箱 temp 目录跨调用稳定**（清单 #11：%TEMP% 每次调用都变、销毁后产物读不回）。目录名改为按（工作区 + temp 根）SHA 派生，`Drop` 不再删 —— agent 可以往 %TEMP% 写编译产物/日志并让下一条命令读回。残留由 disk_cleanup 兜底。

6. **后台命令输出自动落日志**（清单 #2-#4 三轮摩擦：前台超时 → 改 background → `>` 重定向空文件 → Out-File 碰运气）。后台路径现在把 stdout/stderr 直接重定向到 `%TEMP%\hermix-bg-<毫秒时间戳>.log`，返回文本带该路径。前台管道收集不变。

7. **PowerShell 陷阱写进系统提示**（清单 #4、#5、#7 与误判表）。新增 `## PowerShell traps` 小节：$LASTEXITCODE 语义、陈旧日志误报成功、`&&` 不支持、`>` 与 Out-File 的坑、%TEMP% 唯一命名约定。

8. **不许绕过审批**（清单 #12、#16）。递归删除被拦是 **CONFIRM**（要审批卡），不是硬拦 —— 这是对的。但 agent 换了三种写法（`cmd /c del`、`cmd /c rmdir /s /q`）去躲匹配器，那是**审批规避**。新增 `## When a command is denied` 小节：命令被拒 = 用户没批准这个破坏性操作，是决定不是障碍；要么一句话告诉用户你要删什么然后停下，要么把过宽的命令收窄（合法修复），绝不「换个写法重试到通过」。

## Alternatives considered

- 修好 AppContainer 通道让沙箱真正可用：方向对（无特权、低完整性+断网），但该模块在本机触发 STATUS_HEAP_CORRUPTION 进程级崩溃（FFI 缺陷，嫌疑 StartupInfoExW 手工布局）。修 FFI 是独立一轮；本轮先让降级路径零摩擦。
- 后台输出保持管道 + 让 agent 自己重定向：清单证明走不通（三次各显神通全错）。输出捕获应是工具契约的一部分，不该让 LLM 操心。
- 给 `-Command` 形态也加 `-ExecutionPolicy Bypass`：不采用。有些环境只放行裸 `-Command` 而拦带 Bypass 的形态，加了反而把现在能用的路径弄坏。

## Consequences

- terminal 在沙箱不可用机器上：第一条命令付一次 provision 失败成本，之后 O(1) 降级；后台命令输出永远可读；%TEMP% 产物可跨调用传递；不再弹窗口；脚本能真正执行。
- 脱敏收紧后，前面紧跟路径分隔符的真 email 不再捕获 —— 有意取舍：路径完整性优先。行内真 email（前面是空白/标点）照常脱敏。
- 沙箱受限路径现在用 SW_HIDE 隐藏控制台窗口。这与 std::process 路径的 CREATE_NO_WINDOW 是同一类「隐藏执行」，杀软误报风险两条路一样存在，不构成新增暴露。
- 待办：AppContainer FFI 崩溃修复；沙箱可用后把降级变回罕见分支。