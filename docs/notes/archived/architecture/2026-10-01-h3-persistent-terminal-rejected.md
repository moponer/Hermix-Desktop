# Decision Note: K 批 H3（持久终端会话）在本项目不可实施

Status: rejected — 2026-10-01 核实后否决，理由记录在案
Archived: 2026-10-01

> 取代者：[2026-10-01-persistent-terminal-conpty](../../proposed/architecture/2026-10-01-persistent-terminal-conpty.md) —— 本文"能力前提不存在 / 平台限制"的结论有事实错误：Windows 自 Win10 1809 起内置 ConPTY，`portable-pty` 已封装为生产级 Rust 现成件。本文的其余论证（审批链、测试覆盖、诚实失败语义）已由取代者吸收。

## Decision

**不做 H3「持久终端会话」**（提案 §批次 H 的 H3 项 / §批次 K 的实现清单）。
H1、H2、H4、H5、H6 已全部实施（见 [[2026-09-30-doc-cleanup-and-optimization-plan]]）。

否决理由不是工作量，而是**能力前提不存在**。

## Alternatives considered

- **引入 `portable-pty` / `conpty` 依赖并实现 ConPTY** —— 否决。理由：
  1. `hermix-tools` 与 `hermix-environments` 当前**没有任何 PTY crate 依赖**
     （`Cargo.toml` 无 `portable-pty` / `conpty`），等于从零引入 Win32
     ConPTY 绑定并自行处理句柄生命周期、读循环、超时回收。
  2. 这不是"对齐一个行为契约"，而是新增一块平台相关子系统。提案
     §H 方针升级明确说"抄行为不抄代码"，但 ConPTY 没有可抄的等价物
     ——dsh 的 `tool-bash-persistent` 底层依赖 Unix PTY 或 macOS
     `ConPTY` 库，与 hermix 的 Windows-only 目标不重合。
  3. 引入后 `pty: true` 的语义会从"直接报错"变成"真的分配伪终端"，
     这是一条新的、无测试覆盖的执行路径，风险与收益不匹配。
- **在 Windows 上用管道模拟（`pty: true` 静默降级为普通管道）** ——
  否决。等于对模型撒谎：它以为拿到了交互式终端（可以跑 REPL / gdb /
  `vim`），实际是单向管道，行为不可预期且不报错。现有实现选择
  **显式报错**（见下）是更诚实的失败语义。
- **仅做"会话表 + cwd/env 状态保存"，不接真 PTY** —— 否决。这能解决
  `cd` 丢失，但 REPL / venv 激活 / gdb 状态仍无法跨调用存活，只是把
  半个问题包装成"持久终端"，反而更容易误导模型。

## Consequences

- **放弃**：模型在 Windows 上无法跨调用保持交互式会话状态
  （REPL、gdb、已激活的 venv）。这确实是"不顺手"的一项，提案判断
  正确 —— 但它是**平台限制**，不是实现缺失。dsh 在这一项上的优势来自
  其 PTY 后端，hermix 的单平台约束决定了无法照搬。
- **保留**：`terminal` 仍有 `pty` 参数，在 Windows 上会给出明确的
  不支持提示并建议改用 `pty: false`（见
  `hermix-environments/src/local.rs:408`）。这个诚实失败是有价值的：
  它告诉模型"这条路走不通，换一条"，而不是让它对着异常输出反复试。
- **不影响的**：`background: true` 的进程管理机制（`process` 工具的
  list / poll / write-stdin / kill）仍然可用，长任务场景可以走后台
  进程而非持久会话。这是当前架构下"多次交互"的既有解法。
- **若将来要做**：应作为独立立项，先写 ConPTY 绑定的技术方案
  （句柄生命周期 / 读循环 / 超时回收 / 与现有 background 进程的
  关系），而不是作为"工具契约对齐"的一个条目。

## 证据

```
crates/hermix-environments/src/local.rs:408
    return Err(AgentError::Io(
        "PTY mode is not supported on Windows. \
         Re-run the command with `pty: false` to use \
         standard pipe-based execution. (Windows \
         pseudo-console support via ConPTY is a future \
         enhancement.)"
```

Unix 分支（`local.rs:341`）用 `script -q /dev/null -c <cmd>` 包装，
该工具在 Windows 不存在——这就是 `pty: true` 在 Windows 直接报错的
根本原因。
