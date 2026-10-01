# Decision Note: H3 持久终端 —— Rust 原生 ConPTY 方案（推翻"Windows 无 PTY"否决）

Status: proposed — 待评审；取代 [2026-10-01-h3-persistent-terminal-rejected](../../archived/architecture/2026-10-01-h3-persistent-terminal-rejected.md)（已归档）的"Windows 无 PTY"结论，该结论有事实错误

## Decision

H3 持久终端**可行且改为 Rust 原生实现**，不走 Node 侧车。核心依据（2026-10-01 核实）：

- **Windows 有 PTY**：ConPTY（Win10 1809+ 内置）。执行 agent 此前否决 H3 的
  理由"Windows 无 PTY / `pty: true` 直接报错"是**事实错误**——那报错是
  hermix 自己还没接 ConPTY，不是 Windows 没有这能力。
- **Rust 生态有生产级现成件**：`portable-pty` 0.9（wezterm 出品，crates.io
  1770 万下载 / 584 个依赖方，持续维护），`native_pty_system()` 在 Windows
  上就是 `ConPtySystem`，Unix 上是标准 PTY——trait 化跨平台，正合 hermix
  Windows-only 现状。六步决策阶梯第 4 步（现有依赖）直接命中，无需自写
  ConPTY FFI，也无需 Node。
- **与 dsh 路线对比**：dsh 是 TS，只能依赖 `node-pty`（native 依赖，每个
  OS 要构建冒烟）。hermix 是 Rust，portable-pty 纯 Rust 集成进
  hermix-tools 的 TerminalBackend，**比 dsh 的路线还干净**——这正是"语言
  不同，抄路线不抄实现"的又一例。

### 实现要点（从 dsh 笔记 + wezterm 已知问题提炼）

1. **会话表**：`session_id → (PtyPair, Child)` 挂在 TerminalBackend；工具参数
   加 `persistent: true` + `session_id`（续用）/缺省（一次性，现行为不变）；
   空闲会话超时回收（对齐 D.4 drain 语义）。
2. **ConPTY 输出必须过滤**（wezterm 官方已知问题）：ConPTY 经 win32 屏幕
   缓冲区重编码输出流，会合成 OSC 标题变更、光标移动等转义序列，噪音远超
   POSIX PTY。需要一层 ANSI/OSC 过滤（`vte` parser 或最小状态机）再回给
   模型——dsh 同样做了 prompt-marker 剥离，这是 ConPTY 路线的固有成本，
   不是可选项。
3. **空闲判定是启发式**（dsh 诚实记录）：Windows 无 /proc，只能 Toolhelp32
   快照 + 输出静默推断，语义标"尽力而为"；总时长预算 + 静默超时兜底。
4. **REPL 场景的 send/read 分离**：`terminal_send`（submit 可控，支持不带回车
   的控制片段）+ 读取沿用现有输出流；不追求全屏 TUI（vim/htop 明确不支持，
   dsh 同样 deferred）。
5. **与 2026-09-26 审批定策正交**：持久会话内的**命令解析审批**沿用现有
   terminal 审批链（每条 send 的命令文本照过 CONFIRM/DENY）；资源门不变。

## Alternatives considered

- **维持否决（不实现 H3）** —— 否决：否决理由的事实基础（"Windows 无
  PTY"）已被证伪；REPL/gdb/venv 跨调用是用户实测"不顺手"的第三大来源。
- **Node 侧车 + node-pty（此前交接词的建议）** —— 否决：核实后放弃。
  portable-pty 使 Rust 侧零原生依赖即可得 ConPTY；侧车方案要自建
  进程池/IPC 协议/审批桥接，为单一功能引一套分布式复杂度，违反阶梯第 3 步
  （原生平台优先于库/旁路）。PTC（F-3）仍走 Node 侧车——那边跑的是 JS
  程序，Node 是必需品而非载体；两者不再捆绑。
- **自写 ConPTY FFI（windows-rs 直接调 CreatePseudoConsole）** —— 否决：
  portable-pty 已封装好 resize/kill/size 跨平台语义与已知坑（如 escape
  合成），自写只多了维护面没有新增能力。
- **全屏 TUI 支持** —— 明确不做：ConPTY 重编码下 vim/htop 的差分渲染对
  LLM 不可读，价值为负（dsh 同样 deferred）。

## Consequences

- 获得：cd/venv/REPL/gdb 状态跨调用存活（H3 原始目标）；比 dsh 少一个
  native npm 依赖；Windows-only 定位下无跨平台负担。
- 放弃：全屏 TUI；精确的进程级空闲判定（启发式 + 超时兜底）。
- 代价：portable-pty 0.9（pre-1.0，但 wezterm 本体即最大消费方， API 稳定
  风险低）；ANSI/OSC 过滤层约 300-500 行 + 测试；总工作量 3-5 天
  （比原估的"从零 ConPTY"低一个量级）。
- 风险：ConPTY 在 Windows Server 旧版本缺 API → 运行时探测降级为一次性
  模式（现有行为），不阻断启动；受限令牌（F-2）与 ConPTY 组合需联测
  （伪控制台内的受限进程行为）。
- **排序建议**：排在 F-2 ACL 沙箱之后、F-3 PTC 之前——沙箱先建立能力
  边界，持久终端在其内运行才不出现"先给自由再收权"的返工。
