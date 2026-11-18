# 前端等待点补充清单（hermix-desktop）

范围：`apps/desktop-tauri/src` 下的前端代码（TypeScript / React）。本件只写前端侧等待点，不含 Rust crates。
覆盖判定基准：主清单 `docs/wait-timeout-audit.md` 的编号条目 1–18；主清单已写明的条目写「已覆盖，条目 N」，主清单未写明的新发现写「新增」。
时长口径：源码字面量，单位统一写毫秒（ms）或秒（s）；代码里没有上限的写「无上限」。
分级口径：**卡死型** = 等待期间界面无反馈，或到点前不收敛；**只是慢** = 有明确上限且期间有可见反馈。
本轮未启动应用、未跑测试、未跑构建，所有数值来自源码字面量。

## 一、等待点条目

（回源中，本版先落盘结构与已核对部分）

### 1. REST 单请求默认客户端死线

- 位置：`apps/desktop-tauri/src/api/rest.ts:77`（常量定义）、`apps/desktop-tauri/src/api/rest.ts:111`（`AbortSignal.timeout`）、`apps/desktop-tauri/src/api/rest.ts:142`（`Promise.race`）
- 等待机制：请求超时（客户端死线）
- 上限数值：`REQUEST_TIMEOUT_MS = 10_000` → 10s
- 触发条件：任何经 `apiInvoke` / `fetchJSON` 发出的、调用方未显式传 `timeoutMs` 的请求。
- 界面表现：**只是慢**（有明确上限；超时后 `Promise.race` 抛出 `request timed out after 10000ms`，调用方进入既有错误分支）
- 主清单覆盖：待与 `docs/wait-timeout-audit.md` 对照后填写。
- 源码旁证：

```ts
const REQUEST_TIMEOUT_MS = 10_000;
...
  const timeoutSignal = AbortSignal.timeout(timeoutMs);
  const combined = signal
    ? AbortSignal.any([signal, timeoutSignal])
    : timeoutSignal;
```

### 2. 新建会话同步请求的放宽死线

- 位置：`apps/desktop-tauri/src/api/rest.ts:409`（`createSession`）、`apps/desktop-tauri/src/api/rest.ts:422`（`{ timeoutMs: 30_000 }`）
- 等待机制：请求超时
- 上限数值：30_000 → 30s（覆盖默认 10s）
- 触发条件：用户新建会话（POST `/v1/sessions`）；后端正忙于长轮次或全盘扫描时该请求排队。
- 界面表现：**只是慢**（有明确上限 30s，超时抛错并落到错误卡片）
- 主清单覆盖：待与 `docs/wait-timeout-audit.md` 对照后填写。
- 源码旁证：

```ts
  const res = await fetchJSON<SessionSummary>(
    "/v1/sessions",
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ title, project }),
    },
    { timeoutMs: 30_000 },
  );
```

### 3. 首屏等待后端就绪的轮询预算

- 位置：`apps/desktop-tauri/src/api/rest.ts:1419`（`waitForBackendReady`）、`apps/desktop-tauri/src/api/rest.ts:1422`（`timeoutMs`）、`apps/desktop-tauri/src/api/rest.ts:1423`（`intervalMs`）
- 等待机制：轮询间隔 + 总预算
- 上限数值：总预算 `90_000` → 90s；轮询间隔 `1_000` → 1s
- 触发条件：首屏 hydration 请求（`getAvailableTools` / `getSessionMessages`）发出前等待后端 `/health` 应答；dev / 冷启动时前端 effect 早于 Rust 侧 `stdio_bridge::init_global`。
- 界面表现：**只是慢**（有明确上限 90s；期间连接指示灯显示离线，用户不盲）
- 主清单覆盖：待与 `docs/wait-timeout-audit.md` 对照后填写。
- 源码旁证：

```ts
export async function waitForBackendReady(
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<boolean> {
  const timeoutMs = opts.timeoutMs ?? 90_000;
  const intervalMs = opts.intervalMs ?? 1_000;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await checkBackendHealth()) return true;
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  return false;
}
```
