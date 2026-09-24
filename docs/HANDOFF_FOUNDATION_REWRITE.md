# 交接文档：Hermix 底座重写 + 网络安全层（给后续开发 agent）

> 更新时间：2026-09-24。本文是唯一权威交接入口。动手前**必须完整读完本文 + 必读文档清单**，禁止跳读后自行发挥。
> 当前累计 15 个 commit，工作区除 untracked 外干净。最近 commit：`b23670d0`。

---

## 0. 必读文档（按此顺序读，缺一不可）

| 顺序 | 文档 | 作用 |
|---|---|---|
| 1 | `docs/FOUNDATION_REWRITE_DESIGN.md` | **总设计 v2**（权威）。§1 边界（重写/重构/砍掉/保留）、§3 底座架构、§5 网络安全层、§7 执行阶段 P0-P8、§11 Non-goals |
| 2 | `docs/SEAMS_DESIGN.md` | C3 接缝设计（主人已拍板 S1-S5）。§2 八个接缝签名、§3 五个关键决策 |
| 3 | `docs/STABLE_FOUNDATION_DESIGN.md` | v1 设计（对照保留）。§1 症状→根因诊断、六条不变量仍有效 |
| 4 | `AGENTS.md`（仓库根） | 构建/验证命令、项目约定、六步决策阶梯、核心原则 |
| 5 | `.workbuddy/memory/2026-09-24.md` | 本轮全部批次的实施细节与踩坑记录 |
| 6 | `git log` 最近 15 个 commit message | 每个 commit 正文含完整改动清单与测试证据 |

**参考项目**：https://github.com/earendil-works/pi （TS monorepo，只对齐架构语义，禁止逐行移植）

---

## 1. 主人已拍板的决策（不可重新讨论）

| # | 决策 | 结论 |
|---|---|---|
| D1 | 重写范围 | C→B→A 递进：先砍死代码，再重写底座，agent loop 重构放最后 |
| D2 | PI 对齐方式 | **架构模式/不变量/状态机/协议契约对齐，Rust 原生实现**。逐行翻译 TS = 自毁，当场叫停 |
| D3 | 网络暴露面 | 默认关闭（127.0.0.1），用户显式开启 |
| D4 | 传输加密 | TLS 1.3 (rustls) + 自签 + 指纹固定（TOFU） |
| D5 | 远程设备默认权限 | 会话级：可发消息看会话，高危工具强制桌面确认 |
| D6 | 手机端 App | **不在本期**。本期只交付服务端安全基础设施 |
| S1-S5 | 接缝拍板 | "按你建议的全来"，已落地，见 SEAMS_DESIGN.md |

---

## 2. 已完成（15 个 commit，全部已验证）

### 阶段一：底座四件套 + P1 接缝化（8 个 commit）

| commit | 内容 |
|---|---|
| `61d3347f` | hermix-store 单一 SQLite 入口，metrics 迁移 |
| `491c5f6d` | focus 接 WAL，session 写入走 hermix-store |
| `000cc565` | 变更线 + commit 协议骨架 |
| `5888869d` | hermix-session SQLite sink，持久写前分配 seq |
| `abc6f4a1` | C1 hermix-context 可逆服务注册 |
| `3d6c69b6` | C3 接缝落地（S1-S5） |
| `87a15643` | C4 boot 装配层 |
| `346695ed` | Boot 接入主进程 |
| `4629c318` | P1-1 read_file/write_file → Fs seam |
| `a0e257f8` | P1-2 terminal 失败输出接 Shell seam |
| `6f103ba5` | P1-4 read_image S4 结构化输出 |

### 阶段二：网络安全五层（§5.5，4 个 commit）

| commit | 内容 |
|---|---|
| `2cf701a1` | 第 5 层核心：`hermix-core::origin` 权限矩阵（Origin::{Local, PairedDevice, Unpaired} × Capability 六分类 → Decision 三值） |
| `18dc6763` | HTTP 中间件 IP→Origin 推导 → axum Extension 透传 → terminal 工具 `execute_for_origin` |
| `863955f0` | Origin 装进 ToolCallContext，agent loop 调度路径接通（`execute_with_context` 读 `ctx.origin`） |
| `ff2cf281` | 第 2 层：TLS 1.3 监听器（rustls + ring provider）+ rcgen 自签证书生命周期（`<hermix_home>/tls/` 落盘复用，指纹 = SHA-256(cert DER)，`HERMIX_TLS_ADDR` 显式开启） |
| `b23670d0` | 第 3/4 层：设备配对（PairingSessions 一次性 TTL 120s + DeviceRegistry `<hermix_home>/devices.db` token 只存 SHA-256）+ Unpaired HTTP 关口 + 4 个配对端点 |

### 安全五层当前状态

| 层 | 状态 |
|---|---|
| 1 暴露面（默认 loopback） | ✅ |
| 2 TLS 1.3 + 指纹 | ✅（真机握手验证过） |
| 3 配对会话 | ✅ 后端（真机全流程验证过） |
| 4 凭证 + 注册表 | ✅ token 版（请求签名 T4 未做，device_pubkey/scopes 列已留） |
| 5 来源感知权限 | ✅ 两条路径全通（HTTP + agent loop） |

### 测试基线（改动后必须维持或超过）

```
hermix-tools lib: 691 passed（terminal 31）
hermix-agent lib: 1049 passed
hermix-server lib: 166 passed（pairing 8 / security 8 / tls 4）
hermix-core lib: 168+ passed
全仓 cargo check --workspace --all-targets 绿
```

验证命令（必须双绿才算完成）：
```bash
CARGO_INCREMENTAL=0 cargo check --workspace --all-targets
CARGO_INCREMENTAL=0 cargo test -p <crate> --lib   # 改了哪个 crate 测哪个，串行跑
```

---

## 3. 未完成任务（按优先级）

### 3.1 下一批（进行中被中断）：桌面端设备管理 UI + 二维码展示

- 上一个 agent 在探索 `apps/desktop-tauri/src/components/SystemConfigPanel.tsx` 的 tab 注册结构时被中断，**未写任何代码**。
- 需求：Settings 里加「设备管理」tab——展示二维码（含 pairing.rs 返回的 QR 负载）、已配对设备列表、撤销按钮。调用 §2 已落地的 4 个端点（`POST /api/pairing/sessions`、`GET/DELETE /api/pairing/devices`）。
- 前端栈：React 19 + Tailwind 4 + Vite；UI 走 token（禁 slate/blue/indigo/violet/purple 色系）；组件放 `src/components/settings/`，参考 DiagnosticsTab 模式。
- **注意**：前端目前没有 qrcode 依赖，需要选型（YAGNI 优先：能一行 SVG 生成就不引大库）。

### 3.2 P0 砍除与收敛（设计 §7，净减 ≥3 万行）

砍 `gateway`（27451 行）/ `acp`（2276）/ stubs 大部分 / consciousness / researcher；调度三套合一；传输两套合一。验收：`cargo check --workspace` 绿 + 测试全绿 + 净减 ≥3 万行。**全是死代码，可立刻做**。

### 3.3 请求签名（T4，安全加固批）

device token 泄漏 ≠ 无限使用。`paired_devices` 表已留 `device_pubkey`/`scopes` 列（当前存占位 `'[]'`），签名批次启用。防重放 + scopes 细粒度。

### 3.4 手机客户端

**D6 已拍板不在本期**，另立项目。本期交付的协议契约：QR 负载结构、4 个配对端点、TLS 指纹 TOFU，见 `pairing.rs` 与本文档。

### 3.5 设计文档 §7 其余阶段（P2 会话内核后续 / P3 任务运行时 / P4 协议层 / P5 服务端重写 / P7 前端 seq 对账 / P8 soak）

按设计文档执行，每期独立可交付可回退。**冻结在 P0-P8，新增项必须替换其中一项（F11）**。

---

## 4. 铁律（违反 = 返工）

1. **验证双绿**：`cargo check --workspace --all-targets` + 对应 crate `cargo test` 全过，才能报完成。前端改动加 `pnpm tsc --noEmit`。
2. **cargo 测试一律串行**：并行跑两个 cargo test 会撞 target 文件锁（os error 5）。
3. **注释只写功能/技术根因**，禁日期/人物/changelog 体。
4. **一语义一 commit**，精确暂存（禁 `git add -A`），commit message 含改动清单 + 测试证据。
5. **工具层接缝纪律**：新 origin-aware 工具实现 `ContextAware::execute_with_context` 读 `ctx.origin` 即可，禁止在 trait impl 外再加旁路；`ToolHandler::execute` 默认语义 = Local，不许改。
6. **Unpaired 关口双层纵深**：HTTP 层拦截（security.rs）+ 工具层 `Origin::may` 兜底，改任何一层都要保另一层完整。
7. **token 安全**：设备 token 只存 SHA-256（`pairing.rs` 已实现），任何新代码禁止明文落库/落日志。
8. **零 config schema 变更**：网络开关走环境变量（`HERMIX_SERVER_ADDR` / `HERMIX_TLS_ADDR`），模式已定型。
9. **前端 UI**：走设计 token，禁蓝紫色系，浅色简约呼吸感，Lucide 风格内嵌 SVG 图标。
10. **六步决策阶梯**（AGENTS.md）：YAGNI → 标准库 → 原生平台 → 现有依赖 → 单行 → 最小实现。不可削减项：信任边界校验、防数据丢失错误处理、安全措施。

## 5. 已知坑（前人踩过，别再踩）

- axum 0.8：trait impl 里不能放 inherent 方法；handler 多分支返回类型必须统一 `.into_response()`；改 Request 要 `into_parts()` → 装扩展 → `from_parts()` 装回。
- hermix-store 闭包返回裸 `T` 不是 `Result`（用 `.is_ok()` / `.flatten()` 处理）。
- rustls 必须显式 ring provider（`builder_with_provider(ring::default_provider())`），aws-lc-sys 在 MSVC 要 CMake/NASM + 多 provider 运行时 panic。
- axum-server 0.7.2+ TLS 内置但 module 在 feature `tls-rustls-no-provider` 后面；`from_tcp_rustls` 保住 SO_REUSEADDR。
- rcgen 0.13 SAN 类型叫 `SanType`；std `UdpSocket::connect` 返回 `Result<()>` 不是 socket。
- Git Bash 里 `curl -d` 传中文会 GBK 乱码，冒烟测试用 ASCII。
- Windows spawn 控制台子进程加 `.creation_flags(0x08000000)` 防黑窗。

## 6. 明确不做（Non-goals，设计 §11 + §1）

- ❌ 逐行移植 PI 的 TS 代码
- ❌ 抄 Chord（Hermix 无跨进程分片需求）
- ❌ 手机端 App（本期）
- ❌ 前端大改（seq 对账 ≤300 行、0 组件改动）
- ❌ 多平台分发 / 交叉编译（Windows MSVC only）

## 7. 工作区遗留物（接手前处理）

- `paired_smoke.json`（仓库根）：冒烟测试残留，**含一次性明文 device_token，直接删除**。
- untracked 文件（`WorkbenchChat.tsx`、`DiagnosticsTab.tsx`、`design-previews/`、三个 docs、两个 scripts）：历史批次产物，归属其他工作线，**不要动、不要顺手提交**。
