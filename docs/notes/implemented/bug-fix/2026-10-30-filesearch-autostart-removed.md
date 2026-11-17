# Decision Note: 关闭主程序后的残留进程——拆掉 filesearch 开机自启，helper 改为纯按需

Status: implemented — 2026-10-30 移除 `HermixFileSearch` 计划任务的注册（改为主动清理）、去掉启动预热、看门狗 10s→1s、退出时有界等待 helper 真的退掉

关联：[退出清理收口到 shutdown.rs](2026-10-27-exit-cleanup-and-backend-offline-visibility.md)、[安装器收进程](../bug-fix/2026-10-29-installer-locked-exe-viewer.md)、[filesearch 单飞锁](2026-10-25-filesearch-daemon-silent-single-flight.md)、[集成 FilePulse 文件搜索](../feature/2026-10-14-integrate-filepulse-file-search.md)

## Decision

`hermix-filesearch.exe` 是**提权常驻进程**（MFT/USN 需要管理员，见 2026-10-14 笔记 §5），宿主 `TerminateProcess` 会被「拒绝访问」挡回。此前为了让索引"开机即就绪"，它被注册成 `AtLogOn + RunLevel Highest` 的计划任务——这条链是"关掉主程序仍有残留、下次启动/安装失败"的根源。四处改动：

1. **不再注册计划任务**：`crates/hermix-filesearch/src/bin/daemon.rs` 的 `install_autostart_task` 删除，换成 `remove_autostart_task`（`Unregister-ScheduledTask -TaskName HermixFileSearch`）。旧的协议名 `installTask` / `--install-task` **保留但语义反转**：任何旧宿主（已发布版本）发来的注册请求，都会变成"清理这条任务"——旧客户端越多，清理越彻底。
2. **helper 纯按需拉起**：`crates/hermix-server/src/filesearch.rs` 的 `ensure_helper` 仍是唯一入口（首次搜索时 `ShellExecuteExW("runas")` 弹一次 UAC，已有单飞锁 + 60s 失败冷却防连环弹）。`install_autostart_once` → `remove_autostart_once`。
3. **取消启动预热**：`crates/hermix-server/src/lib.rs` 里"启动 5s 后 `ensure_helper()`"整块删掉。取消自启后若仍预热，就变成"每次启动主程序弹一次 UAC"，比首次搜索更烦。改为 `purge_legacy_autostart()`——只有当旧的提权 helper 恰好在跑（登录任务拉起的）时，才让它删掉自己名下的任务；不拉起任何进程。
4. **残留窗口收紧**：daemon 的孤儿看门狗轮询 10s → **1s**（`daemon.rs`，代价是每秒一次 `stat`）；宿主 `shutdown::cleanup` 写完 `stop-request.json` 后**有界等待 2s**（`shutdown.rs::wait_filesearch_gone`，用回环端口判活——提权进程连句柄都拿不到，进程一退端口即释放）。

## 为什么必须拆（现场证据）

用户报告"每次关闭主程序总有残留进程，导致下次启动或安装失败"。实测这台机器上的计划任务：

```
Task To Run   : <workspace>/hermix-desktop/target/debug/hermix-filesearch.exe --serve --data-dir ...
Schedule Type : At logon time        Run As: Alex        RunLevel: Highest
```

它指向 **debug 构建产物**。原因是注册用的是 `Register-ScheduledTask -Force`：**任何一次 dev 构建的 daemon 握手成功，都会把任务抢注到 `target/debug/`**。于是下次登录拉起 debug 版 → 它长期锁住 `target/debug/hermix-filesearch.exe` → `tauri-build` 拷 sidecar 时 `fs::remove_file(...).unwrap()` 报 `PermissionDenied`（`tauri-build-2.7.1/src/lib.rs:164`），安装器报"文件被占用"。

三层叠加，缺一层都不会这么难受：

| 层 | 现象 | 证据 |
|---|---|---|
| 点 ⓧ 只是隐藏（`close_to_tray` 默认 true） | "关掉了"但主进程、`hermix.exe`、`hermix-filesearch.exe` 全在 | `lib.rs:1049-1058`、`system_settings.rs:73-79` |
| 真退出也只"写个请求就走" | 宿主 `exit(0)` 立即返回，helper 最快 1s（原 10s）后才读到请求，刷盘最长 60s | `shutdown.rs:50-53`、`daemon.rs` 看门狗与刷盘看门狗 |
| 提权进程被注册成登录常驻 | 下次登录必起，用户永远清不掉；`-Force` 还会被 dev 构建抢注 | `daemon.rs` 原 `install_autostart_task` |

`hermix-viewer.exe` 不在此列：它是独立查看器，用户关掉主程序后继续看文件是正常用法（`shutdown.rs` 顶部注释），它自己的窗口关掉即退出。

## Alternatives considered

| 方案 | 否决理由 |
|---|---|
| 保留 `AtLogOn` 常驻 + 空闲自退（N 分钟无请求退出） | 残留最多存在 N 分钟——`N` 取小了等于取消常驻，取大了仍然卡住安装窗口；且"登录即起提权进程"这条性质没变，用户对"我什么都没开，任务管理器里有个管理员进程"的反应不会更好 |
| 计划任务保留但去掉 `AtLogOn` 触发器，主程序用 `schtasks /run` 按需拉起 | 能免 UAC，但**任务本身必须常驻存在**，而它占的正是"用户不可见的提权入口"这个位置；`-Force` 抢注问题依旧（dev 构建照样能把它指到 target/debug）。多一个需要维护的系统级状态，换来的只是省一次 UAC |
| 只修"退出时等待"，不动自启 | 等待只覆盖"用户主动退出"这一条路径。任务管理器结束、注销、关机、崩溃都不走 `cleanup`，而计划任务会在下次登录把 helper 重新拉起来——残留是**设计产生**的，不是漏杀 |
| 给 helper 加 `TerminateProcess` 兜底 | 提权进程，普通权限拒绝访问；宿主即使提权也不该去杀别的提权进程（安全边界） |
| 把 filesearch 并入主进程（不再拆 exe） | 主程序会被迫 `require<user>`，与"便携、双击即用"的产品前提冲突（2026-10-14 笔记 §5 的原始取舍） |

## Consequences

- **得到**：没有主程序在跑时，机器上不再有提权的 `hermix-filesearch.exe` 常驻。关闭主程序 → helper 在 1s 内收到停止请求并退出，宿主最多等 2s；退出后立刻重开主程序 / 跑安装器不再撞锁。dev 构建也不会再把计划任务抢注到 `target/debug/`（它连注册代码都没有了）。
- **代价**：
  - **首次搜索弹一次 UAC**（每个主程序会话至多一次，被拒后 60s 冷却）。这是用户明确选择的取舍：宁可点一次授权，也不要一个常驻的管理员进程。
  - **索引不再开机预热**：索引随第一次搜索才开始建/续增量。旧快照仍在（`indexes/` 按卷落盘），所以"打开就能搜到昨天的文件"这件事不变，慢的只是全新卷的首次全量扫描。
- **遗留 / 后续**：
  - 老用户机器上的 `HermixFileSearch` 任务需要**一次新版启动**才会被清掉：`purge_legacy_autostart` 只在"旧 helper 恰好在跑"时才发得动删除请求（删任务要管理员）。若旧 helper 没在跑、任务却还在，会等到用户第一次搜索时（`ensure_helper` 成功后同一条清理路径）才删。彻底一点可在安装器 `hooks.nsh` 的 PREINSTALL 里加一条 `schtasks /Delete /TN HermixFileSearch /F`（安装器本身提权，最稳）——本次未做。
  - 数据目录里还留着 2026-10-25 生成的 `autostart-task.xml`（旧版用 `schtasks /Create /XML` 的遗留），已无代码读写，可手工删除；不清理也无害。
  - `hermix-viewer.exe` 也会按需拉起 helper（`apps/viewer/src-tauri/src/filesearch.rs`），但 viewer 关闭时不写停止请求——主程序在的话由主程序退出时收；只开 viewer 的场景会留下 helper 直到下次交接。属于独立议题。
  - **门禁缺口**：没有任何检查能发现"某处又注册了开机自启 / 又加回了预热"。这类回退只能靠人读代码发现。
