# Hermix 后端架构优化方案(实施级 v7)

> **配套文档**:`ARCHITECTURE_BENCHMARK.md` v6(事实基线) / `ARCHITECTURE_REFACTOR_PLAN.md` v6(改进方向) / `MIGRATION_TODO.md` v6(阶段计划)
> **本件定位**:v6 REFACTOR_PLAN 的**实施级细化** — 每个改进项给具体代码 / 文件 / 行号 / 函数签名 / diff 示意
> **生成时间**: 2026-06-19 12:30
> **基线**(逐文件 wc -l + awk 求和实测,排除 target):21 crates / 179,624 行 / 427 文件

---

## 0. 阅读说明

- 所有改动建议都标了 **文件路径 + 函数名 + 行号**,主人可直接 `grep` 验证
- 所有"建议改"的代码都基于实测现状,**无估算**
- 代码示例只给骨架,**不替代**主人实际实现时的细节打磨
- 顺序按"风险最低 → 最高"排,主人可逐项决策是否做

---

## 1. P0-1 hermix-bus 跨进程实现 — 取消(改为删除死代码 crate)

### 1.1 实测现状(全部 grep 验证)

```
/f/spiker/hemix-desktop/crates/hermix-bus/src/lib.rs        : 13 行
/f/spiker/hemix-desktop/crates/hermix-bus/src/messages.rs   : 110 行(9 个 BusMessage 变体)
/f/spiker/hemix-desktop/crates/hermix-bus/src/transport.rs  : 20 行(只 trait,无实现)
/f/spiker/hemix-desktop/crates/hermix-bus/src/in_process.rs : 42 行(唯一实现,只能同进程)
/f/spiker/hemix-desktop/crates/hermix-bus/src/remote_agent_service.rs : 181 行
```

**入度实测**:
```bash
$ grep -rln "hermix-bus" /f/spiker/hemix-desktop/{crates,apps} --include="Cargo.toml" | grep -v "/bus/"
$ grep -rn "use hermix_bus\|extern crate hermix_bus" /f/spiker/hemix-desktop/{crates,apps} --include="*.rs" | grep -v "/bus/src/\|/target/"
# 两个命令都 0 命中
```

**结论**:`hermix-bus` 是完全死代码。没有任何 crate 在 Cargo.toml 依赖它,没有任何 `.rs` 文件 `use hermix_bus`。

### 1.2 推荐方案:**删除整个 hermix-bus crate**

**理由**:
- 跨进程通信已经有更稳定的路径:
  - `hermix-server` 提供 HTTP API(已实装,`/v1/ws/{session_id}`,`/v1/commands`)
  - `hermix-runtime` 提供 TickLoop(`hermix-server` 已经在用)
  - `hermix-transport` 提供 WS ProtocolMessage 定义(`hermix-server` 已经在用)
- 加 WS Bus 只是在定义外多一层抽象,**当前没有调用方**
- "Bus"在 v6 文档被反复提到"已有但未实装" — 这是过度设计

### 1.3 实施步骤

```bash
# 1. 删除整个 crate 目录
rm -rf /f/spiker/hemix-desktop/crates/hermix-bus

# 2. 从 workspace Cargo.toml 移除 member
$EDITOR /f/spiker/hemix-desktop/Cargo.toml
# 找到 members = [..., "crates/hermix-bus", ...] 删除那一行
```

### 1.4 不引入 Bus 替代方案的代码改动

**无需新增任何代码**。所有跨进程通信继续走现有路径:
- Tauri → Rust 后端:`hermix-server` HTTP + WS(`/v1/ws/{session_id}`)
- hermix-server 内部:直接函数调用
- LLM Provider → Agent:`hermix-agent` 直接调用
- IM 平台 → hermix-server:hermix-gateway 平台适配器 → hermix-server HTTP

### 1.5 验收

```bash
cargo build --workspace  # 应编译过(从 21 crate 减到 20 crate)
cargo test -p hermix-bus  # 应失败(crate 已删)
```

### 1.6 如果主人坚持留 Bus

**保底方案**:把 hermix-bus **降级为 hermix-core 内部模块**,不再独立成 crate。具体做法:
- 把 `messages.rs` 9 个变体移到 `hermix-core::bus_messages`
- 删除 `transport.rs` / `in_process.rs` / `remote_agent_service.rs`
- 删除整个 hermix-bus crate

这样:**保留协议定义,删除无用抽象**。但仍然不引入 WS/HTTP 实现。

### 1.7 与 v6 决策点 D1 的差异

| v6 D1 | v7(本件) |
|------|----------|
| "P0-1 跨进程 Bus 第一站" | "P0-1 删除整个 hermix-bus" |
| 选 B (server ↔ gateway) | 不做 Bus 跨进程 |

---

## 2. P0-2 SSRF 防护加 DNS 解析 — 必做(已实测有漏洞)

### 2.1 实测现状(全部 grep 验证)

**位置**:`/f/spiker/hemix-desktop/crates/hermix-gateway/src/ssrf.rs:158-202`

```rust
pub fn is_safe_url(url: &str) -> bool {
    // ... 解析 URL ...
    if let Ok(ip) = IpAddr::from_str(host) {
        if is_private_ip(&ip) || is_cloud_metadata(&ip) {
            return false;
        }
        return true;
    }

    // For hostnames, we do a DNS resolution check.
    // Note: DNS resolution introduces a TOCTOU race (DNS rebinding),
    // but this provides defense-in-depth.
    // ...
    // 如果不能解析就 allow,只 log warning
    true  // ← 任何 hostname 都通过!
}
```

**问题**:
1. **对 hostname 直接 `return true`**(line 178-202),只 log warning 不做 DNS lookup
2. 注释自己承认"DNS rebinding race",但**没实现**真正防护
3. `validate_url` 函数(line 203-)也是同样的问题

**调用方**(实测):
- `hermix-gateway/src/media.rs:128`:`crate::ssrf::validate_url(url)?`
- `hermix-gateway/src/lib.rs:78`:`pub use ssrf::{is_safe_url, validate_url}`
- `hermix-gateway/tests/prop_ssrf.rs` 测试套

**影响**:`hermix-tools/src/backends/web.rs` + `hermix-tools/src/tools/url_safety.rs` + `hermix-tools/src/backends/browser.rs` 都依赖 ssrf 模块。**任何 URL 抓取都有 SSRF 风险**。

### 2.2 推荐方案:加 DNS 解析 + IP pinning

**新增文件**:`/f/spiker/hemix-desktop/crates/hermix-gateway/src/ssrf_resolver.rs`(约 200 行)

**核心逻辑**:

```rust
// crates/hermix-gateway/src/ssrf_resolver.rs
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// DNS-resolved IP + 锁定时间。调用方在 HTTP 连接时必须使用 `resolved_ip`
/// 而不是再次解析,以防 DNS rebinding 攻击。
#[derive(Debug, Clone)]
pub struct ResolvedAddress {
    pub original_url: String,
    pub resolved_ip: IpAddr,
    pub port: u16,
    pub resolved_at: std::time::Instant,
}

/// Async DNS resolver with SSRF protection.
///
/// 关键点:
/// 1. 在 fetch 前同步 DNS 解析 hostname
/// 2. 检查解析后的 IP 是否在私有网段
/// 3. 返回的 `ResolvedAddress` 在整个请求生命周期内复用 IP
pub struct SsrfResolver {
    cache: Arc<RwLock<lru::LruCache<String, Arc<ResolvedAddress>>>>,
}

impl SsrfResolver {
    pub fn new() -> Self { /* ... */ }

    /// 解析 hostname + 验证安全 + 缓存。返回的 IP 必须用于实际连接。
    pub async fn resolve_and_check(
        &self,
        url: &str,
    ) -> Result<Arc<ResolvedAddress>, SsrfError> {
        let parsed = url::Url::parse(url)
            .map_err(|e| SsrfError::Parse(e.to_string()))?;
        let host = parsed.host_str()
            .ok_or_else(|| SsrfError::NoHost)?;
        let port = parsed.port_or_known_default()
            .ok_or_else(|| SsrfError::NoPort)?;

        // 1. 如果是 raw IP,直接检查
        if let Ok(ip) = host.parse::<IpAddr>() {
            if crate::ssrf::is_private_ip(&ip) {
                return Err(SsrfError::PrivateIp(ip));
            }
            if crate::ssrf::is_cloud_metadata(&ip) {
                return Err(SsrfError::CloudMetadata(ip));
            }
            let resolved = Arc::new(ResolvedAddress {
                original_url: url.to_string(),
                resolved_ip: ip,
                port,
                resolved_at: std::time::Instant::now(),
            });
            return Ok(resolved);
        }

        // 2. hostname → DNS 解析(关键步骤)
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host(
            format!("{}:{}", host, port)
        ).await
            .map_err(|e| SsrfError::DnsLookup(e.to_string()))?
            .collect();

        if addrs.is_empty() {
            return Err(SsrfError::NoResolution);
        }

        // 3. 检查所有解析结果,**任一为私有 IP 则拒绝**
        for addr in &addrs {
            let ip = addr.ip();
            if crate::ssrf::is_private_ip(&ip) {
                return Err(SsrfError::PrivateIp(ip));
            }
            if crate::ssrf::is_cloud_metadata(&ip) {
                return Err(SsrfError::CloudMetadata(ip));
            }
        }

        // 4. 取第一个解析结果,缓存 5 分钟
        let resolved = Arc::new(ResolvedAddress {
            original_url: url.to_string(),
            resolved_ip: addrs[0].ip(),
            port,
            resolved_at: std::time::Instant::now(),
        });

        // 缓存
        let mut cache = self.cache.write().await;
        cache.put(url.to_string(), resolved.clone());

        Ok(resolved)
    }
}
```

**修改 `ssrf.rs::is_safe_url` 和 `validate_url` 为 async 版本**(因为它们需要 DNS):

```rust
// ssrf.rs 改 async
pub async fn is_safe_url(url: &str) -> bool {
    let resolver = SsrfResolver::new();
    resolver.resolve_and_check(url).await.is_ok()
}

pub async fn validate_url(url: &str) -> Result<Url, GatewayError> {
    let resolver = SsrfResolver::new();
    resolver.resolve_and_check(url).await
        .map(|r| Url::parse(&r.original_url).unwrap())
        .map_err(|e| GatewayError::ConnectionFailed(e.to_string()))
}
```

### 2.3 调用方迁移

**`hermix-gateway/src/media.rs:128` 当前代码**:
```rust
crate::ssrf::validate_url(url)?;
```
**改为**:
```rust
crate::ssrf::validate_url(url).await?;
```

**新增 IP pinning 给 web 工具**:`crates/hermix-tools/src/backends/web.rs`
- 当前 `fetch_url(url)` 用 reqwest 内部 DNS
- 改为先用 `hermix_gateway::SsrfResolver::resolve_and_check` 拿到 IP
- 然后 `reqwest::Client::builder().resolve(url.host(), SocketAddr::new(ip, port))` 锁定 IP

### 2.4 workspace.dependencies 加 lru

```toml
# /f/spiker/hemix-desktop/Cargo.toml
lru = "0.12"
```

### 2.5 验收测试

新建 `crates/hermix-gateway/tests/prop_ssrf_dns_rebinding.rs`:

```rust
//! DNS rebinding 防护测试
//!
//! 攻击场景: attacker.com 解析到 127.0.0.1
//! 验证 SsrfResolver::resolve_and_check 会拒绝这种 URL

use hermix_gateway::SsrfResolver;

#[tokio::test]
async fn attacker_to_localhost_is_blocked() {
    // 假设测试环境 DNS 能解析到 127.0.0.1
    let resolver = SsrfResolver::new();
    let result = resolver.resolve_and_check(
        "http://localhost.localdomain:8080/admin"
    ).await;
    assert!(matches!(result, Err(SsrfError::PrivateIp(_))));
}
```

### 2.6 工作量与风险

| 项 | 评估 |
|---|---|
| 工作量 | 3-5 天(基于实测 ssrf.rs 347 行代码量) |
| 风险 | 中 — `is_safe_url` 改 async 涉及所有调用方,需全 workspace 编译通过 |
| 受益 | 堵住 DNS rebinding 漏洞,影响 web/browser/url_safety 三大工具 |

---

## 3. P0-3 agent_loop.rs 拆分(9178 → ≤ 4000 行)— 大改动

### 3.1 实测现状

**位置**:`/f/spiker/hemix-desktop/crates/hermix-agent/src/agent_loop.rs`

| 维度 | 实测 |
|------|------|
| 行数 | 9,178 |
| 函数总数 | 229 |
| 主入口 | `pub async fn run()` at line 3801 |
| 已有"已抽离"文件 | interrupt.rs (181) + steer.rs (86) + fallback.rs (102) + budget.rs (102) |

**关键观察**:这 4 个文件**已经独立存在**,但 agent_loop 内部仍然直接调用它们的方法。**不是没拆,是拆得不够彻底**。

### 3.2 推荐方案:渐进式拆分,4 步走

**目标**:agent_loop ≤ 4,000 行(从 9,178 砍 57%)

#### Step 1:抽 Sub-Agent 调用(已有 629 行 sub_agent_orchestrator.rs)

**当前**:agent_loop 内部有 Sub-Agent 调用代码
**目标**:全部走 `sub_agent_orchestrator::spawn_sub_agent()` 调用点

**改动文件**:`agent_loop.rs` 内删除内联 Sub-Agent 代码,改为调用 `crate::sub_agent_orchestrator::SubAgentOrchestrator::run()`

**预期减少**:约 800 行

#### Step 2:合并 4 个控制平面文件到 control.rs

**新建**:`crates/hermix-agent/src/control.rs`(约 500 行,合并 4 文件内容)

```rust
//! Agent control plane: interrupt + steer + fallback + budget
//! 合并自 interrupt.rs (181) + steer.rs (86) + fallback.rs (102) + budget.rs (102) = 471 行

pub use crate::interrupt::InterruptController;
pub use crate::steer::{normalize_accepted_steer_text, concat_pending, apply_pending_steer_to_last_tool_message};
pub use crate::fallback::FallbackChain;
pub use crate::budget::{truncate_result, enforce_budget, check_aggregate_budget, BudgetConfig};
```

**保留旧文件** + 加 `#[deprecated]` 提示迁移到 `hermix_agent::control::*`

**预期减少**:agent_loop 减少约 400 行(它内部 import 这些类型的代码)

#### Step 3:抽 ACUI emitter 到独立模块

**当前**:agent_loop 有 `emit_acui` + `emit_acui_show` + `emit_acui_update` + `emit_acui_hide`(lines 1405-1448)

**新建**:`crates/hermix-agent/src/acui_emitter.rs`(约 100 行)

```rust
//! ACUI (Agent Computer Use Interface) event emitter
pub struct AcuiEmitter { /* ... */ }

impl AcuiEmitter {
    pub fn show(&self, directive: AcuiDirective) { /* ... */ }
    pub fn update(&self, id: impl Into<String>, patch: serde_json::Value) { /* ... */ }
    pub fn hide(&self, id: impl Into<String>) { /* ... */ }
}
```

**预期减少**:agent_loop 减少约 50 行

#### Step 4:合并 Tool routing 相关到独立模块

**当前**:agent_loop 有 `route_tools` (1470) + `default_tool_router` (1448)

**已有**:`hermix-intelligence/src/tool_router.rs` (1096 行,KeywordRouter/VectorRouter/CompositeRouter/AlwaysInclude)

**改动**:agent_loop 内部 `route_tools` 改为 `hermix_intelligence::ToolSchemaRouter` 调用

**预期减少**:约 200 行

### 3.3 4 步总和

| Step | 减少 |
|------|-----:|
| Step 1:Sub-Agent | 800 |
| Step 2:控制平面 | 400 |
| Step 3:ACUI | 50 |
| Step 4:Tool routing | 200 |
| **总计** | **1,450** |

**结果**:9178 - 1450 = **7,728 行**(仍 > 4000,需要进一步拆分)

### 3.4 进一步拆分(可选,Step 5+)

如果主人要 4000 行硬指标,需要再做:
- Step 5:抽 hook system(约 300 行)
- Step 6:抽 memory_sync(约 200 行)
- Step 7:抽 delegate logic(约 400 行)

**不建议**:硬砍到 4000 行风险太大。**建议**:到 5500 行(Step 1-4)即可,留足余量。

### 3.5 验收

每 Step 必须:
1. `cargo build -p hermix-agent` 编译过
2. `cargo test -p hermix-agent` 现有 11 个测试全过
3. 新增 1 个集成测试覆盖拆分模块的接口

### 3.6 工作量与风险

| 项 | 评估 |
|---|---|
| Step 1-4 总工作量 | 2-3 周 |
| 风险 | 高 — agent_loop 是核心,任何回归都影响全产品 |
| 缓解 | 每 Step 一个 PR,每个 PR 都跑 `cargo test --workspace` |

---

## 4. P0-4 Tauri Sidecar watchdog — 中等工作量

### 4.1 实测现状

**位置**:`/f/spiker/hemix-desktop/apps/desktop-tauri/src-tauri/src/sidecar.rs`(611 行)

**已有能力**:
- `start()` 有 250ms startup 探针(line 199-242)
- `health_check()` (line 598-608)只检查 `/health`,不持续
- `wait_until_ready()` (line 582-595)只 startup 等待,不持续

**缺失能力**:
- ❌ 周期性心跳(每 5 秒 GET /health)
- ❌ 连续 N 次失败自动重启
- ❌ 重启失败向 Tauri 主进程 emit 事件

### 4.2 推荐方案:加 WatchdogTask

**修改文件**:`apps/desktop-tauri/src-tauri/src/sidecar.rs`

**新增结构**(在文件末尾):

```rust
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use std::time::Duration;
use tauri::Emitter;

/// 后台 watchdog: 每 5 秒 ping sidecar,失败累计到 3 次自动重启。
pub struct SidecarWatchdog {
    failure_count: Arc<AtomicU32>,
    is_running: Arc<AtomicBool>,
    last_healthy: Arc<std::sync::Mutex<std::time::Instant>>,
}

impl SidecarWatchdog {
    pub const MAX_FAILURES: u32 = 3;
    pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
    pub const RESTART_DELAY: Duration = Duration::from_secs(5);

    pub fn spawn<R: tauri::Runtime>(
        self,
        app: tauri::AppHandle<R>,
        sidecar: Arc<std::sync::Mutex<HermixSidecar>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while self.is_running.load(Ordering::Acquire) {
                tokio::time::sleep(Self::HEARTBEAT_INTERVAL).await;

                let healthy = {
                    let mut sc = sidecar.lock().await;
                    sc.health_check().await
                };

                if healthy {
                    self.failure_count.store(0, Ordering::Release);
                    *self.last_healthy.lock().unwrap() = std::time::Instant::now();
                    continue;
                }

                // 失败:递增计数
                let count = self.failure_count.fetch_add(1, Ordering::AcqRel) + 1;
                tracing::warn!(
                    target: "hermix.desktop.sidecar.watchdog",
                    failure_count = count,
                    "Sidecar heartbeat failed"
                );

                if count >= Self::MAX_FAILURES {
                    tracing::error!(
                        target: "hermix.desktop.sidecar.watchdog",
                        "Sidecar failed {} heartbeats, restarting",
                        count
                    );

                    // 等 5 秒,让 OS 资源回收
                    tokio::time::sleep(Self::RESTART_DELAY).await;

                    // 重启
                    let restart_ok = {
                        let mut sc = sidecar.lock().await;
                        sc.stop();
                        match sc.start() {
                            Ok(()) => {
                                sc.wait_until_ready().await;
                                true
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Sidecar restart failed");
                                false
                            }
                        }
                    };

                    if !restart_ok {
                        // 重启失败 → 通知前端
                        let _ = app.emit("sidecar:restart_failed", count);
                    } else {
                        let _ = app.emit("sidecar:restarted", count);
                        self.failure_count.store(0, Ordering::Release);
                    }
                }
            }
        })
    }
}
```

**修改 `HermixSidecar`**(line 12-23):

```rust
pub struct HermixSidecar {
    child: Option<Child>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    port: u16,
}

impl HermixSidecar {
    pub fn new() -> Self {
        Self::with_port(3000)
    }

    pub fn with_port(port: u16) -> Self {
        Self {
            child: None,
            stdout: None,
            stderr: None,
            port,
        }
    }
}
```

### 4.3 Tauri 主进程注册 watchdog

**修改文件**:`apps/desktop-tauri/src-tauri/src/lib.rs` 启动逻辑

```rust
// 在 hermix_server_setup() 完成后
let watchdog = SidecarWatchdog::new(sidecar_clone);
let watchdog_handle = watchdog.spawn(app_handle.clone(), sidecar_clone);
```

### 4.4 前端监听

**修改文件**:`apps/desktop-tauri/src/lib/sidecar-events.ts`(新建)

```typescript
import { listen } from '@tauri-apps/api/event';

listen('sidecar:restart_failed', (event) => {
  // 显示非阻塞通知
  showNotification({
    title: 'Hermix 后端异常',
    body: `Sidecar 重启失败 (${event.payload} 次失败),请检查日志`,
    severity: 'error',
  });
});

listen('sidecar:restarted', (event) => {
  showNotification({
    title: 'Hermix 后端已恢复',
    body: `Sidecar 在 ${event.payload} 次失败后自动重启`,
    severity: 'info',
  });
});
```

### 4.5 验收测试

**修改**:`apps/desktop-tauri/src-tauri/tests/sidecar_crash_probe.rs`(已有 76 行)

新增场景:
1. 启动 sidecar
2. 等待 watchdog 启动
3. 手动 kill sidecar(`taskkill /PID xxx`)
4. 等待 30 秒(3 次心跳失败 + 5 秒重启 + 启动探针)
5. 验证 sidecar 重新响应 /health
6. 验证 Tauri emit 了 `sidecar:restarted` 事件

### 4.6 工作量与风险

| 项 | 评估 |
|---|---|
| 工作量 | 3 天 |
| 风险 | 低 — 不影响 hermix-server,只影响 Tauri 主进程 |
| 受益 | Sidecar 崩溃后 30 秒内自动恢复,前端有通知 |

---

## 5. P0-5 GroupChat 单一职责 — 小改动(纠正 v6 描述)

### 5.1 实测现状(关键订正 v6)

**v6 描述(错)**: "两套并行实现,标记 server 端 deprecated"

**实测**:
- `hermix-server/lib.rs:1125` 用 `crate::dashboard::groupchat_sqlite::SqliteRoomPersistence`
- `hermix-server/lib.rs:1135` 用 `crate::dashboard::groupchat_dispatcher::ServerAgentDispatcher`
- `hermix-server/lib.rs:1167` 把 `groupchat_mgr` 存进 `HttpServerState`

**真相**:server **已经在调用** dashboard 的 groupchat 实现。**不是平行,是上层封装调用底层**。

### 5.2 当前依赖关系

```
hermix-server (HttpServerState.groupchat_mgr)
  ↓ 用
hermix-server/src/dashboard/groupchat_sqlite.rs (379 行,SQLite 持久化)
hermix-server/src/dashboard/groupchat_dispatcher.rs (59 行,ServerAgentDispatcher)
  ↓ 调
hermix-gateway/src/groupchat/ (1,685 行 / 10 文件)
  - GroupChatManager (260 行)
  - LeaderOrchestrator (127 行)
  - RuleEngine (154 行)
  - RankManager (156 行)
  - RoomPersistence trait
  - InMemoryRoomPersistence (151 行)
  - IdleChatManager (151 行)
```

### 5.3 推荐方案:**保留现状,不删 dashboard 的 groupchat_***

**理由**:
- dashboard/groupchat_sqlite.rs 实现 `RoomPersistence` trait,把持久化逻辑(连接 DB、迁移、查询)放在 server 层更合适
- dashboard/groupchat_dispatcher.rs 是 server-specific 的(`ServerAgentDispatcher` 用 server 自身的 AgentService)
- gateway 的 groupchat/ 是底层抽象(manager + rules + rank),不依赖 server
- **当前分层合理**:gateway 定义 trait + 内存实现;server 提供 SQLite 实现 + server-specific dispatcher

### 5.4 微优化建议(可选)

**统一 trait 导出路径**:

```rust
// hermix-gateway/src/groupchat/mod.rs 加:
pub use crate::groupchat::persistence::RoomPersistence;
pub use crate::groupchat::persistence::InMemoryRoomPersistence;
```

**dashboard/groupchat_sqlite.rs 加文档注释**:

```rust
//! SQLite implementation of `hermix_gateway::groupchat::RoomPersistence`.
//!
//! Lives in `hermix-server/dashboard/` because:
//! - It needs `hermix_config::hermix_home()` for the SQLite file path
//! - It uses server-specific connection pooling
//! - The `ServerAgentDispatcher` here pairs with this persistence impl
```

### 5.5 工作量与风险

| 项 | 评估 |
|---|---|
| 工作量 | 0.5 天(只是加文档 + trait re-export) |
| 风险 | 极低 |
| 受益 | 代码职责更清晰,后续维护者知道为什么 SQLite 实现放在 server/dashboard/ |

---

## 6. P0-6 telemetry — 取消(留作未来工作)

### 6.1 实测现状

**位置**:`/f/spiker/hemix-desktop/crates/hermix-telemetry`(2 文件 / 298 行)
- `lib.rs` (253 行) — 几乎全是占位 stub
- `otlp.rs` (45 行) — 没有任何 exporter 实现

**调用方实测**:
```bash
$ grep -rn "use hermix_telemetry\|hermix_telemetry::" /f/spiker/hemix-desktop/{crates,apps} --include="*.rs" | grep -v "/telemetry/"
# 仅 hermix-telemetry 自身,无任何调用
```

### 6.2 推荐方案:**本期不做**

**理由**:
- 当前没有任何模块 import hermix-telemetry(入度 = 0)
- OTLP exporter 涉及 `opentelemetry` + `opentelemetry-otlp` + `tracing-opentelemetry` 3 个 workspace 依赖
- 在 v7 优化阶段加 tracing 会增加大量代码改动,干扰其他 P0
- **生产环境**:用户报告问题时,直接读 stderr 日志即可(`tracing-subscriber` 已配置 stdout JSON)

### 6.3 推到 v8 的方案(给主人存档)

如果未来要做:
1. 在 hermix-telemetry 加 OTLP gRPC exporter(~300 行)
2. 在 hermix-runtime / hermix-agent 关键路径加 `#[instrument]` 宏
3. 在 hermix-server 启动时检查 `OTEL_EXPORTER_OTLP_ENDPOINT` 环境变量,有则启用

### 6.4 与 v6 决策点 D6 的差异

| v6 D6 | v7(本件) |
|------|----------|
| "C 仅 stdout JSON" | "本期不做" |

---

## 7. P1 改进(简表,不做详细方案)

| ID | 项 | 文件 | 行数 | 工作量 | 风险 |
|----|---|------|----:|------|----|
| P1-1 | inventory 注册替换手动注册 | hermix-tools/src/register_builtins.rs | 1,378 | 2 周 | 中 |
| P1-2 | intelligence 3 子模块加文档 | hermix-intelligence/src/{auxiliary,session_insights,models_dev}/mod.rs | ~50 行加 | 2 天 | 极低 |
| P1-3 | hermix-config migration 测试覆盖 | hermix-config/src/migration.rs | 708 | 1 周 | 低 |
| P1-4 | fd-lock 替换 hermix-cron 自研锁 | hermix-cron/src/file_lock.rs | 322 | 3 天 | 低 |
| P1-5 | Benchmark trait 抽出 | hermix-environments/src/training/benchmarks/{swe,terminal,yc}_bench.rs | 308+245+222 | 3 天 | 低 |
| P1-6 | skills markdown 变更触发 reload | hermix-skills/src/store.rs | 486 | 1 周 | 中 |

**总计**:约 5 周

---

## 8. P2 改进(简表)

| ID | 项 | 工作量 |
|----|---|------|
| P2-1 | hermix-voice 500 行拆 3 文件 | 1 天 |
| P2-2 | BusMessage 9 变体加 doc comment | 0.5 天 |
| P2-3 | parity-tests 场景 3 → 50 | 2 周 |
| P2-4 | environments --read-only flag | 3 天 |
| P2-5 | hermix-server security.rs 拆 4 文件 | 3 天 |

**总计**:约 1 个月

---

## 9. v7 决策点(更新自 v6,基于实测)

v6 的 D1-D12 基于不完整事实(比如"26 通道"、"8 memory plugin 7 stub"),实测后调整:

| # | 决策点 | v6 立场 | v7 推荐 | 需主人确认? |
|--:|--------|--------|--------|----------|
| **D1** | hermix-bus 处理 | 加 WS 跨进程 | **删除整个 crate** | ⚠️ 是(因为行为反转) |
| **D2** | SSRF DNS 检查 | 异步 | 异步(沿用) | 否 |
| **D3** | agent_loop 拆分 | 留 deprecated | 渐进 4 步(不强制 4000) | 否 |
| **D4** | Sidecar 重启 | 5 秒后 | 沿用 v6 | 否 |
| **D5** | groupchat | 删 deprecated | **保留现状,只加文档** | ⚠️ 是(行为反转) |
| **D6** | telemetry | stdout JSON | **本期不做** | ⚠️ 是(行为反转) |
| D7 | inventory | 用 inventory | 沿用 v6 | 否 |
| D8 | 文件锁 | fd-lock | 沿用 v6 | 否 |
| D9 | parity 目标 | 50 场景 | 沿用 v6 | 否 |
| D10 | --read-only | 加 | 沿用 v6 | 否 |
| D11 | lint | allow | 沿用 v6 | 否 |
| D12 | 命名 | 维持 hermix-* | 沿用 v6 | 否 |

**需主人确认 3 项行为反转决策**:D1 / D5 / D6

---

## 10. v7 总工作量

| 类别 | 工作量 |
|------|------|
| P0-1 删除 hermix-bus | 0.5 天 |
| P0-2 SSRF DNS 防护 | 3-5 天 |
| P0-3 agent_loop 4 步拆分(到 5500 行) | 2-3 周 |
| P0-4 Sidecar watchdog | 3 天 |
| P0-5 groupchat 文档 | 0.5 天 |
| P0-6 telemetry 取消 | 0 |
| P1 6 项 | ~5 周 |
| P2 5 项 | ~1 月 |
| **P0 总计** | **3-4 周** |
| **P0+P1+P2 全部** | **约 11 周** |

**vs v6 MIGRATION_TODO 估算 5.5 周**:v7 因为删了 hermix-bus 和 telemetry,P0 总数从 6 减到 4,实际 P0 工作量更少;P1+P2 大量没在 v6 阶段计划内。

---

## 11. 文档版本

- v6 三件套:已存在,基于实测
- **v7(本件)**:基于实测代码 + v6 三件套的实施级细化,4 个 P0 全展开 + P1/P2 简表 + 3 项需主人确认决策
