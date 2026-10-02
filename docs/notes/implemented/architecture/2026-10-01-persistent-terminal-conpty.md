# Decision Note: H3 持久终端 —— Rust 原生 ConPTY 实现

Status: implemented — 2026-10-02 交付；cd/venv/REPL 状态跨调用存活，真机 ConPTY 探针与集成测试全绿

## Decision

H3 持久终端**用 `portable-pty` 0.9（wezterm 出品）原生实现**，取代
[archived 的"Windows 无 PTY"否决](../../archived/architecture/2026-10-01-h3-persistent-terminal-rejected.md)
（该否决的事实基础有误：`pty: true` 报错是 hermix 未接 ConPTY，
Windows 自 Win10 1809 起内置 ConPTY）。纯 Rust 集成，无需自写
ConPTY FFI、无需 Node 侧车。

交付面：

- **会话表**：`session_id → (master/writer/child, 过滤后文本缓冲)`，
  挂在 `LocalBackend.pty_sessions`（`PtySessionRegistry`），经
  `TerminalBackend` 的五个 `pty_*` 方法（带 default 实现，不支持的
  backend 零改动）暴露。
- **工具参数**：`terminal` 加 `persistent`（建会话）/ `session_id`
  （续用）/ `submit`（是否补回车，false 走 REPL 续行/多行粘贴）。
  空 command + `submit: false` = 纯读轮询。缺省行为一字未变。
- **输出降噪**：`pty_filter.rs` 剥离 ConPTY 经屏幕缓冲区重编码合成的
  OSC 标题 / SGR / 光标等序列，CRLF 归一，UTF-8 跨块续接。
- **`pty: true` 一次性路径**：Windows 上改走同一个 ConPTY 通道，
  旧的"PTY not supported"报错消失。
- **审批正交**：每条 send 的命令文本走 `terminal` 原有的同一段审批
  门（无第二条路径），与 2026-09-26 定策不冲突。

## 真机探针钉死的实现前提（`tests/pty_conpty_probe.rs`）

1. **DSR 握手**：ConPTY 启动即发 `ESC[6n`（光标位置请求），必须在
   输入管道回复 `ESC[1;1R`，否则 conhost 永不渲染——cmd 与
   PowerShell 全部静默卡死。读线程内建自动应答。
2. **会话 shell 形态**：`powershell -NoLogo -NoProfile -NonInteractive
   -NoExit -Command "function prompt { '<marker>> ' }"`。
   `-NonInteractive` 是必需项：默认交互态的 PSReadLine 逐字符语法
   高亮回显（`ESC[93me` / 退格 / 光标闪烁）噪音不可用。marker
   提示符在 shell 就绪时自渲染、每条命令后重现——既作就绪信号，
   也作逐命令完成信号（剥离后不出现在模型可见文本里）。
3. **输出流恒 UTF-8**（子进程 OEM/GBK 字节先被 conhost 解码再重编码）。
4. **关闭顺序**：kill → 等退出 → 排干 → drop master；直接 drop
   活着的 pseudoconsole 会挂死（ClosePseudoConsole 阻塞）。

## Alternatives considered

- **维持否决（不实现 H3）** —— 否决：否决理由的事实基础已被证伪。
- **Node 侧车 + node-pty** —— 否决：portable-pty 使 Rust 侧零原生
  依赖即可得 ConPTY；PTC（F-3）仍走 Node 侧车（跑的是 JS 程序），
  两者不捆绑。
- **自写 ConPTY FFI** —— 否决：portable-pty 已封装 resize/kill/跨平台
  语义与已知坑，自写只增维护面。
- **全屏 TUI 支持（vim/htop）** —— 不做：重编码差分渲染对 LLM 不可读。
- **独立的 `terminal_send` 工具**（原提案）—— 实现期收敛为
  `terminal` 的 `submit` 参数：功能等价，但审批门天然复用同一段
  契约代码（H2 教训：同一契约不开第二条路径），少一个工具 schema。
- **用默认 `PS C:\...>` 提示符做就绪信号** —— 否决：形态随 cwd/语言
  变化，marker 是会话私有的确定 token。

## Consequences

- **获得**：cd/激活 venv/REPL/gdb 状态跨调用存活（`terminal` 一个
  工具内）；`pty: true` 在 Windows 真实可用；比 dsh 少一个 native
  npm 依赖。
- **放弃**：全屏 TUI；进程级精确空闲判定（Windows 无 /proc）。
- **代价**：`portable-pty` 0.9（pre-1.0，但 wezterm 本体即最大消费
  方）；过滤层 + 会话表约 700 行；会话上限 8、空闲 30 分钟回收、
  静默 1.5s/5s 两档启发式（常量，未入配置）。
- **风险**：ConPTY 缺失（Win10 1809 前）时 `create` 明确报错降级为
  一次性模式，不阻断启动；**pty/持久会话与 background 同样不走
  Windows ACL 沙箱**（安全语义由审批层承接），与 F-2 沙箱的组合
  需后续联测。
- **排序**：F-2 沙箱之后、F-3 PTC 之前。
