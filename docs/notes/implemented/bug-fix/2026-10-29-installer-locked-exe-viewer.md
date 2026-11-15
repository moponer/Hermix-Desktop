# Decision Note: 安装器「无法打开要写入的文件」——查看器锁着 hermix-viewer.exe

Status: implemented — 2026-10-29 交付：NSIS 钩子在写入文件前先收主程序与它的伴生进程（查看器 / AI 内核 / 文档转换 / 索引助手 / 外壳加载器），卸载走同一条路径

## Decision

现场症状：升级安装（Tauri NSIS）走到解压阶段弹

```
无法打开要写入的文件：
"D:\gacc\Hermix Desktop\hermix-viewer.exe"
[Abort] 停止安装 / [Retry] 重试 / [Ignore] 忽略
```

根因不是权限、不是杀软，是**文件被运行中的进程锁着**：

- 查看器是**独立进程**，主程序退出后按设计继续运行（
  `apps/desktop-tauri/src-tauri/src/shutdown.rs` 顶部明确「不杀 `hermix-viewer.exe`」，
  用户关掉主程序后继续看文件是正常用法）。于是「用过查看器 → 关主程序 → 装新版」
  这条最常见的升级路径上，`hermix-viewer.exe` 一定被占用。
- 模板自带的 `CheckIfAppIsRunning` 只检查 `${MAINBINARYNAME}.exe` 一个名字
  （`target/release/nsis/x64/utils.nsh:22`），`externalBin` 里那批伴生 exe
  完全不在它的视野内 —— 主程序关了它就认为「没在运行」。

修法：`installer/hooks.nsh` 的 `NSIS_HOOK_PREINSTALL`（卸载用
`NSIS_HOOK_PREUNINSTALL`）插入 `HermixStopLockingProcesses`，**在 `File` 覆盖之前**：

1. `hermix-desktop.exe` 在跑 → 交互模式弹一次确认（取消 = `Quit`，不留拆一半的
   中间态），`/S` 与 `/P` 没人可问直接关；`KillProcessCurrentUser` 后 `Sleep 800`
   等 Job Object 收树（`kill-on-close`，sidecar 随主进程一棵树死）。
2. 再逐个收伴生进程：`hermix-filesearch.exe`（先按名杀；提权进程会被「拒绝访问」
   挡回来，于是退回它自带的 `--stop` 走回环 `shutdown` 协议，刷盘后自己退出，
   再杀一次）、`hermix-viewer.exe`、`hermix.exe`、`markitdown.exe`、`hx-shell.exe`。
   每次先 `FindProcessCurrentUser` 再 `KillProcessCurrentUser`，没在跑就零动作。

**顺序不能反**：先关主程序再关伴生进程。反过来先杀 sidecar，主程序会留下
「界面还在、后端没了」的半死状态（若用户在确认框里点了取消，这个状态就留给了用户）。

**这段逻辑只能写成宏，不能写成 Function**：模板在 `!include hooks.nsh`（第 28 行）
**之后**的第 87 行才 `!addplugindir` 加 `nsis_tauri_utils` 插件目录，函数体在
include 处立即汇编，那时插件还不可见，直接报
`Plugin not found, cannot call nsis_tauri_utils::FindProcessCurrentUser`。
宏只在 Section 里展开（第 611 / 747 行），插件早已可见。
标签用调用方传入的 TAG（`pre_` / `un_` 前缀）保证两次插入不撞名。

## Alternatives considered

- **让查看器随主程序退出**：否。产品语义是「关了主程序还能继续看文件」，
  安装器没有权力改这个行为的边界。
- **依赖模板的 `CheckIfAppIsRunning`**：否。它按单一名匹配，伴生 exe 永远漏。
- **把逻辑写成 Function 并在里面调插件**：编译即失败（见上，插件目录顺序）。
- **无条件杀所有伴生进程（不区分主程序是否在跑）**：不做。主程序在跑时先杀
  sidecar，会让「用户点了取消」这条路把界面留在半死状态。
- **让用户选「忽略」继续装**：等于把旧 exe 留在新版本旁边，用户之后打开的是
  几周前的界面且没有任何提示（与 `scripts/dev.mjs` 里同款坑的注释一致）。

## Consequences

- 安装/卸载会**静默关闭**用户正开着的查看器窗口（不弹窗）；主程序在运行时才弹
  一次确认。查看器没有未保存状态，代价可接受。
- 匹配口径是**进程名**（与模板对主程序的做法一致）：机器上另一份 Hermix 的
  `hermix.exe` 也会被一起关掉。路径级匹配在 NSIS 里要自己枚举进程模块路径，
  不值得。
- 旧版 `hermix-filesearch.exe` 没有 `--stop`（只有包装了才知道），此时那条命令
  会短暂起一个实例，随后的按名杀会把它收掉；新版无此问题。
- **仍未覆盖**：以管理员身份启动过 Hermix 的机器（`currentUser` 安装包的
  `RequestExecutionLevel user`，见生成的 `installer.nsi:100`）。提权进程的
  `hermix-viewer.exe` 普通用户杀不掉，`File` 仍会弹同一个框 —— 这类只能人工先关
  查看器。要根治得改安装模式（`perMachine`，每台机一次 elevation），与「便携、
  免管理员」的产品前提冲突，不做。
- 修复只在**新构建的安装包**里：已发出去的旧 exe 装的还是旧钩子。
- 验证方式：用 tauri 自带的 `makensis` 直接重编已生成的
  `target/release/nsis/x64/installer.nsi`（钩子是按绝对路径 include 的，改完即生效），
  并用一个探针脚本确认插件语义 `FindProcessCurrentUser` 找到=0 / 未找到=1、
  `KillProcessCurrentUser` 成功=0。**完整安装流程本身没有在本机跑**（会真装一遍），
  首次发版需人工验证一次升级安装。
