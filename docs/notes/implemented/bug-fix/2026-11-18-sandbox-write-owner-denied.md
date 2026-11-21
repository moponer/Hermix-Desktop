# 沙箱建不起来时必须降级执行，而不是把 terminal 工具判死

**日期**: 2026-11-18（2026-11-19 更新）
**类目**: bug-fix
**影响**: `crates/hermix-environments/src/sandbox.rs`、`crates/hermix-environments/src/local.rs`

## 症状

用户在 `F:\cleanking` 上让 agent 编译一个 Go 项目，`terminal` 工具**每一条命令**
都失败；后来（2026-11-19）沙箱放行之后，每条命令又在桌面上**弹出可见的
PowerShell 窗口**。用户对照 CodeBuddy / DSH："人家的都在自己的沙箱内跑、不弹窗"。

## 根因（三层，逐层揭开）

**① provision 改不动目录安全属性（Win32 error 5）。** `grant_write` 原来把
DACL 与强制标签合成一次 `SetNamedSecurityInfoW`：改 DACL 需要 WRITE_DAC，
改强制标签还需要 WRITE_OWNER；普通工程目录 `Authenticated Users: Modify`
缺 WRITE_OWNER，合成调用没有"部分成功"，整步失败。本机对照：
`icacls /grant` 成功、`icacls /setintegritylevel` → Access is denied。

**② 受限进程创建缺特权（Win32 error 1314）。** 修好 ① 后暴露：
`CreateProcessAsUserW` 报 ERROR_PRIVILEGE_NOT_HELD。已排除 token 编辑与
父进程完整性级别的因素 —— 本机从**未**成功过沙箱
（AGENTS.md 记录的"环境性测试失败"就是它）。

**③ 降级后走 ConPTY 路径弹可见窗口。** GUI 进程（windows_subsystem=windows）
spawn 控制台程序不设 CREATE_NO_WINDOW 时，Windows 给它新建**可见**控制台。
ConPTY 分支没有这个标志 —— 这就是弹窗的来源。

**④（发现即搁置）AppContainer 通道堆损坏。** 2026-11-19 曾把 hermix-core
appcontainer（CreateProcessW + SECURITY_CAPABILITIES，普通用户无特权）接为
第二通道，结果 Medium 完整性父进程下同样触发 STATUS_HEAP_CORRUPTION
**进程级崩溃** —— FFI 层既有缺陷（嫌疑：StartupInfoExW / attribute list 的
手工布局），execute_code 的"失败回退"接不住它。已从路由撤下，仅在
`appcontainer.rs` 留 KNOWN BUG 注释。

## Decision

1. **`grant_write` 拆成两次落盘**，返回「Low 标签是否真的生效」；token 完整性
   级别跟随标签结果配对（Low 目录 + Medium 进程可写，Medium 目录 + Low 进程
   写不进，必须一致）。安全边界在受限 token 的 restricting SID 列表。
2. **沙箱建不起来 → 降级执行 + stderr 显式告知**：`[sandbox] 本机无法为 <dir>
   建立沙箱（原因），这条命令以非受限方式执行。` 命令自身失败绝不降级掩盖。
3. **降级路径绕开 ConPTY**，落到 std::process + CREATE_NO_WINDOW + 管道：
   无弹窗、输出可收集。AppContainer 通道撤销，待 FFI 缺陷修复后再接。

## Alternatives considered

- **fail-closed（原行为）**：沙箱恒失败的机器上等于把终端工具报废，用户被迫
  绕过工具，隔离收益归零。否决。
- **AppContainer 第二通道**：理论上是无特权沙箱的正解，实测堆崩溃。待修。
- **要求管理员运行**：与"便携、零安装、普通用户可跑"冲突。否决。

## Consequences

- 沙箱可用机器：行为不变；不可用机器：终端可用、无弹窗、stderr 带降级说明。
- `hermix-environments` 本机测试：83 passed / 5 failed → **89 passed / 0 failed**
  （2026-11-19）。
- 遗留：② 1314 与 ④ 堆损坏都要在沙箱机制层收敛（AppContainer 修复后把降级
  变回罕见分支）。**execute_code 的 AppContainer 路径在本机同样会崩进程**，
  是同一颗雷。
- 测试污染教训：沙箱测试的 `grant_write` 会给 tempfile 工作区留下 Low 标签
  与 deny ACE，测试目录复用会让后续测试假红（`hermix_ps_route_alias` 实况，
  删目录即恢复）。
