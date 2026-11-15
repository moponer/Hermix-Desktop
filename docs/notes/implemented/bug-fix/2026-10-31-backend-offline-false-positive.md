# Decision Note: 桌面端「后端未连接」误报（健康探测假离线）

Status: implemented — 2026-10-31 落地

## Decision

`useBackendConnection` 的离线判定从「online 态单次探测失败立即转 offline」改为三重收敛（apps/desktop-tauri/src/hooks/useBackendConnection.ts）：

1. **online→offline 需连续 2 次探测失败**（`OFFLINE_FAILURE_THRESHOLD = 2`，探测间隔 6s）。`backend_ready` 的 `/health` 往返有 2s 预算，而审批选中后（终端进程创建 + 杀软实时扫描）、工具执行中（大输出、子进程 spawn）恰是后端最忙的时刻，单次超时是常态而非故障。
2. **冷启动宽限 30s → 90s**（`BOOT_GRACE_MS`），与 `RESTART_PROBE_BUDGET_MS` 和 watchdog 的冷启动校准（~70s）对齐。此前慢机器上后端还在正常启动就被判离线。
3. **新增 `hermix:stream-bridge-dead` 事件监听**（带代次守卫 `generation === current_generation`）：stdout 读线程退出 = 子进程真死，立即判离线，抵消阈值带来的 ~6s 检测延迟。旧代读线程的退出事件忽略，防止把刚重启好的后端误判成离线。

## Alternatives considered

- **加大探测预算**（2s → 5s+）：真死检测等比变慢，且重负载尖峰可以远超任何固定预算，治标不治本。
- **Rust 侧 `backend_ready` 返回「bridge live && child alive」就不算失败**：会把「进程活着但服务挂死」的状态永远报在线——正是这套探测当初要消灭的「绿灯但发不出消息」。
- **复用 `hermix:server-restarting`**：那是 watchdog 的结论（5s tick + 6 次失败才重启），比 bridge-dead 事件慢且不含代次信息。

## Consequences

- 真离线检测延迟从 ~6s 变为 ~12s（两次探测），有 bridge-dead 事件兜底的路径仍即时。
- 误报离线的连锁伤害消除：此前用户点「重启」会杀掉正在干活的健康会话。
- 探测预算（2s）与轮询参数未动；若目标机重负载下连续两次超时仍可能闪离线，届时优先怀疑真实卡死而不是继续放宽阈值。
