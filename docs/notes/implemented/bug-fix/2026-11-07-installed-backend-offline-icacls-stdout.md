# Decision Note: 安装版启动后「后端未连接」而 hermix.exe 仍在运行

Status: implemented — 2026-11-07 落地

范围：`hermix-config`（`restrict_owner_only`）、桌面端 `stdio_bridge`（stdout 读线程）

## 现象

最新安装包在**另一台机器**装完启动，顶栏恒显示「后端未连接」，但任务管理器里
`hermix.exe` 活着、`desktop.log` 里 server 侧日志（seeded skills / relay 连上 /
LLM warmup）一切正常。本机（开发机）同样的包不掉线。界面上没有任何原因 —— 见
「为什么此前查不到」。

## 根因

`hermix-config/src/loader.rs::restrict_owner_only` 在每次写 `config.yaml` 后调用
`icacls` 收紧 ACL，用的是 `.status()` —— **stdio 全部继承父进程**。中文 Windows 的
icacls 按 ANSI(GBK) + CRLF 打印两行进度：

```
已处理的文件: C:\Users\<用户>\.hermix\config.yaml
已成功处理 1 个文件; 处理 0 个文件时失败
```

这两行落进 sidecar 的 stdout，也就是 `--stdio` 的 JSONL 协议通道。桌面桥用
`BufRead::read_line`（要求整行合法 UTF-8）读帧，首行即 `InvalidData`，读线程
`break` → `mark_dead_and_notify` → `BRIDGE_LIVE=false`。此后：

- 子进程继续活着、继续处理 stdin（REST 请求照常被服务端处理）；
- 桥不再读任何应答 → 所有请求悬挂 → `/health` 探测全失败 → 界面永久「未连接」；
- 看门狗只看 `bridge_ready && child_alive`，`bridge_ready=false` 会触发重启，
  但重启后同样的首次写盘再污染一次，直到 5 次预算耗尽 give-up。

只在**全新安装**触发的原因：`config.yaml` 首次生成（seed 免费模型通道）+ 首次
`/api/config/auto-correct` 都会走 `write_yaml_atomic → restrict_owner_only`；开发机
的 `~/.hermix/config.yaml` 早已存在且 ACL 已收紧，启动路径不写盘，所以干净。
换句话说，任何一次「保存设置」都会在干净机器上复现同一故障。

## Decision

两处都改，缺一不可：

1. **断源**：`restrict_owner_only` 给 icacls 显式 `stdin/stdout/stderr = null`，
   非零退出码升为 warn（原先只看 spawn 失败）。外部命令的输出永远不许进协议通道。
2. **缩小爆炸半径**：桌面桥的读线程从 `read_line` 改为 `read_until(b'\n')` +
   `String::from_utf8_lossy`。协议是「行分隔 JSON」，坏行本该被跳过 —— 此前一个
   非法字节就能永久打死整条通道。跳过时把坏行前 120 字符写进 `desktop.log`。

配套（同一轮）：`stdio_bridge` 的读线程诊断从 `eprintln!` 改为 `desktop_log!`
（GUI 进程 eprintln 无处可去，这次正因如此第一现场完全不可见）；`backend_ready`
的桥死/超时分支补 `note_probe` 落盘；`hermix:backend-error` 增加 `BOOT_ERROR`
回查（事件在 setup 阶段 emit，前端监听要等页面加载完，必然错过）。

## Alternatives considered

- **只改桥（容错）**：能把用户救回来，但每秒都在往日志里灌 mojibake，且任何其他
  继承 stdout 的外部命令都会重现同一幕 —— 不修源头就是留一个再犯条件。
- **只改 icacls**：修掉当前这一处，但桥仍然「一个非法字节即永久离线」。协议边界上
  这是不可接受的脆弱性。
- **在 sidecar 侧把 stdout 重定向到私有句柄**：需要改 Windows 句柄继承语义，
  且第三方子进程仍可能拿到控制台句柄；对「协议要能容忍垃圾行」而言是绕路。

## Consequences

- 全新安装不再出现「进程在、界面未连接」；已在用的机器上「保存设置」也不会再
  静默打死后端。
- `desktop.log` 现在能直接看出桥的死因（reader started / unparseable frame /
  reply without waiter / reader exited + 原因 + 帧数），不必再靠推断。
- 打包必须重编 sidecar（`hermix.exe`）才能带上 icacls 修复；只重编桌面壳只能靠
  桥的容错兜住（会出现可读的 unparseable 日志）。
- 未加自动化回归测试：`ChildStdout` 无法在单测里伪造，icacls 的 stdio 继承也
  只能实机验证。验证手段是「隔离 HERMIX_HOME + 以 release 布局启动主程序」的
  实机复现（见提交说明）。

## Reintroduction conditions

若将来把 `restrict_owner_only` 换成 Win32 API（`SetNamedSecurityInfoW`）直接改 ACL，
就不再有外部命令继承 stdout 的问题；此时第 1 条可撤，第 2 条（协议容错）必须长期
保留 —— 它是 `--stdio` 通道的边界防护，与谁污染 stdout 无关。
