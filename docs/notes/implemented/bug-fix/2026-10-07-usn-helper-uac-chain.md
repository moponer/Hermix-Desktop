# Decision Note: 本机索引 helper「没弹 UAC / 扫描半天搜不到」修复链

Status: implemented — 修复位于 `commands_local_search.rs`（spawn_elevated / locate_helper / 退位握手 / set_main_hwnd）与 `lib.rs`（HWND 记入 + 自动索引时机）；幽灵进程与陈旧二进制已于 2026-10-07 现场清理。

## Decision

用户现场症状：应用声称首次运行需要授权，但从不弹 UAC；后台扫描半天；搜索无结果。取证（交换目录 `%TEMP%\hermix-usn-index`）定位为四层缺陷叠加，逐一修复：

1. **`ShellExecuteW` 返回值误判**：失败时返回 `SE_ERR_*`（≤32 的正数），旧代码的 `is_invalid()` 只能抓 -1 → UAC 被拒/发起失败被当成功 → 傻等 `done.json` 300s 超时才判 `HELPER_FAILED`。修复：按 `code > 0 && code <= 32` 判定失败并给出具体错误文案（如"访问被拒绝（UAC 被拒绝或被策略禁止）"）。
2. **UAC 无前台宿主**：`start_background_index()` 在 setup 里裸跑，主窗口 `visible:false`，UAC 弹窗落在后台用户看不见。修复：主窗口 HWND 经 `set_main_hwnd()` 记入 `OnceLock`，`spawn_elevated` 以它为 owner 弹 UAC；自动索引挪到窗口 show（4s 兜底定时器内）之后发起。`SW_HIDE` 保持不变——它只作用于 helper 控制台，与 UAC 可见性无关。
3. **dev 期陈旧 helper 陷阱**：`locate_helper` 原为 first-exists，命中 `src-tauri/target/debug` 下 9-30 冻结的旧二进制（CARGO_TARGET_DIR 已改道 `release/target`），旧逻辑 bug 全部复发。修复：候选按 **mtime 取最新**，并补 `release/target/{debug,release}` 两个候选路径。
4. **幽灵 helper 永生**：旧版心跳判 `main.alive` 存在性而非新鲜度（10-05 笔记已记录但盘上仍是旧版），残留文件使旧提权进程永不退出（现场 `updates.jsonl` 膨胀至 315MB 仍在写）。修复：拉新 helper 前做**退位握手**——发 `shutdown.request`、等 `helper.pid` 消失（8s 上限）、清陈旧身份文件（pid/alive），杜绝双提权进程并发写。

## 现场清理（2026-10-07）

- 幽灵 helper（PID 2784，9-30 旧版）不认识 10-04 加的 `shutdown.request` 协议，强杀被 ACCESS_DENIED（非提权杀提权）；按其唯一自然出口处理：删 `main.alive` → idle 计满后自行退出（已确认退出）。
- 删除陈旧 `src-tauri/target/debug/usn-indexer.exe`（9-30 版），重编译后该路径落新 helper（10-07 10:23），mtime 择新规则下必被选中。

## Alternatives considered

- **强杀提权进程**（`taskkill /F`）：非提权进程无权杀提权进程（已实测 ACCESS_DENIED）；任务管理器手工杀则要求用户操作，不如 idle 自杀零接触。
- **helper 内嵌 manifest 自动提权 + 应用整体 requireAdministrator**：一劳永逸解决 UAC 时机，但改变整个应用的启动体验（每次开应用都弹 UAC），影响面远大于本 bug。
- **放弃提权 helper、全走 jwalk**：无授权依赖但全盘分钟级（正是本次"扫描半天"的兜底路径本身），性能倒退不可接受。
- **版本号嵌入 helper 文件名做兼容选择**：增加构建脚本复杂度；mtime 择新在 dev 场景等价且零构建改动。注意其局限：若未来出现"故意部署旧 helper"的需求需回改。

## Consequences

- UAC 失败现在会**立即**以明确文案失败（不再 300s 假成功），查找面板的"重新授权"按钮成为有效重试路径。
- 退位握手引入最长 8s 的启动延迟（仅当残留 pid 存在时）；正常路径零开销。
- mtime 择新使"两个 target 目录并存"从 bug 变为无害；但若用户手工 touch 旧 exe 可再触发选择错误——概率极低，接受。
- 验证：`cargo check`（src-tauri 子工作区）通过；`cargo test --lib` 19 passed；现场幽灵已退、新 helper 已编译。UAC 弹窗实际表现需下次应用启动人工确认。
